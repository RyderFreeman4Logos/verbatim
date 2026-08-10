use std::path::Path;

use anyhow::{Context, Result};

use crate::image_limits::ImageArtifactLimits;
use crate::parser::text_segments::pdf_page_evidence_units;
use crate::traits::Parser;
use crate::types::{EvidenceUnit, ParsedImageArtifact, SourceId};

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
            units.extend(pdf_page_evidence_units(
                &source_id,
                page_num,
                &page_text,
                &mut position,
            ));
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

fn source_id_from_path(path: &Path) -> SourceId {
    SourceId::from_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::image_limits::ImageArtifactLimitStage;
    use crate::ingest::IngestPipeline;
    use crate::store::Store;
    use crate::types::SourceLocator;

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
    fn mvp_regression_pdf_text_fixture_extracts_page_locator() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("text-fixture.pdf");
        write_pdf_with_text(&pdf_path, "MVP PDF text evidence");

        let units = PdfOxideParser
            .parse(&pdf_path)
            .expect("text PDF fixture should parse");

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "MVP PDF text evidence");
        assert!(matches!(
            units[0].locator,
            SourceLocator::Pdf {
                page: 1,
                paragraph: 0,
                bbox: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn issue_288_relocation_preserves_ingested_selector_after_reload() {
        let tempdir = tempfile::tempdir().unwrap();
        let db_path = tempdir.path().join("verbatim.db");
        let mut config = Config::default();
        config.embedding.enabled = false;
        let mut pipeline = IngestPipeline::new(&config, tempdir.path()).unwrap();
        let old_path = tempdir.path().join("before.pdf");
        let new_path = tempdir.path().join("after.pdf");
        write_pdf_with_text(&old_path, "Relocated born digital evidence");
        let source_id = pipeline.add_source(&old_path).unwrap();

        pipeline.ingest_source(&source_id).await.unwrap();
        let before = pipeline
            .store()
            .list_evidence_by_source(&source_id)
            .unwrap();
        let selector = before
            .iter()
            .find_map(|unit| match &unit.locator {
                SourceLocator::Pdf {
                    selector: Some(selector),
                    ..
                } => Some(selector.clone()),
                _ => None,
            })
            .expect("ingest should attach a PDF selector");
        std::fs::rename(&old_path, &new_path).unwrap();

        pipeline.relocate_source(&source_id, &new_path).unwrap();
        drop(pipeline);

        let reopened = Store::new(&db_path).unwrap();
        let source = reopened.get_source(&source_id).unwrap().unwrap();
        assert_eq!(source.path, std::fs::canonicalize(&new_path).unwrap());
        assert_eq!(selector.source_hash, source.hash);
        assert_eq!(
            selector.parser_profile_id,
            source.parser_used.as_deref().unwrap()
        );
        let reloaded = reopened.list_evidence_by_source(&source_id).unwrap();
        assert!(reloaded.iter().any(|unit| matches!(
            &unit.locator,
            SourceLocator::Pdf {
                selector: Some(reloaded),
                ..
            } if reloaded == &selector
        )));
    }

    #[test]
    fn mvp_regression_pdf_diagram_image_fixture_extracts_artifact() {
        let tempdir = tempfile::tempdir().unwrap();
        let pdf_path = tempdir.path().join("diagram-fixture.pdf");
        write_pdf_with_image(&pdf_path);

        let artifacts = PdfOxideParser
            .extract_image_artifacts(&pdf_path)
            .expect("diagram image fixture should parse");

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].mime_type, "image/png");
        assert_eq!(artifacts[0].width, 8);
        assert_eq!(artifacts[0].height, 8);
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

    fn write_pdf_with_text(path: &Path, text: &str) {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let content = format!("BT\n/F1 12 Tf\n72 120 Td\n({escaped}) Tj\nET\n");
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            stream_object(b"<<", content.as_bytes()),
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
