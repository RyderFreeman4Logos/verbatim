use super::*;
use crate::types::{BackingSelector, EvidenceKind, SourceLocator};
use std::io::Write;
use tempfile::NamedTempFile;

fn fixture(contents: &[u8]) -> NamedTempFile {
    let mut file = NamedTempFile::with_suffix(".usx").unwrap();
    file.write_all(contents).unwrap();
    file.flush().unwrap();
    file
}

const ONE_VERSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<usx version="3.0">
  <book code="JHN" style="id">John</book>
  <chapter number="3" style="c" altnumber="III" pubnumber="3" sid="JHN 3"/>
  <para style="p"><verse number="16" style="v" altnumber="XVI" pubnumber="16" sid="JHN 3:16"/>For God so loved the world.<verse eid="JHN 3:16"/></para>
  <chapter eid="JHN 3"/>
</usx>
"#;

#[test]
fn parses_one_verse_with_canonical_usx_backing() {
    let file = fixture(ONE_VERSE.as_bytes());
    let units = UsxParser.parse(file.path()).unwrap();
    assert_eq!(units.len(), 1);
    let unit = &units[0];
    assert_eq!(unit.kind, EvidenceKind::Verse);
    assert_eq!(unit.text, "For God so loved the world.");
    match &unit.locator {
        SourceLocator::Canonical { locator } => {
            assert_eq!(locator.display, "John 3:16");
            assert_eq!(locator.normalized, "john:3:16");
            assert!(locator
                .backing_selectors
                .contains(&BackingSelector::SourceNative {
                    scheme: "usx".to_string(),
                    value: "JHN 3:16".to_string(),
                }));
        }
        locator => panic!("expected canonical locator, got {locator:?}"),
    }
}

#[test]
fn rejects_unknown_marker_with_line_and_path() {
    let file = fixture(b"<usx version=\"3.0\"><book code=\"JHN\" style=\"id\"/><mystery/></usx>");
    let error = UsxParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("USX_UNKNOWN_MARKER"));
    assert!(error.contains("line 1"));
    assert!(error.contains(&file.path().display().to_string()));
}

#[test]
fn rejects_unclosed_verse_without_partial_units() {
    let file = fixture(
        b"<usx version=\"3.0\"><book code=\"JHN\" style=\"id\"/><chapter number=\"3\" style=\"c\" sid=\"JHN 3\"/><para style=\"p\"><verse number=\"16\" style=\"v\" sid=\"JHN 3:16\"/>text</para></usx>",
    );
    let error = UsxParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("USX_INCOMPLETE_ROOT") || error.contains("USX_MALFORMED_XML"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_doctype_before_parsing_any_units() {
    let file = fixture(b"<!DOCTYPE usx><usx version=\"3.0\"/>");
    let error = UsxParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("USX_DOCTYPE_REJECTED"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_invalid_coordinate_with_line_and_path() {
    let file = fixture(
        b"<usx version=\"3.0\"><book code=\"JHN\" style=\"id\"/><chapter number=\"3\" style=\"c\" sid=\"JHN 3\"/><para style=\"p\"><verse number=\"0\" style=\"v\" sid=\"JHN 3:0\"/></para></usx>",
    );
    let error = UsxParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("USX_INVALID_COORDINATE"));
    assert!(error.contains("line 1"));
    assert!(error.contains(&file.path().display().to_string()));
}
