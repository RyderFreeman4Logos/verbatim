use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::traits::Parser;
use crate::types::{EvidenceId, EvidenceUnit, SourceId, SourceLocator};

pub struct PlaintextParser;

impl Parser for PlaintextParser {
    fn name(&self) -> &str {
        "plaintext"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["txt", "text"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read: {}", path.display()))?;
        let path_str = path.to_string_lossy().to_string();
        let source_id = source_id_from_path(path);

        let mut units = Vec::new();
        let mut position: u32 = 0;
        let mut line_num: u32 = 1;
        let mut para_start_line: u32 = 1;
        let mut para_text = String::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                if !para_text.trim().is_empty() {
                    let trimmed = para_text.trim();
                    let end_line = line_num - 1;
                    units.push(EvidenceUnit {
                        id: EvidenceId(format!(
                            "{}:L{}:n{}",
                            source_id.0, para_start_line, position
                        )),
                        source_id: source_id.clone(),
                        locator: SourceLocator::Document {
                            path_or_url: path_str.clone(),
                            line_start: para_start_line,
                            line_end: if end_line > para_start_line {
                                Some(end_line)
                            } else {
                                None
                            },
                        },
                        text: trimmed.to_string(),
                        text_hash: hex_sha256(trimmed),
                        heading_path: Vec::new(),
                        position,
                    });
                    position += 1;
                }
                para_text.clear();
                para_start_line = line_num + 1;
            } else {
                if para_text.is_empty() {
                    para_start_line = line_num;
                }
                if !para_text.is_empty() {
                    para_text.push(' ');
                }
                para_text.push_str(line.trim());
            }
            line_num += 1;
        }

        if !para_text.trim().is_empty() {
            let trimmed = para_text.trim();
            let end_line = line_num - 1;
            units.push(EvidenceUnit {
                id: EvidenceId(format!(
                    "{}:L{}:n{}",
                    source_id.0, para_start_line, position
                )),
                source_id: source_id.clone(),
                locator: SourceLocator::Document {
                    path_or_url: path_str,
                    line_start: para_start_line,
                    line_end: if end_line > para_start_line {
                        Some(end_line)
                    } else {
                        None
                    },
                },
                text: trimmed.to_string(),
                text_hash: hex_sha256(trimmed),
                heading_path: Vec::new(),
                position,
            });
        }

        Ok(units)
    }
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
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn split_by_blank_lines() {
        let mut f = NamedTempFile::with_suffix(".txt").unwrap();
        write!(
            f,
            "First paragraph.\n\nSecond paragraph.\nContinued.\n\nThird.\n"
        )
        .unwrap();
        let parser = PlaintextParser;
        let units = parser.parse(f.path()).unwrap();
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].text, "First paragraph.");
        assert_eq!(units[1].text, "Second paragraph. Continued.");
        assert_eq!(units[2].text, "Third.");
    }

    #[test]
    fn line_numbers() {
        let mut f = NamedTempFile::with_suffix(".txt").unwrap();
        write!(f, "Line 1\nLine 2\n\nLine 4\n").unwrap();
        let parser = PlaintextParser;
        let units = parser.parse(f.path()).unwrap();
        assert_eq!(units.len(), 2);
        match &units[0].locator {
            SourceLocator::Document {
                line_start,
                line_end,
                ..
            } => {
                assert_eq!(*line_start, 1);
                assert_eq!(*line_end, Some(2));
            }
            _ => panic!("expected Document locator"),
        }
    }
}
