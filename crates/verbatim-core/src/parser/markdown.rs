use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use pulldown_cmark::{Event, HeadingLevel, Parser as MdParser, Tag, TagEnd};
use sha2::{Digest, Sha256};

use crate::traits::Parser;
use crate::types::{EvidenceId, EvidenceUnit, SourceId, SourceLocator};

pub struct MarkdownParser;

impl Parser for MarkdownParser {
    fn name(&self) -> &str {
        "markdown"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["md", "markdown"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read: {}", path.display()))?;
        let path_str = path.to_string_lossy().to_string();
        let source_id = source_id_from_path(path);
        let lines: Vec<&str> = content.lines().collect();

        let parser = MdParser::new(&content);
        let mut units = Vec::new();
        let mut heading_stack: Vec<(HeadingLevel, String)> = Vec::new();
        let mut current_text = String::new();
        let mut block_start_line: usize = 0;
        let mut position: u32 = 0;
        let mut in_heading = false;
        let mut current_heading_level: Option<HeadingLevel> = None;
        let mut current_heading_text = String::new();
        let mut byte_offset: usize = 0;

        for (event, range) in parser.into_offset_iter() {
            byte_offset = range.start;
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    flush_block(
                        &mut current_text,
                        &path_str,
                        &source_id,
                        &heading_stack,
                        block_start_line,
                        &lines,
                        &content,
                        byte_offset,
                        &mut position,
                        &mut units,
                    );
                    in_heading = true;
                    current_heading_level = Some(level);
                    current_heading_text.clear();
                }
                Event::End(TagEnd::Heading(_)) => {
                    in_heading = false;
                    if let Some(level) = current_heading_level.take() {
                        while heading_stack.last().is_some_and(|(l, _)| *l >= level) {
                            heading_stack.pop();
                        }
                        heading_stack.push((level, current_heading_text.clone()));
                    }
                    block_start_line = byte_to_line(&content, byte_offset);
                }
                Event::Text(text) => {
                    if in_heading {
                        current_heading_text.push_str(&text);
                    } else {
                        if current_text.is_empty() {
                            block_start_line = byte_to_line(&content, range.start);
                        }
                        current_text.push_str(&text);
                    }
                }
                Event::SoftBreak | Event::HardBreak => {
                    if !in_heading {
                        current_text.push(' ');
                    }
                }
                Event::End(TagEnd::Paragraph | TagEnd::Item) => {
                    flush_block(
                        &mut current_text,
                        &path_str,
                        &source_id,
                        &heading_stack,
                        block_start_line,
                        &lines,
                        &content,
                        byte_offset,
                        &mut position,
                        &mut units,
                    );
                }
                Event::Code(code) if !in_heading => {
                    if current_text.is_empty() {
                        block_start_line = byte_to_line(&content, range.start);
                    }
                    current_text.push('`');
                    current_text.push_str(&code);
                    current_text.push('`');
                }
                _ => {}
            }
        }

        flush_block(
            &mut current_text,
            &path_str,
            &source_id,
            &heading_stack,
            block_start_line,
            &lines,
            &content,
            byte_offset,
            &mut position,
            &mut units,
        );

        Ok(units)
    }
}

#[allow(clippy::too_many_arguments)]
fn flush_block(
    text: &mut String,
    path_str: &str,
    source_id: &SourceId,
    heading_stack: &[(HeadingLevel, String)],
    start_line: usize,
    lines: &[&str],
    content: &str,
    end_byte: usize,
    position: &mut u32,
    units: &mut Vec<EvidenceUnit>,
) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        text.clear();
        return;
    }

    let end_line = byte_to_line(content, end_byte);
    let line_start = start_line as u32 + 1;
    let line_end = if end_line > start_line && end_line < lines.len() {
        Some(end_line as u32 + 1)
    } else {
        None
    };

    let text_hash = hex_sha256(trimmed);
    units.push(EvidenceUnit {
        id: EvidenceId(format!("{}:L{}:n{}", source_id.0, line_start, position)),
        source_id: source_id.clone(),
        locator: SourceLocator::Document {
            path_or_url: path_str.to_string(),
            line_start,
            line_end,
        },
        text: trimmed.to_string(),
        text_hash,
        heading_path: heading_stack.iter().map(|(_, t)| t.clone()).collect(),
        position: *position,
    });
    *position += 1;
    text.clear();
}

fn byte_to_line(content: &str, byte_offset: usize) -> usize {
    let clamped = byte_offset.min(content.len());
    content[..clamped].matches('\n').count()
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

    fn parse_str(md: &str) -> Vec<EvidenceUnit> {
        let mut f = NamedTempFile::with_suffix(".md").unwrap();
        f.write_all(md.as_bytes()).unwrap();
        let parser = MarkdownParser;
        parser.parse(f.path()).unwrap()
    }

    #[test]
    fn heading_hierarchy() {
        let units = parse_str("# H1\n\nPara under H1.\n\n## H2\n\nPara under H2.\n");
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].heading_path, vec!["H1"]);
        assert_eq!(units[1].heading_path, vec!["H1", "H2"]);
    }

    #[test]
    fn line_locators() {
        let units = parse_str("First paragraph.\n\nSecond paragraph.\n");
        assert_eq!(units.len(), 2);
        for u in &units {
            match &u.locator {
                SourceLocator::Document { line_start, .. } => {
                    assert!(*line_start >= 1);
                }
                _ => panic!("expected Document locator"),
            }
        }
    }
}
