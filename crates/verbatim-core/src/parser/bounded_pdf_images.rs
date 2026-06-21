use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, Stream};

use crate::image_limits::{
    ImageArtifactBudget, ImageArtifactLimitError, ImageArtifactLimitStage, ImageArtifactLimits,
};
use crate::types::ParsedImageArtifact;

const BACKEND: &str = "bounded-lopdf-xobject";

/// Extract Image XObjects with parser-side count, dimension, and byte checks.
///
/// This deliberately does not call backend APIs that return whole-page image
/// vectors or decoded image bytes. `lopdf::Document::load` necessarily reads
/// the PDF's already-compressed streams into memory; after that, every artifact
/// byte clone is preceded by count, dimension, per-image byte, and total-byte
/// budget checks. Filters that require decoding before a bounded standalone
/// artifact is available fail closed with a structured unsupported error.
pub(crate) fn extract_image_artifacts(
    path: &Path,
    limits: ImageArtifactLimits,
) -> Result<Vec<ParsedImageArtifact>> {
    let doc = Document::load(path).context("failed to open PDF for bounded image extraction")?;
    let mut budget = ImageArtifactBudget::new(limits, ImageArtifactLimitStage::Parser);
    let mut artifacts = Vec::new();

    for (page_num, page_id) in doc.get_pages() {
        let mut page_image_index = 0;
        let mut seen_resource_ids = HashSet::new();
        let (direct_resources, resource_ids) = doc
            .get_page_resources(page_id)
            .context("failed to read PDF page resources")?;

        if let Some(resources) = direct_resources {
            extract_from_resource_dict(
                &doc,
                page_num,
                resources,
                &mut page_image_index,
                &mut budget,
                &mut artifacts,
            )?;
        }

        for resource_id in resource_ids {
            if !seen_resource_ids.insert(resource_id) {
                continue;
            }
            let Ok(resources) = doc.get_dictionary(resource_id) else {
                continue;
            };
            extract_from_resource_dict(
                &doc,
                page_num,
                resources,
                &mut page_image_index,
                &mut budget,
                &mut artifacts,
            )?;
        }
    }

    Ok(artifacts)
}

fn extract_from_resource_dict(
    doc: &Document,
    page_num: u32,
    resources: &Dictionary,
    page_image_index: &mut u32,
    budget: &mut ImageArtifactBudget,
    artifacts: &mut Vec<ParsedImageArtifact>,
) -> Result<()> {
    let Ok(xobjects) = resources.get(b"XObject") else {
        return Ok(());
    };
    let Some(xobjects) = resolve_object(doc, xobjects).and_then(|obj| obj.as_dict().ok()) else {
        return Ok(());
    };

    for (_, entry) in xobjects.iter() {
        let Some(object) = resolve_object(doc, entry) else {
            continue;
        };
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        if stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name_str)
            .ok()
            != Some("Image")
        {
            continue;
        }

        *page_image_index = page_image_index.saturating_add(1);
        let image_index = *page_image_index;
        budget.reserve_image_slot(page_num, image_index)?;
        match extract_stream_artifact(page_num, image_index, stream, budget, artifacts) {
            Ok(()) => {}
            Err(err) => {
                if let Some(limit) = unsupported_image_extraction(&err) {
                    warn_unsupported_image_skip(limit);
                } else {
                    return Err(err);
                }
            }
        }
    }

    Ok(())
}

fn extract_stream_artifact(
    page_num: u32,
    image_index: u32,
    stream: &Stream,
    budget: &mut ImageArtifactBudget,
    artifacts: &mut Vec<ParsedImageArtifact>,
) -> Result<()> {
    let width = positive_u32(&stream.dict, b"Width").ok_or_else(|| {
        unsupported(
            page_num,
            image_index,
            "image XObject is missing positive Width metadata needed for pre-decode limits",
        )
    })?;
    let height = positive_u32(&stream.dict, b"Height").ok_or_else(|| {
        unsupported(
            page_num,
            image_index,
            "image XObject is missing positive Height metadata needed for pre-decode limits",
        )
    })?;
    budget.validate_dimensions(page_num, image_index, width, height)?;

    let filters = filter_names(&stream.dict, page_num, image_index)?;
    let (bytes, mime_type, extension) = match filters.as_slice() {
        [] => (
            encode_unfiltered_png(page_num, image_index, stream, width, height, budget)?,
            "image/png",
            "png",
        ),
        [filter] if filter == "DCTDecode" => {
            budget.accept_image_bytes(page_num, image_index, stream.content.len())?;
            (stream.content.clone(), "image/jpeg", "jpg")
        }
        _ => {
            return Err(unsupported(
                page_num,
                image_index,
                "image filter chain requires decode/re-encoding before bounded artifact bytes are available",
            )
            .into());
        }
    };

    artifacts.push(ParsedImageArtifact {
        page: page_num,
        image_index,
        bbox: None,
        bytes,
        mime_type: mime_type.into(),
        extension: extension.into(),
        width,
        height,
        nearby_text_before: None,
        nearby_text_after: None,
    });

    Ok(())
}

fn encode_unfiltered_png(
    page_num: u32,
    image_index: u32,
    stream: &Stream,
    width: u32,
    height: u32,
    budget: &mut ImageArtifactBudget,
) -> Result<Vec<u8>> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;

    let (components, color_type) = raw_png_color(&stream.dict, page_num, image_index)?;
    let expected_len = raw_sample_len(width, height, components).ok_or_else(|| {
        unsupported(
            page_num,
            image_index,
            "raw image dimensions overflow the parser-side byte bound calculation",
        )
    })?;
    if stream.content.len() != expected_len {
        return Err(unsupported(
            page_num,
            image_index,
            "raw image sample length does not match Width/Height/ColorSpace metadata",
        )
        .into());
    }

    let upper_bound = png_encoded_upper_bound(expected_len, height).ok_or_else(|| {
        unsupported(
            page_num,
            image_index,
            "PNG output bound overflows parser-side byte calculation",
        )
    })?;
    budget.validate_image_bytes(page_num, image_index, upper_bound)?;

    let mut bytes = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::NoFilter);
    encoder
        .write_image(&stream.content, width, height, color_type.into())
        .context("failed to encode bounded raw PDF image as PNG")?;

    if bytes.len() > upper_bound {
        return Err(unsupported(
            page_num,
            image_index,
            "PNG encoder exceeded the precomputed parser-side byte bound",
        )
        .into());
    }
    budget.accept_image_bytes(page_num, image_index, bytes.len())?;
    Ok(bytes)
}

fn raw_png_color(
    dict: &Dictionary,
    page: u32,
    image_index: u32,
) -> Result<(usize, image::ColorType)> {
    let bits_per_component = dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|bits| bits.as_i64().ok())
        .unwrap_or(8);
    if bits_per_component != 8 {
        return Err(unsupported(
            page,
            image_index,
            "raw image BitsPerComponent is not 8, so PNG output cannot be bounded simply",
        )
        .into());
    }

    let color_space = dict
        .get(b"ColorSpace")
        .ok()
        .and_then(|color_space| color_space.as_name_str().ok())
        .unwrap_or("DeviceRGB");
    match color_space {
        "DeviceGray" => Ok((1, image::ColorType::L8)),
        "DeviceRGB" => Ok((3, image::ColorType::Rgb8)),
        _ => Err(unsupported(
            page,
            image_index,
            "raw image ColorSpace is not DeviceGray or DeviceRGB",
        )
        .into()),
    }
}

fn raw_sample_len(width: u32, height: u32, components: usize) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(components)
}

fn png_encoded_upper_bound(raw_len: usize, height: u32) -> Option<usize> {
    let row_filter_bytes = usize::try_from(height).ok()?;
    let deflate_overhead = (raw_len / 16_383).saturating_add(1).checked_mul(5)?;
    raw_len
        .checked_add(row_filter_bytes)?
        .checked_add(deflate_overhead)?
        .checked_add(4_096)
}

fn resolve_object<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object.as_reference() {
        Ok(id) => doc.get_object(id).ok(),
        Err(_) => Some(object),
    }
}

fn positive_u32(dict: &Dictionary, key: &[u8]) -> Option<u32> {
    let value = dict.get(key).ok()?.as_i64().ok()?;
    if value <= 0 || value > i64::from(u32::MAX) {
        None
    } else {
        Some(value as u32)
    }
}

fn filter_names(dict: &Dictionary, page: u32, image_index: u32) -> Result<Vec<String>> {
    let Ok(filter) = dict.get(b"Filter") else {
        return Ok(Vec::new());
    };

    match filter {
        Object::Name(name) => Ok(vec![String::from_utf8_lossy(name).into_owned()]),
        Object::Array(filters) => filters
            .iter()
            .map(|filter| match filter {
                Object::Name(name) => Ok(String::from_utf8_lossy(name).into_owned()),
                _ => Err(unsupported(
                    page,
                    image_index,
                    "image Filter array contains a non-name entry",
                )
                .into()),
            })
            .collect(),
        _ => Err(unsupported(
            page,
            image_index,
            "image Filter metadata is not a name or name array",
        )
        .into()),
    }
}

fn unsupported(page: u32, image_index: u32, reason: &'static str) -> ImageArtifactLimitError {
    ImageArtifactLimitError::UnsupportedImageExtraction {
        stage: ImageArtifactLimitStage::Parser,
        backend: BACKEND,
        reason,
        page,
        image_index,
    }
}

fn unsupported_image_extraction(err: &anyhow::Error) -> Option<&ImageArtifactLimitError> {
    err.downcast_ref::<ImageArtifactLimitError>()
        .filter(|limit| limit.is_unsupported_extraction())
}

fn warn_unsupported_image_skip(limit: &ImageArtifactLimitError) {
    if let ImageArtifactLimitError::UnsupportedImageExtraction {
        stage,
        backend,
        reason,
        page,
        image_index,
    } = limit
    {
        tracing::warn!(
            stage = %stage,
            backend = *backend,
            reason = *reason,
            page = *page,
            image_index = *image_index,
            "skipping unsupported PDF image artifact"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::image_limits::ImageArtifactLimitError;

    #[test]
    fn extracts_small_image_xobject_without_backend_batch_extraction() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&pdf_path, image_stream_prefix(None), &[255u8; 8 * 8 * 3]);

        let artifacts = extract_image_artifacts(&pdf_path, ImageArtifactLimits::default())
            .expect("bounded image extraction should succeed");

        assert_eq!(artifacts.len(), 1);
        let artifact = &artifacts[0];
        assert_eq!(artifact.page, 1);
        assert_eq!(artifact.image_index, 1);
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(artifact.extension, "png");
        assert_eq!(artifact.width, 8);
        assert_eq!(artifact.height, 8);
        assert!(artifact.bytes.starts_with(&[137, 80, 78, 71]));
        assert!(artifact.bbox.is_none());
    }

    #[test]
    fn checks_image_count_before_stream_byte_budget() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&pdf_path, image_stream_prefix(None), &[255u8; 8 * 8 * 3]);
        let limits = ImageArtifactLimits {
            max_images_per_source: 0,
            max_bytes_per_image: 0,
            ..ImageArtifactLimits::default()
        };

        let err = extract_image_artifacts(&pdf_path, limits).unwrap_err();
        let limit = image_limit_error(&err);

        assert!(matches!(
            limit,
            ImageArtifactLimitError::TooManyImages {
                stage: ImageArtifactLimitStage::Parser,
                page: 1,
                image_index: 1,
                ..
            }
        ));
    }

    #[test]
    fn rejects_stream_bytes_before_retaining_artifact_bytes() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&pdf_path, image_stream_prefix(None), &[255u8; 8 * 8 * 3]);
        let limits = ImageArtifactLimits {
            max_bytes_per_image: 1,
            ..ImageArtifactLimits::default()
        };

        let err = extract_image_artifacts(&pdf_path, limits).unwrap_err();
        let limit = image_limit_error(&err);

        assert!(matches!(
            limit,
            ImageArtifactLimitError::ImageBytesExceeded {
                stage: ImageArtifactLimitStage::Parser,
                page: 1,
                image_index: 1,
                ..
            }
        ));
    }

    #[test]
    fn rejects_dimensions_before_stream_byte_budget() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&pdf_path, image_stream_prefix(None), &[255u8; 8 * 8 * 3]);
        let limits = ImageArtifactLimits {
            max_image_pixels: 1,
            max_bytes_per_image: 1,
            ..ImageArtifactLimits::default()
        };

        let err = extract_image_artifacts(&pdf_path, limits).unwrap_err();
        let limit = image_limit_error(&err);

        assert!(matches!(
            limit,
            ImageArtifactLimitError::ImageDimensionsExceeded {
                stage: ImageArtifactLimitStage::Parser,
                page: 1,
                image_index: 1,
                ..
            }
        ));
    }

    #[test]
    fn unsupported_filter_is_skipped_without_retaining_artifact_bytes() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("flate-image-fixture.pdf");
        write_pdf_with_image(
            &pdf_path,
            image_stream_prefix(Some("FlateDecode")),
            &[120, 156, 3, 0, 0, 0, 0, 1],
        );

        let artifacts = extract_image_artifacts(&pdf_path, ImageArtifactLimits::default())
            .expect("unsupported image extraction should be skipped");

        assert!(artifacts.is_empty());
    }

    fn image_limit_error(err: &anyhow::Error) -> &ImageArtifactLimitError {
        err.downcast_ref::<ImageArtifactLimitError>()
            .expect("error should preserve structured image limit type")
    }

    fn image_stream_prefix(filter: Option<&str>) -> Vec<u8> {
        let mut prefix =
            b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8"
                .to_vec();
        if let Some(filter) = filter {
            prefix.extend(format!(" /Filter /{filter}").as_bytes());
        }
        prefix
    }

    fn write_pdf_with_image(path: &Path, image_prefix: Vec<u8>, image_bytes: &[u8]) {
        let content = b"q\n36 0 0 36 72 72 cm\n/Im1 Do\nQ\n";
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
            stream_object(&image_prefix, image_bytes),
            stream_object(b"<<", content),
        ];
        std::fs::write(path, pdf_bytes(objects)).expect("fixture PDF should save");
    }

    fn stream_object(prefix: &[u8], data: &[u8]) -> Vec<u8> {
        let mut object = prefix.to_vec();
        object.extend(format!(" /Length {} >>\nstream\n", data.len()).as_bytes());
        object.extend(data);
        object.extend(b"\nendstream");
        object
    }

    fn pdf_bytes(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (idx, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n", idx + 1).as_bytes());
            pdf.extend(object);
            pdf.extend(b"\nendobj\n");
        }
        let xref_offset = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }
}
