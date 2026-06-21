use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::image_limits::ImageArtifactLimits;
use crate::traits::Parser;
use crate::types::{
    EvidenceId, EvidenceKind, EvidenceUnit, ParsedImageArtifact, SourceId, SourceLocator,
};

pub struct PdfOxideParser;

impl Parser for PdfOxideParser {
    fn name(&self) -> &str {
        "pdf_oxide"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["pdf"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let path_str = path.to_str().context("non-UTF8 path")?;
        let source_id = source_id_from_path(path);
        let doc = pdf_oxide::document::PdfDocument::open(path_str)
            .context("failed to open PDF with pdf_oxide")?;

        let num_pages = doc.page_count().context("failed to get page count")?;
        let mut units = Vec::new();
        let mut position: u32 = 0;

        for page_idx in 0..num_pages {
            let page_text = match doc.extract_text(page_idx) {
                Ok(t) => t,
                Err(_) => continue,
            };

            if page_text.trim().is_empty() {
                continue;
            }

            let page_num = page_idx as u32 + 1;
            let paragraphs = split_paragraphs(&page_text);
            for (para_idx, para_text) in paragraphs.iter().enumerate() {
                let trimmed = para_text.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let text_hash = hex_sha256(trimmed);
                units.push(EvidenceUnit {
                    id: EvidenceId(format!("{}:p{}:n{}", source_id.0, page_num, para_idx)),
                    source_id: source_id.clone(),
                    kind: EvidenceKind::Text,
                    locator: SourceLocator::Pdf {
                        page: page_num,
                        paragraph: para_idx as u32,
                        bbox: None,
                    },
                    text: trimmed.to_string(),
                    text_hash,
                    heading_path: Vec::new(),
                    position,
                });
                position += 1;
            }
        }

        Ok(units)
    }

    fn extract_image_artifacts_with_limits(
        &self,
        path: &Path,
        limits: ImageArtifactLimits,
    ) -> Result<Vec<ParsedImageArtifact>> {
        crate::parser::bounded_pdf_images::extract_image_artifacts(path, limits)
    }
}

fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|s| s.replace('\n', " ").trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn hex_sha256(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn source_id_from_path(path: &Path) -> SourceId {
    SourceId::from_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_limits::ImageArtifactLimitStage;

    #[test]
    fn extracts_image_artifacts_from_pdf_fixture() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&pdf_path);

        let artifacts = PdfOxideParser
            .extract_image_artifacts(&pdf_path)
            .expect("image extraction should succeed");

        assert_eq!(artifacts.len(), 1);
        let artifact = &artifacts[0];
        assert_eq!(artifact.page, 1);
        assert_eq!(artifact.image_index, 1);
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(artifact.extension, "png");
        assert_eq!(artifact.width, 8);
        assert_eq!(artifact.height, 8);
        assert!(artifact.bbox.is_none());
        assert!(artifact.bytes.starts_with(&[137, 80, 78, 71]));
    }

    #[test]
    fn rejects_image_artifacts_over_byte_limit() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&pdf_path);
        let limits = ImageArtifactLimits {
            max_bytes_per_image: 1,
            ..ImageArtifactLimits::default()
        };

        let err = PdfOxideParser
            .extract_image_artifacts_with_limits(&pdf_path, limits)
            .unwrap_err();

        let limit = err
            .downcast_ref::<crate::image_limits::ImageArtifactLimitError>()
            .expect("error should preserve structured limit type");
        assert!(matches!(
            limit,
            crate::image_limits::ImageArtifactLimitError::ImageBytesExceeded {
                stage: ImageArtifactLimitStage::Parser,
                page: 1,
                image_index: 1,
                ..
            }
        ));
    }

    #[test]
    fn rejects_image_artifacts_over_pixel_limit_before_byte_retention() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&pdf_path);
        let limits = ImageArtifactLimits {
            max_image_pixels: 1,
            ..ImageArtifactLimits::default()
        };

        let err = PdfOxideParser
            .extract_image_artifacts_with_limits(&pdf_path, limits)
            .unwrap_err();

        let limit = err
            .downcast_ref::<crate::image_limits::ImageArtifactLimitError>()
            .expect("error should preserve structured limit type");
        assert!(matches!(
            limit,
            crate::image_limits::ImageArtifactLimitError::ImageDimensionsExceeded {
                stage: ImageArtifactLimitStage::Parser,
                page: 1,
                image_index: 1,
                ..
            }
        ));
    }

    fn write_pdf_with_image(path: &Path) {
        let image_bytes = vec![255u8; 8 * 8 * 3];
        let content = b"q\n36 0 0 36 72 72 cm\n/Im1 Do\nQ\n";
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8",
                &image_bytes,
            ),
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
