use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use pulldown_cmark::{Event, HeadingLevel, Parser as MdParser, Tag, TagEnd};

use crate::traits::Parser;
use crate::types::{
    hex_sha256, EvidenceId, EvidenceKind, EvidenceUnit, MarkdownBlockKind, MarkdownHeadingLocator,
    SourceId, SourceLocator,
};

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
        let document = MarkdownDocumentContext {
            path_str: &path_str,
            source_id: &source_id,
            content: &content,
        };
        let parser = MdParser::new(&content);
        let mut units = Vec::new();
        let mut heading_stack: Vec<MarkdownHeadingLocator> = Vec::new();
        let mut slug_counts: HashMap<String, u32> = HashMap::new();
        let mut current_block: Option<MarkdownBlock> = None;
        let mut position: u32 = 0;
        let mut in_heading = false;
        let mut current_heading_level: Option<HeadingLevel> = None;
        let mut current_heading_text = String::new();
        let mut current_heading_line: u32 = 1;
        let mut list_item_depth = 0usize;
        let mut block_quote_depth = 0usize;
        let mut in_code_block = false;

        for (event, range) in parser.into_offset_iter() {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    flush_block(
                        &mut current_block,
                        &document,
                        &heading_stack,
                        range.start,
                        &mut position,
                        &mut units,
                    );
                    in_heading = true;
                    current_heading_level = Some(level);
                    current_heading_line = line_number(&content, range.start);
                    current_heading_text.clear();
                }
                Event::End(TagEnd::Heading(_)) => {
                    in_heading = false;
                    if let Some(level) = current_heading_level.take() {
                        let level = heading_level_number(level);
                        while heading_stack
                            .last()
                            .is_some_and(|heading| heading.level >= level)
                        {
                            heading_stack.pop();
                        }
                        let text = current_heading_text.trim().to_string();
                        let slug = unique_heading_slug(&text, &mut slug_counts);
                        heading_stack.push(MarkdownHeadingLocator {
                            level,
                            text,
                            slug,
                            line: current_heading_line,
                        });
                    }
                }
                Event::Start(Tag::Paragraph) if !in_heading && !in_code_block => {
                    start_block(
                        &mut current_block,
                        contextual_block_kind(list_item_depth, block_quote_depth, in_code_block),
                        range.start,
                    );
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    block_quote_depth += 1;
                    start_block(
                        &mut current_block,
                        MarkdownBlockKind::BlockQuote,
                        range.start,
                    );
                }
                Event::End(TagEnd::BlockQuote(_)) => {
                    if block_quote_depth == 1 {
                        flush_block(
                            &mut current_block,
                            &document,
                            &heading_stack,
                            range.end,
                            &mut position,
                            &mut units,
                        );
                    }
                    block_quote_depth = block_quote_depth.saturating_sub(1);
                }
                Event::Start(Tag::Item) => {
                    list_item_depth += 1;
                    start_block(&mut current_block, MarkdownBlockKind::ListItem, range.start);
                }
                Event::End(TagEnd::Item) => {
                    flush_block(
                        &mut current_block,
                        &document,
                        &heading_stack,
                        range.end,
                        &mut position,
                        &mut units,
                    );
                    list_item_depth = list_item_depth.saturating_sub(1);
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    flush_block(
                        &mut current_block,
                        &document,
                        &heading_stack,
                        range.start,
                        &mut position,
                        &mut units,
                    );
                    in_code_block = true;
                    start_block(
                        &mut current_block,
                        MarkdownBlockKind::CodeBlock,
                        range.start,
                    );
                }
                Event::End(TagEnd::CodeBlock) => {
                    flush_block(
                        &mut current_block,
                        &document,
                        &heading_stack,
                        range.end,
                        &mut position,
                        &mut units,
                    );
                    in_code_block = false;
                }
                Event::Text(text) => {
                    if in_heading {
                        current_heading_text.push_str(&text);
                    } else {
                        push_block_text(
                            &mut current_block,
                            contextual_block_kind(
                                list_item_depth,
                                block_quote_depth,
                                in_code_block,
                            ),
                            range.start,
                            range.end,
                            &text,
                        );
                    }
                }
                Event::SoftBreak | Event::HardBreak => {
                    if in_heading {
                        current_heading_text.push(' ');
                    } else {
                        push_block_separator(&mut current_block, in_code_block, range.end);
                    }
                }
                Event::End(TagEnd::Paragraph) => {
                    if list_item_depth == 0 && block_quote_depth == 0 && !in_code_block {
                        flush_block(
                            &mut current_block,
                            &document,
                            &heading_stack,
                            range.end,
                            &mut position,
                            &mut units,
                        );
                    } else {
                        push_block_separator(&mut current_block, in_code_block, range.end);
                    }
                }
                Event::Code(code) if !in_heading => {
                    push_block_text(
                        &mut current_block,
                        contextual_block_kind(list_item_depth, block_quote_depth, in_code_block),
                        range.start,
                        range.end,
                        &format!("`{code}`"),
                    );
                }
                Event::Code(code) => {
                    current_heading_text.push_str(&code);
                }
                _ => {}
            }
        }

        flush_block(
            &mut current_block,
            &document,
            &heading_stack,
            content.len(),
            &mut position,
            &mut units,
        );

        Ok(units)
    }
}

#[derive(Debug, Clone, Copy)]
struct MarkdownDocumentContext<'a> {
    path_str: &'a str,
    source_id: &'a SourceId,
    content: &'a str,
}

#[derive(Debug, Clone)]
struct MarkdownBlock {
    kind: MarkdownBlockKind,
    start_byte: usize,
    end_byte: usize,
    text: String,
}

impl MarkdownBlock {
    fn new(kind: MarkdownBlockKind, start_byte: usize) -> Self {
        Self {
            kind,
            start_byte,
            end_byte: start_byte,
            text: String::new(),
        }
    }
}

fn contextual_block_kind(
    list_item_depth: usize,
    block_quote_depth: usize,
    in_code_block: bool,
) -> MarkdownBlockKind {
    if in_code_block {
        MarkdownBlockKind::CodeBlock
    } else if list_item_depth > 0 {
        MarkdownBlockKind::ListItem
    } else if block_quote_depth > 0 {
        MarkdownBlockKind::BlockQuote
    } else {
        MarkdownBlockKind::Paragraph
    }
}

fn start_block(block: &mut Option<MarkdownBlock>, kind: MarkdownBlockKind, start_byte: usize) {
    if block.is_none() {
        *block = Some(MarkdownBlock::new(kind, start_byte));
    }
}

fn push_block_text(
    block: &mut Option<MarkdownBlock>,
    kind: MarkdownBlockKind,
    start_byte: usize,
    end_byte: usize,
    text: &str,
) {
    start_block(block, kind, start_byte);
    if let Some(block) = block {
        block.text.push_str(text);
        block.end_byte = block.end_byte.max(end_byte);
    }
}

fn push_block_separator(block: &mut Option<MarkdownBlock>, in_code_block: bool, end_byte: usize) {
    let Some(block) = block else {
        return;
    };
    let separator = if in_code_block { '\n' } else { ' ' };
    if !block.text.chars().last().is_some_and(char::is_whitespace) {
        block.text.push(separator);
    }
    block.end_byte = block.end_byte.max(end_byte);
}

fn flush_block(
    block: &mut Option<MarkdownBlock>,
    document: &MarkdownDocumentContext<'_>,
    heading_stack: &[MarkdownHeadingLocator],
    end_byte_hint: usize,
    position: &mut u32,
    units: &mut Vec<EvidenceUnit>,
) {
    let Some(mut block) = block.take() else {
        return;
    };
    block.end_byte = block
        .end_byte
        .max(end_byte_hint)
        .min(document.content.len());

    let trimmed = block.text.trim();
    if trimmed.is_empty() {
        return;
    }

    let start_byte = block.start_byte.min(document.content.len());
    let end_byte = block.end_byte.max(start_byte).min(document.content.len());
    let line_start = line_number(document.content, start_byte);
    let line_end = end_line_number(document.content, end_byte);
    let heading_level = heading_stack.last().map(|heading| heading.level);
    let heading_slug = heading_stack.last().map(|heading| heading.slug.clone());

    let text_hash = hex_sha256(trimmed.as_bytes());
    let block_hash = markdown_block_hash(block.kind, heading_stack, trimmed);
    units.push(EvidenceUnit {
        id: EvidenceId(format!(
            "{}:L{}:n{}",
            document.source_id.0, line_start, position
        )),
        source_id: document.source_id.to_owned(),
        kind: EvidenceKind::Text,
        derived_from: None,
        locator: SourceLocator::Markdown {
            path: document.path_str.to_string(),
            line_start,
            line_end,
            byte_start: start_byte as u64,
            byte_end: end_byte as u64,
            block_kind: block.kind,
            block_index: *position,
            block_hash,
            heading_level,
            heading_slug,
            heading_path: heading_stack.to_vec(),
        },
        text: trimmed.to_string(),
        text_hash,
        heading_path: heading_stack
            .iter()
            .map(|heading| heading.text.clone())
            .collect(),
        position: *position,
    });
    *position += 1;
}

fn markdown_block_hash(
    kind: MarkdownBlockKind,
    heading_stack: &[MarkdownHeadingLocator],
    trimmed_text: &str,
) -> String {
    let mut input = String::new();
    input.push_str(kind.as_str());
    input.push('\n');
    for heading in heading_stack {
        input.push_str(&heading.slug);
        input.push('\n');
    }
    input.push_str(trimmed_text);
    hex_sha256(input.as_bytes())
}

fn unique_heading_slug(text: &str, counts: &mut HashMap<String, u32>) -> String {
    let base = heading_slug_base(text);
    let count = counts.entry(base.clone()).or_insert(0);
    let slug = if *count == 0 {
        base
    } else {
        format!("{base}-{count}")
    };
    *count += 1;
    slug
}

fn heading_slug_base(text: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;

    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_was_separator = false;
        } else if !previous_was_separator && !slug.is_empty() {
            slug.push('-');
            previous_was_separator = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "section".to_string()
    } else {
        slug
    }
}

fn heading_level_number(level: HeadingLevel) -> u32 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn line_number(content: &str, byte_offset: usize) -> u32 {
    byte_to_line(content, byte_offset) as u32 + 1
}

fn end_line_number(content: &str, byte_end: usize) -> u32 {
    if content.is_empty() {
        return 1;
    }
    let last_byte = byte_end
        .saturating_sub(1)
        .min(content.len().saturating_sub(1));
    line_number(content, last_byte)
}

fn byte_to_line(content: &str, byte_offset: usize) -> usize {
    let clamped = byte_offset.min(content.len());
    content[..clamped].matches('\n').count()
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
                SourceLocator::Markdown { line_start, .. } => {
                    assert!(*line_start >= 1);
                }
                _ => panic!("expected Markdown locator"),
            }
        }
    }

    #[test]
    fn mvp_regression_markdown_headings_and_links() {
        let markdown =
            "# Retrieval Notes\n\nSee [graph expansion](https://example.test/graph) for citation flow.\n";
        let units = parse_str(markdown);

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].heading_path, vec!["Retrieval Notes"]);
        assert_eq!(units[0].text, "See graph expansion for citation flow.");
        match &units[0].locator {
            SourceLocator::Markdown {
                line_start,
                line_end,
                block_kind,
                heading_slug,
                byte_start,
                byte_end,
                ..
            } => {
                assert_eq!(*line_start, 3);
                assert_eq!(*line_end, 3);
                assert_eq!(*block_kind, MarkdownBlockKind::Paragraph);
                assert_eq!(heading_slug.as_deref(), Some("retrieval-notes"));
                let source_slice = &markdown[*byte_start as usize..*byte_end as usize];
                assert!(source_slice.contains("[graph expansion](https://example.test/graph)"));
            }
            _ => panic!("expected Markdown locator"),
        }
    }

    #[test]
    fn structural_locators_include_heading_slugs_block_kinds_and_ranges() {
        let markdown = "# Intro\n\nAlpha [link](https://example.test).\n\n## Details\n\n> Quoted line\n> continued.\n\n## Details\n\n- First item\n- Second item\n";
        let units = parse_str(markdown);

        assert_eq!(units.len(), 4);

        match &units[0].locator {
            SourceLocator::Markdown {
                line_start,
                line_end,
                byte_start,
                byte_end,
                block_kind,
                block_index,
                block_hash,
                heading_level,
                heading_slug,
                heading_path,
                ..
            } => {
                assert_eq!(*line_start, 3);
                assert_eq!(*line_end, 3);
                assert_eq!(*block_kind, MarkdownBlockKind::Paragraph);
                assert_eq!(*block_index, 0);
                assert!(!block_hash.is_empty());
                assert_eq!(*heading_level, Some(1));
                assert_eq!(heading_slug.as_deref(), Some("intro"));
                assert_eq!(heading_path.len(), 1);
                assert_eq!(heading_path[0].level, 1);
                assert_eq!(heading_path[0].text, "Intro");
                assert_eq!(heading_path[0].slug, "intro");
                assert_eq!(heading_path[0].line, 1);
                let source_slice = &markdown[*byte_start as usize..*byte_end as usize];
                assert!(source_slice.contains("[link](https://example.test)"));
            }
            _ => panic!("expected Markdown locator"),
        }

        match &units[1].locator {
            SourceLocator::Markdown {
                line_start,
                line_end,
                block_kind,
                heading_slug,
                ..
            } => {
                assert_eq!(*line_start, 7);
                assert_eq!(*line_end, 8);
                assert_eq!(*block_kind, MarkdownBlockKind::BlockQuote);
                assert_eq!(heading_slug.as_deref(), Some("details"));
                assert_eq!(units[1].text, "Quoted line continued.");
            }
            _ => panic!("expected Markdown locator"),
        }

        match &units[2].locator {
            SourceLocator::Markdown {
                line_start,
                block_kind,
                heading_level,
                heading_slug,
                heading_path,
                ..
            } => {
                assert_eq!(*line_start, 12);
                assert_eq!(*block_kind, MarkdownBlockKind::ListItem);
                assert_eq!(*heading_level, Some(2));
                assert_eq!(heading_slug.as_deref(), Some("details-1"));
                assert_eq!(heading_path[0].slug, "intro");
                assert_eq!(heading_path[1].slug, "details-1");
                assert_eq!(heading_path[1].line, 10);
                assert_eq!(units[2].text, "First item");
            }
            _ => panic!("expected Markdown locator"),
        }
    }

    #[test]
    fn block_hash_does_not_include_line_numbers_or_path() {
        let original = parse_str("# Intro\n\nStable paragraph.\n");
        let shifted = parse_str("\n\n# Intro\n\nStable paragraph.\n");

        let SourceLocator::Markdown {
            line_start: original_line,
            block_hash: original_hash,
            ..
        } = &original[0].locator
        else {
            panic!("expected Markdown locator");
        };
        let SourceLocator::Markdown {
            line_start: shifted_line,
            block_hash: shifted_hash,
            ..
        } = &shifted[0].locator
        else {
            panic!("expected Markdown locator");
        };

        assert_ne!(original_line, shifted_line);
        assert_eq!(original_hash, shifted_hash);
    }
}
