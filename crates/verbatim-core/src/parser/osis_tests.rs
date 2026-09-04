use super::OsisParser;
use crate::traits::Parser;
use crate::types::{BackingSelector, EvidenceUnit, SourceLocator};
use std::io::Write;
use tempfile::NamedTempFile;

fn fixture(contents: &str) -> NamedTempFile {
    let mut file = NamedTempFile::with_suffix(".osis").unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.flush().unwrap();
    file
}

fn byte_fixture(contents: &[u8]) -> NamedTempFile {
    let mut file = NamedTempFile::with_suffix(".osis").unwrap();
    file.write_all(contents).unwrap();
    file.flush().unwrap();
    file
}

fn without_line_selectors(unit: &mut EvidenceUnit) {
    if let SourceLocator::Canonical { locator } = &mut unit.locator {
        locator
            .backing_selectors
            .retain(|selector| !matches!(selector, BackingSelector::LineRange { .. }));
    }
}

#[test]
fn container_and_chapter_verse_milestones_emit_equivalent_evidence() {
    let container = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">For God so loved the world.</verse></chapter></div></osisText></osis>",
    );
    let milestones = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter sID=\"John.3\"/><verse sID=\"JHN.3.16\"/>For God so loved the world.<verse eID=\"JHN.3.16\"/><chapter eID=\"John.3\"/></div></osisText></osis>",
    );
    let parser = OsisParser;
    let expected = parser.parse(container.path()).unwrap();
    let actual = parser.parse(milestones.path()).unwrap();

    assert_eq!(expected.len(), 1);
    assert_eq!(actual.len(), 1);
    let mut expected = expected[0].clone();
    let mut actual = actual[0].clone();
    actual.id = expected.id.clone();
    actual.source_id = expected.source_id.clone();
    without_line_selectors(&mut expected);
    without_line_selectors(&mut actual);
    assert_eq!(actual, expected);
}

fn milestone_document(body: &str) -> String {
    format!("<osis><osisText><div type=\"book\" osisID=\"John\">{body}</div></osisText></osis>")
}

#[test]
fn malformed_milestone_pairs_fail_closed_with_stable_diagnostics() {
    let cases = [
        (
            "both_start_and_end",
            "<chapter sID=\"John.3\" eID=\"John.3\"/>",
        ),
        (
            "mismatched_end_id",
            "<chapter sID=\"John.3\"/><verse sID=\"JHN.3.16\"/>text<verse eID=\"JHN.3.17\"/>",
        ),
        (
            "overlapping_start",
            "<chapter sID=\"John.3\"/><chapter sID=\"John.3\"/>",
        ),
        (
            "duplicate_end",
            "<chapter sID=\"John.3\"/><chapter eID=\"John.3\"/><chapter eID=\"John.3\"/>",
        ),
        ("unclosed_chapter", "<chapter sID=\"John.3\"/>"),
        (
            "eof_open_verse",
            "<chapter sID=\"John.3\"/><verse sID=\"JHN.3.16\"/>text",
        ),
        (
            "mixed_container_milestone",
            "<chapter osisID=\"John.3\"><verse sID=\"JHN.3.16\"/>text",
        ),
        (
            "text_outside_open_verse",
            "<chapter sID=\"John.3\"/>text<chapter eID=\"John.3\"/>",
        ),
    ];

    for (name, body) in cases {
        let file = fixture(&milestone_document(body));
        let error = OsisParser.parse(file.path()).unwrap_err().to_string();
        assert!(
            error.contains("OSIS_MALFORMED_MILESTONE"),
            "{name}: {error}"
        );
        assert!(error.contains("line 1"), "{name}: {error}");
        assert!(
            error.contains(file.path().to_str().unwrap()),
            "{name}: {error}"
        );
    }
}

#[test]
fn rejects_invalid_utf8_with_line_and_path() {
    let file = byte_fixture(b"<osis>\n\xff");
    let error = OsisParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 2"));
    assert!(error.contains(file.path().to_str().unwrap()));
}

#[test]
fn rejects_xml_10_illegal_character_with_line_and_path() {
    let file = byte_fixture(
        b"<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">bad\x01text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
    assert!(error.contains(file.path().to_str().unwrap()));
}

#[test]
fn rejects_hex_xml_character_reference_with_line_and_path() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">bad&#x1;text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
    assert!(error.contains(file.path().to_str().unwrap()));
}

#[test]
fn rejects_decimal_xml_character_reference_with_line_and_path() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">bad&#1;text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
    assert!(error.contains(file.path().to_str().unwrap()));
}

#[test]
fn rejects_xml_character_reference_in_attribute_with_line_and_path() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John&#x1;\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
    assert!(error.contains(file.path().to_str().unwrap()));
}

#[test]
fn rejects_missing_osis_id() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse>text</verse></chapter></div></osisText></osis>",
    );
    let error = OsisParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("OSIS_MISSING_ATTRIBUTE"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_invalid_verse_coordinate() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.37\">text</verse></chapter></div></osisText></osis>",
    );
    let error = OsisParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("OSIS_INVALID_COORDINATE"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_duplicate_osis_id_attributes() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\" osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let error = OsisParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
}

#[test]
fn accepts_legal_leading_xml_declaration() {
    let file = fixture(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let units = OsisParser.parse(file.path()).unwrap();
    assert_eq!(units.len(), 1);
}

#[test]
fn accepts_xml_declaration_with_standalone_without_encoding() {
    for standalone in ["yes", "no"] {
        let file = fixture(&format!(
            "<?xml version=\"1.0\" standalone=\"{standalone}\"?>\n<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>"
        ));
        let units = OsisParser.parse(file.path()).unwrap();
        assert_eq!(units.len(), 1);
    }
}

#[test]
fn rejects_duplicate_xml_declaration_encoding_without_partial_evidence() {
    let file = fixture(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" encoding=\"UTF-8\"?><osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_unknown_xml_declaration_attribute_without_partial_evidence() {
    let file = fixture(
        "<?xml version=\"1.0\" foo=\"bar\"?><osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_out_of_order_xml_declaration_encoding_without_partial_evidence() {
    let file = fixture(
        "<?xml encoding=\"UTF-8\" version=\"1.0\"?><osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_MALFORMED_XML"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_xml_declaration_after_prolog_whitespace() {
    let file = fixture(
        " \n<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let error = OsisParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("OSIS_UNEXPECTED_STRUCTURE"));
    assert!(error.contains("line 2"));
}

#[test]
fn rejects_unsupported_xml_version() {
    let file = fixture(
        "<?xml version=\"1.1\" encoding=\"UTF-8\"?><osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let error = OsisParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("OSIS_UNSUPPORTED_XML_DECLARATION"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_unsupported_xml_encoding() {
    let file = fixture(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText></osis>",
    );
    let error = OsisParser.parse(file.path()).unwrap_err().to_string();
    assert!(error.contains("OSIS_UNSUPPORTED_XML_DECLARATION"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_extra_osis_text_without_partial_evidence() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter></div></osisText><osisText></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_DUPLICATE_OSIS_TEXT"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_duplicate_chapter_without_partial_evidence() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse></chapter><chapter osisID=\"John.4\"><verse osisID=\"JHN.4.1\">text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_DUPLICATE_CHAPTER"));
    assert!(error.contains("line 1"));
}

#[test]
fn rejects_duplicate_verse_without_partial_evidence() {
    let file = fixture(
        "<osis><osisText><div type=\"book\" osisID=\"John\"><chapter osisID=\"John.3\"><verse osisID=\"JHN.3.16\">text</verse><verse osisID=\"JHN.3.17\">text</verse></chapter></div></osisText></osis>",
    );
    let result = OsisParser.parse(file.path());
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("OSIS_DUPLICATE_VERSE"));
    assert!(error.contains("line 1"));
}
