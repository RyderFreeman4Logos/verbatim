#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
use crate::types::{hex_sha256, EvidenceId, EvidenceKind, EvidenceUnit, SourceId, SourceLocator};

pub(crate) fn normalize_line_endings(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(ch);
        }
    }

    normalized
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
pub(crate) fn pdf_page_evidence_units(
    source_id: &SourceId,
    page_num: u32,
    page_text: &str,
    position: &mut u32,
) -> Vec<EvidenceUnit> {
    split_pdf_text_segments(page_text)
        .into_iter()
        .enumerate()
        .map(|(para_idx, text)| {
            let text_hash = hex_sha256(text.as_bytes());
            let unit = EvidenceUnit {
                id: EvidenceId(format!("{}:p{}:n{}", source_id.0, page_num, para_idx)),
                source_id: source_id.clone(),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: SourceLocator::legacy_pdf(page_num, para_idx as u32, None),
                text,
                text_hash,
                heading_path: Vec::new(),
                language: None,
                position: *position,
                annotations: Default::default(),
            };
            *position += 1;
            unit
        })
        .collect()
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn split_pdf_text_segments(text: &str) -> Vec<String> {
    let normalized = normalize_line_endings(text);
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut previous_raw = String::new();
    let mut previous_trimmed = String::new();

    for raw_line in normalized.split('\n') {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            flush_segment(&mut segments, &mut current);
            previous_raw.clear();
            previous_trimmed.clear();
            continue;
        }

        if current.is_empty() {
            current.push_str(trimmed);
        } else if starts_pdf_paragraph(&previous_raw, &previous_trimmed, raw_line, trimmed) {
            flush_segment(&mut segments, &mut current);
            current.push_str(trimmed);
        } else {
            current.push(' ');
            current.push_str(trimmed);
        }

        previous_raw.clear();
        previous_raw.push_str(raw_line);
        previous_trimmed.clear();
        previous_trimmed.push_str(trimmed);
    }

    flush_segment(&mut segments, &mut current);
    segments
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn flush_segment(segments: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    current.clear();
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn starts_pdf_paragraph(
    previous_raw: &str,
    previous_trimmed: &str,
    current_raw: &str,
    current_trimmed: &str,
) -> bool {
    if previous_trimmed.is_empty() {
        return false;
    }
    has_paragraph_indent(current_raw)
        || starts_with_list_marker(current_trimmed)
        || ends_with_terminal_punctuation(previous_trimmed)
            && !looks_like_visual_wrap(previous_raw, current_raw)
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn has_paragraph_indent(line: &str) -> bool {
    let mut chars = line.chars();
    matches!(chars.next(), Some('\t' | '\u{3000}'))
        || line.chars().take_while(|ch| *ch == ' ').take(2).count() >= 2
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn starts_with_list_marker(line: &str) -> bool {
    let mut chars = line.chars();
    match (chars.next(), chars.next()) {
        (Some('-' | '*' | '+'), Some(' ' | '\t')) => true,
        (Some(ch), Some('.' | ')' | '、')) => ch.is_ascii_digit(),
        _ => false,
    }
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn ends_with_terminal_punctuation(line: &str) -> bool {
    line.chars()
        .rev()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| {
            matches!(
                ch,
                '.' | '!'
                    | '?'
                    | ':'
                    | ';'
                    | '"'
                    | '\''
                    | ')'
                    | ']'
                    | '}'
                    | '。'
                    | '！'
                    | '？'
                    | '：'
                    | '；'
                    | '”'
                    | '’'
                    | '）'
                    | '】'
                    | '》'
            )
        })
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn looks_like_visual_wrap(previous_raw: &str, current_raw: &str) -> bool {
    !has_paragraph_indent(current_raw)
        && visual_width(previous_raw.trim()) >= 72
        && visual_width(current_raw.trim()) >= 24
}

#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
fn visual_width(line: &str) -> usize {
    line.chars()
        .map(|ch| if ch.is_ascii() { 1 } else { 2 })
        .sum()
}

#[cfg(all(test, any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber")))]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<String> {
        split_pdf_text_segments(text)
    }

    #[test]
    fn pdf_lf_single_newline_natural_paragraphs_are_split() {
        let segments = split("第一段说明事实。\n第二段继续说明。");

        assert_eq!(segments, vec!["第一段说明事实。", "第二段继续说明。"]);
    }

    #[test]
    fn pdf_crlf_single_newline_natural_paragraphs_are_split() {
        let segments = split("First paragraph.\r\nSecond paragraph.");

        assert_eq!(segments, vec!["First paragraph.", "Second paragraph."]);
    }

    #[test]
    fn pdf_legacy_cr_single_newline_natural_paragraphs_are_split() {
        let segments = split("First paragraph.\rSecond paragraph.");

        assert_eq!(segments, vec!["First paragraph.", "Second paragraph."]);
    }

    #[test]
    fn pdf_blank_line_paragraphs_still_split() {
        let segments = split("First paragraph\ncontinues here\n\nSecond paragraph");

        assert_eq!(
            segments,
            vec!["First paragraph continues here", "Second paragraph"]
        );
    }

    #[test]
    fn pdf_visual_hard_wraps_are_joined() {
        let segments = split(
            "This paragraph was extracted with hard visual line wraps inside\n\
             the same sentence and should remain a single evidence unit\n\
             instead of becoming one unit for every rendered line.",
        );

        assert_eq!(
            segments,
            vec![
                "This paragraph was extracted with hard visual line wraps inside \
                 the same sentence and should remain a single evidence unit \
                 instead of becoming one unit for every rendered line."
            ]
        );
    }

    #[test]
    fn pdf_indented_single_newline_starts_paragraph_without_terminal_punctuation() {
        let segments = split("Chapter heading\n  Paragraph body starts here");

        assert_eq!(
            segments,
            vec!["Chapter heading", "Paragraph body starts here"]
        );
    }

    #[test]
    fn pdf_single_newline_paragraphs_get_stable_locator_indexes() {
        let source_id = SourceId("source-1".to_string());
        let mut position = 7;

        let units = pdf_page_evidence_units(&source_id, 3, "Alpha one.\nBeta two.", &mut position);

        assert_eq!(position, 9);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].id, EvidenceId("source-1:p3:n0".to_string()));
        assert_eq!(units[1].id, EvidenceId("source-1:p3:n1".to_string()));
        assert_eq!(units[0].position, 7);
        assert_eq!(units[1].position, 8);
        assert!(matches!(
            units[0].locator,
            SourceLocator::Pdf {
                page: 3,
                paragraph: 0,
                bbox: None,
                ..
            }
        ));
        assert!(matches!(
            units[1].locator,
            SourceLocator::Pdf {
                page: 3,
                paragraph: 1,
                bbox: None,
                ..
            }
        ));
    }
}
