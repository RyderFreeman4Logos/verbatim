use super::OsisParser;
use crate::traits::Parser;
use std::io::Write;
use tempfile::NamedTempFile;

fn fixture(contents: &str) -> NamedTempFile {
    let mut file = NamedTempFile::with_suffix(".osis").unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.flush().unwrap();
    file
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
