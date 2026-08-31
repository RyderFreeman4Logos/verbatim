use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::parser::text_segments::pdf_page_evidence_units;
use crate::traits::Parser;
use crate::types::{DerivedConversionMetadata, EvidenceUnit, SourceId};

const CONVERTER: &str = "anydoc+pdf-inspector";
const CONVERTER_VERSION: &str = "anydoc@0.2.4;pdf-inspector@1.14.2";

/// Optional PDF adapter that rejects OCR and keeps PDF page anchors native.
pub struct AnyDocPdfParser;

impl Parser for AnyDocPdfParser {
    fn name(&self) -> &str {
        "anydoc_pdf"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["pdf"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        self.parse_with_derived_metadata(path)
            .map(|(units, _)| units)
    }

    fn parse_with_derived_metadata(
        &self,
        path: &Path,
    ) -> Result<(Vec<EvidenceUnit>, Option<DerivedConversionMetadata>)> {
        let bytes =
            fs::read(path).with_context(|| format!("failed to read: {}", path.display()))?;
        let markdown = anydoc::to_markdown_bytes(&bytes, anydoc::Format::Pdf)
            .map_err(|error| anyhow::anyhow!("anydoc PDF conversion failed: {error}"))?;
        if markdown.is_empty() {
            anyhow::bail!("anydoc PDF conversion produced no Markdown");
        }

        let items = pdf_inspector::extract_text_with_positions_pages(path, None)
            .context("failed to extract native PDF text with pdf-inspector")?;
        let mut pages = BTreeMap::<u32, String>::new();
        for item in items {
            let text = item.text.trim();
            if text.is_empty() {
                continue;
            }
            let page_text = pages.entry(item.page).or_default();
            if !page_text.is_empty() {
                page_text.push(' ');
            }
            page_text.push_str(text);
        }

        let source_id = SourceId::from_path(path);
        let mut position = 0;
        let mut units = Vec::new();
        for (page, text) in pages {
            units.extend(pdf_page_evidence_units(
                &source_id,
                page,
                &text,
                &mut position,
            ));
        }
        crate::pdf_selector::attach_pdf_selectors(
            &mut units,
            &crate::types::hex_sha256(&bytes),
            self.name(),
        );
        Ok((
            units,
            Some(DerivedConversionMetadata {
                converter: CONVERTER.to_string(),
                converter_version: CONVERTER_VERSION.to_string(),
                output_hash: crate::types::hex_sha256(markdown.as_bytes()),
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::parser::anydoc::AnyDocPdfParser;
    use crate::traits::Parser;
    use crate::types::{DerivedConversionMetadata, SourceLocator};

    #[test]
    fn adapter_preserves_original_source_identity_and_native_pdf_anchor() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("golden.pdf");
        write_pdf_with_text(&path, "Golden anydoc PDF evidence");
        let original = std::fs::read(&path).unwrap();

        let (units, conversion) = AnyDocPdfParser
            .parse_with_derived_metadata(&path)
            .expect("golden PDF should parse");

        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].source_id, crate::types::SourceId::from_path(&path));
        assert_eq!(units[0].text, "Golden anydoc PDF evidence");
        let selector = match &units[0].locator {
            SourceLocator::Pdf {
                page: 1,
                paragraph: 0,
                bbox: None,
                selector: Some(selector),
            } => selector,
            locator => panic!("expected native PDF selector, got {locator:?}"),
        };
        assert_eq!(selector.source_hash, crate::types::hex_sha256(&original));
        assert_eq!(selector.parser_profile_id, "anydoc_pdf");
        assert_eq!(
            conversion,
            Some(DerivedConversionMetadata {
                converter: "anydoc+pdf-inspector".into(),
                converter_version: "anydoc@0.2.4;pdf-inspector@1.14.2".into(),
                output_hash: "18f2f6e8bda2d1c75a1898299ed4087e4f8274e2f9746e4ed1c4da325ab88539"
                    .into(),
            })
        );
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
        std::fs::write(path, pdf_bytes(objects)).unwrap();
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
