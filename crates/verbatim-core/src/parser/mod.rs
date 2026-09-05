#[cfg(feature = "parser-anydoc-pdf")]
pub mod anydoc;
#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
mod bounded_pdf_images;
pub mod canonical_jsonl;
pub mod canonical_package;
mod canonical_package_conversion;
pub mod epub_inspect;
mod epub_xml;
pub mod json;
pub mod markdown;
pub mod osis;
#[cfg(feature = "parser-pdf-oxide")]
pub mod oxide;
pub mod plaintext;
#[cfg(feature = "parser-pdfplumber")]
pub mod plumber;
pub(crate) mod text_segments;
pub mod usfm;
pub mod usj;
pub mod usx;

#[cfg(test)]
mod round_trip_tests;

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::traits::Parser;

pub(crate) const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn read_bounded_source<R: Read>(
    reader: R,
    format: &str,
    path: &Path,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {format} source {}", path.display()))?;
    if bytes.len() > MAX_SOURCE_BYTES {
        bail!(
            "{format}_SOURCE_TOO_LARGE: source exceeds {MAX_SOURCE_BYTES} bytes on line 1 of {}",
            path.display()
        );
    }
    Ok(bytes)
}

pub fn select_parser(name: &str) -> Result<Box<dyn Parser>> {
    match name {
        "canonical_jsonl" => Ok(Box::new(canonical_jsonl::CanonicalJsonlParser)),
        "usfm" => Ok(Box::new(usfm::UsfmParser)),
        "usx" => Ok(Box::new(usx::UsxParser)),
        "usj" => Ok(Box::new(usj::UsjParser)),
        "osis" => Ok(Box::new(osis::OsisParser)),

        #[cfg(feature = "parser-pdf-oxide")]
        "pdf_oxide" => Ok(Box::new(oxide::PdfOxideParser)),
        #[cfg(feature = "parser-pdfplumber")]
        "pdfplumber" => Ok(Box::new(plumber::PdfPlumberParser)),
        #[cfg(feature = "parser-anydoc-pdf")]
        "anydoc_pdf" => Ok(Box::new(anydoc::AnyDocPdfParser)),
        "json" => Ok(Box::new(json::JsonParser)),
        "markdown" => Ok(Box::new(markdown::MarkdownParser)),
        "plaintext" => Ok(Box::new(plaintext::PlaintextParser)),
        _ => bail!(
            "unknown parser: {name}. Available: canonical_jsonl, usfm, usx, usj, osis, pdf_oxide, pdfplumber, anydoc_pdf, json, markdown, plaintext"
        ),
    }
}

pub fn parser_for_extension(path: &Path) -> Result<Box<dyn Parser>> {
    if path.is_dir() {
        return Ok(Box::new(canonical_package::CanonicalPackageParser));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => {
            #[cfg(feature = "parser-anydoc-pdf")]
            return Ok(Box::new(anydoc::AnyDocPdfParser));
            #[cfg(all(feature = "parser-pdf-oxide", not(feature = "parser-anydoc-pdf")))]
            return Ok(Box::new(oxide::PdfOxideParser));
            #[cfg(not(any(feature = "parser-pdf-oxide", feature = "parser-anydoc-pdf")))]
            bail!("no PDF parser available (enable parser-pdf-oxide or parser-anydoc-pdf feature)")
        }
        "jsonl" => Ok(Box::new(canonical_jsonl::CanonicalJsonlParser)),
        "usfm" => Ok(Box::new(usfm::UsfmParser)),
        "usx" => Ok(Box::new(usx::UsxParser)),
        "usj" => Ok(Box::new(usj::UsjParser)),
        "osis" => Ok(Box::new(osis::OsisParser)),
        "json" => Ok(Box::new(json::JsonParser)),
        "md" | "markdown" => Ok(Box::new(markdown::MarkdownParser)),
        "txt" | "text" => Ok(Box::new(plaintext::PlaintextParser)),
        _ => bail!("unsupported file extension: .{ext}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{parser_for_extension, select_parser, MAX_SOURCE_BYTES};
    use crate::types::{BackingSelector, EvidenceKind, SourceLocator};
    use std::io::Write;
    use tempfile::NamedTempFile;

    const ONE_VERSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<osis>
  <osisText>
    <div type="book" osisID="John">
      <div type="chapter" osisID="John.3">
        <verse osisID="JHN.3.16">For God so loved the world.</verse>
      </div>
    </div>
  </osisText>
</osis>
"#;

    const ONE_USX_VERSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<usx version="3.0">
  <book code="JHN" style="id">John</book>
  <chapter number="3" style="c" sid="JHN 3"/>
  <para style="p">
    <verse number="16" style="v" sid="JHN 3:16"/>For God so loved the world.
    <verse eid="JHN 3:16"/>
  </para>
  <chapter eid="JHN 3"/>
</usx>
"#;

    const ONE_USFM_VERSE: &str = r#"\id JHN
\c 3
\v 16 For God so loved the world.
"#;

    const ONE_USJ_VERSE: &str = r#"{
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

    fn fixture(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::with_suffix(".osis").unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    fn structured_fixture(contents: &[u8], suffix: &str) -> NamedTempFile {
        let mut file = NamedTempFile::with_suffix(suffix).unwrap();
        file.write_all(contents).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn usx_verse_container_emits_canonical_source_native_evidence() {
        let file = NamedTempFile::with_suffix(".usx").unwrap();
        std::fs::write(file.path(), ONE_USX_VERSE).unwrap();
        let parser = select_parser("usx").unwrap();
        assert_eq!(parser.name(), "usx");
        let units = parser.parse(file.path()).unwrap();

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
                assert!(locator
                    .backing_selectors
                    .contains(&BackingSelector::LineRange { start: 6, end: 7 }));
            }
            locator => panic!("expected canonical locator, got {locator:?}"),
        }
    }

    #[test]
    fn osis_verse_container_emits_canonical_source_native_evidence() {
        let file = fixture(ONE_VERSE);
        let parser = select_parser("osis").unwrap();
        assert_eq!(parser.name(), "osis");
        let units = parser.parse(file.path()).unwrap();

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
                        scheme: "osis".to_string(),
                        value: "JHN.3.16".to_string(),
                    }));
                assert!(locator
                    .backing_selectors
                    .iter()
                    .any(|selector| matches!(selector, BackingSelector::LineRange { .. })));
            }
            locator => panic!("expected canonical locator, got {locator:?}"),
        }
    }

    #[test]
    fn osis_extension_selects_only_osis_parser() {
        let file = fixture(ONE_VERSE);
        let parser = parser_for_extension(file.path()).unwrap();
        assert_eq!(parser.name(), "osis");
        assert!(parser_for_extension(std::path::Path::new("source.xml")).is_err());
    }

    #[test]
    fn usx_extension_selects_usx_parser() {
        let parser = parser_for_extension(std::path::Path::new("source.usx")).unwrap();
        assert_eq!(parser.name(), "usx");
    }

    #[test]
    fn structured_bible_parsers_reject_oversize_sources_without_partial_units() {
        let oversized_len = MAX_SOURCE_BYTES
            .checked_add(1)
            .expect("source bound should fit in usize");
        for (name, suffix) in [("usfm", ".usfm"), ("usx", ".usx"), ("usj", ".usj")] {
            let file = NamedTempFile::with_suffix(suffix).unwrap();
            file.as_file().set_len(oversized_len as u64).unwrap();
            let parser = select_parser(name).unwrap();
            let error = parser.parse(file.path()).unwrap_err().to_string();

            assert!(
                error.contains(&format!("{}_SOURCE_TOO_LARGE", name.to_ascii_uppercase())),
                "{error}"
            );
            assert!(
                error.contains(&file.path().display().to_string()),
                "{error}"
            );
        }
    }

    #[test]
    fn structured_bible_parsers_stream_small_fixtures_through_existing_parsers() {
        let fixtures = [
            ("usfm", ".usfm", ONE_USFM_VERSE.as_bytes()),
            ("usx", ".usx", ONE_USX_VERSE.as_bytes()),
            ("usj", ".usj", ONE_USJ_VERSE.as_bytes()),
        ];
        for (name, suffix, contents) in fixtures {
            let file = structured_fixture(contents, suffix);
            let parser = select_parser(name).unwrap();
            let units = parser.parse(file.path()).unwrap();

            assert_eq!(units.len(), 1, "{name}");
            assert_eq!(units[0].kind, EvidenceKind::Verse, "{name}");
            let SourceLocator::Canonical { locator } = &units[0].locator else {
                panic!("expected canonical locator for {name}");
            };
            assert_eq!(locator.display, "John 3:16", "{name}");
            assert_eq!(locator.normalized, "john:3:16", "{name}");
        }
    }

    #[test]
    fn osis_doctype_rejects_without_partial_units() {
        let file = fixture(
            "<!DOCTYPE osis [<!ENTITY bible SYSTEM \\\"https://example.invalid/bible\\\">]>\n",
        );
        let parser = select_parser("osis").unwrap();
        let error = parser.parse(file.path()).unwrap_err().to_string();

        assert!(error.contains("OSIS_DOCTYPE_REJECTED"));
        assert!(error.contains("line 1"));
    }

    #[test]
    fn osis_milestone_rejects_with_line_diagnostic() {
        let file = fixture(
            "<osis><osisText><div type=\"book\" osisID=\"John\"><div type=\"chapter\" osisID=\"John.3\"><milestone type=\"x-p\"/></div></div></osisText></osis>",
        );
        let parser = select_parser("osis").unwrap();
        let error = parser.parse(file.path()).unwrap_err().to_string();

        assert!(error.contains("OSIS_UNSUPPORTED_MILESTONE"));
        assert!(error.contains("line 1"));
    }
}
