use super::*;
use crate::parser::{parser_for_extension, select_parser};
use crate::traits::Parser;
use crate::types::{BackingSelector, EvidenceKind, SourceLocator};
use std::io::Write;
use tempfile::NamedTempFile;

fn fixture(contents: &[u8]) -> NamedTempFile {
    let mut file = NamedTempFile::with_suffix(".usj").unwrap();
    file.write_all(contents).unwrap();
    file.flush().unwrap();
    file
}

fn line_range(unit: &crate::types::EvidenceUnit) -> (u32, u32) {
    let SourceLocator::Canonical { locator } = &unit.locator else {
        panic!("expected canonical locator")
    };
    locator
        .backing_selectors
        .iter()
        .find_map(|selector| match selector {
            BackingSelector::LineRange { start, end } => Some((*start, *end)),
            _ => None,
        })
        .expect("expected line range backing")
}

const ONE_VERSE: &str = r#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      {"type": "verse", "marker": "v", "number": "16", "sid": "JHN 3:16"},
      "For God so loved the world."
    ]}
  ]
}
"#;

#[test]
fn explicit_and_extension_selection_route_to_usj() {
    assert_eq!(select_parser("usj").unwrap().name(), "usj");
    assert_eq!(
        parser_for_extension(std::path::Path::new("source.USJ"))
            .unwrap()
            .name(),
        "usj"
    );
}

#[test]
fn line_range_uses_verse_span_when_object_keys_are_reordered() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      {
        "sid": "JHN 3:16",
        "marker": "v",
        "number": "16",
        "type": "verse"
      },
      "first verse",
      {"type": "verse", "marker": "v", "number": "17", "sid": "JHN 3:17"},
      "second verse"
    ]}
  ]
}"#,
    );
    let units = UsjParser.parse(file.path()).unwrap();

    assert_eq!(units.len(), 2);
    assert_eq!(line_range(&units[0]), (8, 14));
    assert_eq!(line_range(&units[1]), (15, 16));
}

#[test]
fn line_range_covers_text_with_alternate_json_escapes() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      {"type": "verse", "marker": "v", "number": "16", "sid": "JHN 3:16"},
      "\u0046or God so loved the world."
    ]}
  ]
}"#,
    );
    let units = UsjParser.parse(file.path()).unwrap();

    assert_eq!(units.len(), 1);
    assert_eq!(units[0].text, "For God so loved the world.");
    assert_eq!(line_range(&units[0]), (8, 9));
}

#[test]
fn line_range_covers_the_complete_multiline_verse_node() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      {
        "sid": "JHN 3:16",
        "content": ["line one\nline two"],
        "marker": "v",
        "number": "16",
        "type": "verse"
      }
    ]}
  ]
}"#,
    );
    let units = UsjParser.parse(file.path()).unwrap();

    assert_eq!(units.len(), 1);
    assert_eq!(units[0].text, "line one\nline two");
    assert_eq!(line_range(&units[0]), (8, 14));
}

#[test]
fn parses_one_verse_with_canonical_usj_backing() {
    let file = fixture(ONE_VERSE.as_bytes());
    let units = UsjParser.parse(file.path()).unwrap();

    assert_eq!(units.len(), 1);
    let unit = &units[0];
    assert_eq!(unit.kind, EvidenceKind::Verse);
    assert_eq!(unit.text, "For God so loved the world.");
    match &unit.locator {
        SourceLocator::Canonical { locator } => {
            assert_eq!(locator.profile_id, "bible");
            assert_eq!(locator.work_id, "USJ");
            assert_eq!(locator.display, "John 3:16");
            assert_eq!(locator.normalized, "john:3:16");
            assert!(locator
                .backing_selectors
                .contains(&BackingSelector::SourceNative {
                    scheme: "usj".to_string(),
                    value: "JHN 3:16".to_string(),
                }));
            assert!(locator
                .backing_selectors
                .contains(&BackingSelector::LineRange { start: 8, end: 9 }));
        }
        locator => panic!("expected canonical locator, got {locator:?}"),
    }
}

#[test]
fn rejects_unknown_critical_node_with_line_and_path() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "mystery", "marker": "x"}
  ]
}"#,
    );
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_UNKNOWN_CRITICAL"), "{error}");
    assert!(error.contains("line 6"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn rejects_invalid_coordinate_with_line_and_path() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      {"type": "verse", "marker": "v", "number": "0", "sid": "JHN 3:0"},
      "invalid"
    ]}
  ]
}"#,
    );
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_INVALID_COORDINATE"), "{error}");
    assert!(error.contains("line 8"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn rejects_incomplete_root_with_diagnostic_and_no_units() {
    let file = fixture(br#"{"type":"USJ","version":"3.1"}"#);
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_INCOMPLETE_ROOT"), "{error}");
    assert!(error.contains("line 1"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn rejects_malformed_json_with_line_and_path() {
    let file = fixture(br#"{"type":"USJ","version":"3.1","content":[}"#);
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_INVALID_JSON"), "{error}");
    assert!(error.contains("line 1"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn rejects_invalid_utf8_with_line_and_path() {
    let file = fixture(b"{\xff}");
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_INVALID_UTF8"), "{error}");
    assert!(error.contains("line 1"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn rejects_duplicate_verse_without_partial_units() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      {"type": "verse", "marker": "v", "number": "16", "sid": "JHN 3:16"},
      "first",
      {"type": "verse", "marker": "v", "number": "16", "sid": "JHN 3:16"},
      "second"
    ]}
  ]
}"#,
    );
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_DUPLICATE_VERSE"), "{error}");
    assert!(error.contains("line 10"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn rejects_unscoped_text_without_partial_units() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      "orphan text",
      {"type": "verse", "marker": "v", "number": "16", "sid": "JHN 3:16"},
      "verse text"
    ]}
  ]
}"#,
    );
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_UNEXPECTED_STRUCTURE"), "{error}");
    assert!(error.contains("line 8"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn rejects_notes_until_the_notes_slice_is_implemented() {
    let file = fixture(
        br#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "note", "marker": "f", "content": ["not implemented"]}
  ]
}"#,
    );
    let error = UsjParser.parse(file.path()).unwrap_err().to_string();

    assert!(error.contains("USJ_UNKNOWN_CRITICAL"), "{error}");
    assert!(error.contains("line 6"), "{error}");
}
