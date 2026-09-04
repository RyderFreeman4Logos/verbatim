use super::*;
use crate::types::{BackingSelector, EvidenceKind, SourceLocator};
use std::io::Write;
use std::time::{Duration, Instant};
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

const ONE_VERSE_BODY: &str = r#"<usx version="3.0">
  <book code="JHN" style="id">John</book>
  <chapter number="3" style="c" altnumber="III" pubnumber="3" sid="JHN 3"/>
  <para style="p"><verse number="16" style="v" altnumber="XVI" pubnumber="16" sid="JHN 3:16"/>For God so loved the world.<verse eid="JHN 3:16"/></para>
  <chapter eid="JHN 3"/>
</usx>
"#;

fn assert_rejects_declaration(declaration: &str, expected: &str) {
    let contents = format!("{declaration}\n{ONE_VERSE_BODY}");
    let file = fixture(contents.as_bytes());
    let error = UsxParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains(expected), "{error}");
    assert!(error.contains("line 1"), "{error}");
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
}

#[test]
fn usx_notes_and_cross_references_emit_annotated_units_linked_to_verse() {
    let file = fixture(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<usx version="3.0">
  <book code="JHN" style="id">John</book>
  <chapter number="3" style="c" sid="JHN 3"/>
  <para style="p"><verse number="16" style="v" sid="JHN 3:16"/>For God so loved the world.<note caller="+" style="f"><char style="ft">Or loved all people.</char></note><note caller="+" style="fe"><char style="ft">Endnote text.</char></note><note caller="+" style="x"><char style="xo">3:16</char><char style="xt">John 3:16</char></note><verse eid="JHN 3:16"/></para>
  <chapter eid="JHN 3"/>
</usx>
"#,
    );
    let units = UsxParser.parse(file.path()).unwrap();

    assert_eq!(units.len(), 4);
    let verse = &units[0];
    assert_eq!(verse.kind, EvidenceKind::Verse);
    assert_eq!(verse.text, "For God so loved the world.");
    for (unit, note_type, text) in [
        (&units[1], "footnote", "Or loved all people."),
        (&units[2], "endnote", "Endnote text."),
        (&units[3], "cross_reference", "John 3:16"),
    ] {
        assert_eq!(unit.kind, EvidenceKind::Footnote);
        assert_eq!(unit.text, text);
        assert_eq!(unit.derived_from.as_ref(), Some(&verse.id));
        assert_eq!(
            unit.annotations.get("note_type"),
            Some(&note_type.to_string())
        );
    }
}

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
fn rejects_illegal_xml_declaration_grammar_before_units() {
    for (declaration, expected) in [
        (r#"<?xml version="1.0" foo="bar"?>"#, "USX_MALFORMED_XML"),
        (
            r#"<?xml version="1.0" encoding="UTF-8" encoding="UTF-8"?>"#,
            "USX_MALFORMED_XML",
        ),
        (
            r#"<?xml version="1.0" standalone="maybe"?>"#,
            "USX_UNSUPPORTED_XML_DECLARATION",
        ),
        (
            r#"<?xml encoding="UTF-8" version="1.0"?>"#,
            "USX_MALFORMED_XML",
        ),
    ] {
        assert_rejects_declaration(declaration, expected);
    }
}

#[test]
fn parses_event_dense_source_with_bounded_line_tracking() {
    let mut contents = String::with_capacity(300_000);
    contents.push_str("<usx version=\"3.0\"><book code=\"JHN\" style=\"id\"/><chapter number=\"3\" style=\"c\" sid=\"JHN 3\"/><para style=\"p\"><verse number=\"16\" style=\"v\" sid=\"JHN 3:16\"/>");
    for _ in 0..10_000 {
        contents.push_str("<char style=\"add\">x</char>");
    }
    contents.push_str("<verse eid=\"JHN 3:16\"/></para><chapter eid=\"JHN 3\"/></usx>");
    let file = fixture(contents.as_bytes());

    let started = Instant::now();
    let units = UsxParser.parse(file.path()).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "event-dense USX parsing exceeded the bounded duration"
    );
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].text.len(), 10_000);
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
