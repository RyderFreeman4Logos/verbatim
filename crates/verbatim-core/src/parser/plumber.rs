use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::traits::Parser;
use crate::types::{EvidenceId, EvidenceUnit, SourceId, SourceLocator};

pub struct PdfPlumberParser;

impl Parser for PdfPlumberParser {
    fn name(&self) -> &str {
        "pdfplumber"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["pdf"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let source_id = source_id_from_path(path);
        let pdf =
            pdfplumber::Pdf::open_file(path, None).context("failed to open PDF with pdfplumber")?;

        let mut units = Vec::new();
        let mut position: u32 = 0;

        for page_result in pdf.pages_iter() {
            let page = page_result.context("failed to read page")?;
            let page_num = page.page_number() as u32;
            let text = page.extract_text(&pdfplumber::TextOptions::default());

            if text.trim().is_empty() {
                continue;
            }

            let paragraphs = split_paragraphs(&text);
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
