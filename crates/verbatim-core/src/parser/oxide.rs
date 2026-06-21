use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::traits::Parser;
use crate::types::{EvidenceId, EvidenceUnit, SourceId, SourceLocator};

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
