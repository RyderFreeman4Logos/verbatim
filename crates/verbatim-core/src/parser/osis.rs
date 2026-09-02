use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::profiles::bible::canon_registry::{CanonBook, CanonRegistry, VERSION as CANON_VERSION};
use crate::profiles::bible::versification_registry::{
    VersificationRegistry, VERSION as VERSIFICATION_VERSION,
};
use crate::traits::Parser;
use crate::types::{
    hex_sha256, BackingSelector, CanonicalLocator, EvidenceId, EvidenceKind, EvidenceUnit,
    ReferenceComponent, SourceId, SourceLocator,
};

const SOURCE_NATIVE_SCHEME: &str = "osis";
const WORK_ID: &str = "OSIS";

/// A deliberately narrow parser for one OSIS book/chapter/verse container.
pub struct OsisParser;

impl Parser for OsisParser {
    fn name(&self) -> &str {
        "osis"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["osis"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read OSIS source {}", path.display()))?;
        parse_bytes(&bytes, path)
    }
}

#[derive(Debug)]
enum Element {
    Osis,
    OsisText,
    Book {
        book: CanonBook,
    },
    Chapter {
        book: CanonBook,
        chapter: u16,
        name: String,
    },
    Verse {
        book: CanonBook,
        chapter: u16,
        verse: u16,
        osis_id: String,
        line_start: u32,
        text: String,
    },
}

impl Element {
    fn name(&self) -> &str {
        match self {
            Self::Osis => "osis",
            Self::OsisText => "osisText",
            Self::Book { .. } => "div",
            Self::Chapter { name, .. } => name,
            Self::Verse { .. } => "verse",
        }
    }
}

#[derive(Default)]
struct Counts {
    roots: usize,
    books: usize,
    chapters: usize,
    verses: usize,
}

struct VerseData {
    book: CanonBook,
    chapter: u16,
    verse: u16,
    osis_id: String,
    line_start: u32,
    line_end: u32,
    text: String,
}

fn parse_bytes(bytes: &[u8], path: &Path) -> Result<Vec<EvidenceUnit>> {
    std::str::from_utf8(bytes).map_err(|_| anyhow!("OSIS_MALFORMED_XML: invalid UTF-8"))?;
    reject_declarations(bytes, path)?;

    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut counts = Counts::default();
    let mut verse = None;
    let mut declaration_seen = false;

    loop {
        let event = reader.read_event_into(&mut buffer).map_err(|error| {
            malformed_xml(bytes, reader.buffer_position() as usize, path, error)
        })?;
        let line = line_number(bytes, reader.buffer_position() as usize);
        match event {
            Event::Decl(_) => {
                if declaration_seen || counts.roots != 0 || !stack.is_empty() {
                    bail!(
                        "OSIS_UNEXPECTED_STRUCTURE: XML declaration must precede the root on line {line} of {}",
                        path.display()
                    );
                }
                declaration_seen = true;
            }
            Event::DocType(_) => {
                bail!(
                    "OSIS_DOCTYPE_REJECTED: DTD and external entities are not permitted on line {line} of {}",
                    path.display()
                );
            }
            Event::GeneralRef(reference) => {
                bail!(
                    "OSIS_ENTITY_REJECTED: entity reference `{}` is not permitted on line {line} of {}",
                    String::from_utf8_lossy(reference.as_ref()),
                    path.display()
                );
            }
            Event::Start(start) => {
                let name = xml_name(
                    start.name().as_ref(),
                    bytes,
                    reader.buffer_position() as usize,
                    path,
                )?;
                let attributes = attributes(
                    &start,
                    reader.decoder(),
                    bytes,
                    reader.buffer_position() as usize,
                    path,
                )?;
                reject_milestone(&name, &attributes, line, path)?;
                let element = next_element(&stack, &name, &attributes, &mut counts, line, path)?;
                if matches!(element, Element::Osis) {
                    counts.roots += 1;
                }
                stack.push(element);
            }
            Event::Empty(empty) => {
                let name = xml_name(
                    empty.name().as_ref(),
                    bytes,
                    reader.buffer_position() as usize,
                    path,
                )?;
                let attributes = attributes(
                    &empty,
                    reader.decoder(),
                    bytes,
                    reader.buffer_position() as usize,
                    path,
                )?;
                reject_milestone(&name, &attributes, line, path)?;
                bail!(
                    "OSIS_UNEXPECTED_STRUCTURE: empty `{name}` element is unsupported on line {line} of {}",
                    path.display()
                );
            }
            Event::End(end) => {
                let name = xml_name(
                    end.name().as_ref(),
                    bytes,
                    reader.buffer_position() as usize,
                    path,
                )?;
                let element = stack.pop().ok_or_else(|| {
                    anyhow!(
                        "OSIS_MALFORMED_XML: unexpected end element `{name}` on line {line} of {}",
                        path.display()
                    )
                })?;
                if element.name() != name {
                    bail!(
                        "OSIS_MALFORMED_XML: end element `{name}` does not match `{}` on line {line} of {}",
                        element.name(),
                        path.display()
                    );
                }
                if let Element::Verse {
                    book,
                    chapter,
                    verse: verse_number,
                    osis_id,
                    line_start,
                    text,
                } = element
                {
                    let text = text.trim().to_owned();
                    if text.is_empty() {
                        bail!(
                            "OSIS_EMPTY_VERSE: verse `{osis_id}` has no text on line {line} of {}",
                            path.display()
                        );
                    }
                    verse = Some(VerseData {
                        book,
                        chapter,
                        verse: verse_number,
                        osis_id,
                        line_start,
                        line_end: line,
                        text,
                    });
                }
            }
            Event::Text(text) => {
                let decoded = text.decode().map_err(|error| {
                    malformed_xml(bytes, reader.buffer_position() as usize, path, error)
                })?;
                let decoded = unescape(&decoded).map_err(|error| {
                    malformed_xml(bytes, reader.buffer_position() as usize, path, error)
                })?;
                if let Some(Element::Verse { text, .. }) = stack.last_mut() {
                    text.push_str(&decoded);
                } else if !decoded.trim().is_empty() {
                    bail!(
                        "OSIS_UNEXPECTED_STRUCTURE: non-whitespace text outside verse on line {line} of {}",
                        path.display()
                    );
                }
            }
            Event::CData(_) | Event::Comment(_) | Event::PI(_) => {
                bail!(
                    "OSIS_UNEXPECTED_STRUCTURE: unsupported XML content on line {line} of {}",
                    path.display()
                );
            }
            Event::Eof => break,
        }
        buffer.clear();
    }

    if counts.roots != 1 || !stack.is_empty() {
        bail!(
            "OSIS_MALFORMED_XML: expected one complete root on line {} of {}",
            line_number(bytes, bytes.len()),
            path.display()
        );
    }
    let verse = verse.ok_or_else(|| {
        anyhow!(
            "OSIS_MISSING_VERSE: expected one verse container on line {} of {}",
            line_number(bytes, bytes.len()),
            path.display()
        )
    })?;
    Ok(vec![evidence_unit(verse, path)])
}

fn next_element(
    stack: &[Element],
    name: &str,
    attrs: &BTreeMap<String, String>,
    counts: &mut Counts,
    line: u32,
    path: &Path,
) -> Result<Element> {
    match stack {
        [] => {
            require_name(name, "osis", line, path)?;
            ensure_attributes(attrs, &[], line, path)?;
            if counts.roots != 0 {
                bail!(
                    "OSIS_UNEXPECTED_STRUCTURE: multiple roots are not permitted on line {line} of {}",
                    path.display()
                );
            }
            Ok(Element::Osis)
        }
        [Element::Osis] => {
            require_name(name, "osisText", line, path)?;
            ensure_attributes(attrs, &[], line, path)?;
            Ok(Element::OsisText)
        }
        [Element::Osis, Element::OsisText] => {
            require_name(name, "div", line, path)?;
            ensure_attributes(attrs, &["type", "osisID"], line, path)?;
            require_attr(attrs, "type", line, path).and_then(|value| {
                if value == "book" {
                    Ok(())
                } else {
                    bail!(
                        "OSIS_UNEXPECTED_STRUCTURE: book container requires type=book on line {line} of {}",
                        path.display()
                    );
                }
            })?;
            if counts.books != 0 {
                bail!(
                    "OSIS_DUPLICATE_BOOK: only one book container is supported on line {line} of {}",
                    path.display()
                );
            }
            let book_id = require_osis_id(attrs, line, path)?;
            let book = parse_book(&book_id, line, path)?;
            counts.books += 1;
            Ok(Element::Book { book })
        }
        [Element::Osis, Element::OsisText, Element::Book { book }] => {
            let is_div_chapter =
                name == "div" && attrs.get("type").map(String::as_str) == Some("chapter");
            if name != "chapter" && !is_div_chapter {
                bail!(
                    "OSIS_UNEXPECTED_STRUCTURE: expected chapter container, found `{name}` on line {line} of {}",
                    path.display()
                );
            }
            ensure_attributes(attrs, &["type", "osisID"], line, path)?;
            if name == "chapter" && attrs.contains_key("type") {
                bail!(
                    "OSIS_UNEXPECTED_STRUCTURE: chapter element cannot carry type on line {line} of {}",
                    path.display()
                );
            }
            if counts.chapters != 0 {
                bail!(
                    "OSIS_DUPLICATE_CHAPTER: only one chapter container is supported on line {line} of {}",
                    path.display()
                );
            }
            let id = require_osis_id(attrs, line, path)?;
            let chapter = parse_chapter(&id, *book, line, path)?;
            counts.chapters += 1;
            Ok(Element::Chapter {
                book: *book,
                chapter,
                name: name.to_string(),
            })
        }
        [Element::Osis, Element::OsisText, Element::Book { book }, Element::Chapter {
            book: chapter_book,
            chapter,
            ..
        }] => {
            require_name(name, "verse", line, path)?;
            ensure_attributes(attrs, &["osisID"], line, path)?;
            if counts.verses != 0 {
                bail!(
                    "OSIS_DUPLICATE_VERSE: only one verse container is supported on line {line} of {}",
                    path.display()
                );
            }
            let osis_id = require_osis_id(attrs, line, path)?;
            let verse = parse_verse(&osis_id, *book, *chapter_book, *chapter, line, path)?;
            counts.verses += 1;
            Ok(Element::Verse {
                book: *book,
                chapter: *chapter,
                verse,
                osis_id,
                line_start: line,
                text: String::new(),
            })
        }
        _ => bail!(
            "OSIS_UNEXPECTED_STRUCTURE: unsupported element `{name}` on line {line} of {}",
            path.display()
        ),
    }
}

fn parse_book(value: &str, line: u32, path: &Path) -> Result<CanonBook> {
    CanonRegistry::by_id(value)
        .or_else(|| CanonRegistry::resolve(value))
        .ok_or_else(|| {
            anyhow!(
                "OSIS_INVALID_COORDINATE: unknown book `{value}` on line {line} of {}",
                path.display()
            )
        })
        .copied()
}

fn parse_chapter(value: &str, book: CanonBook, line: u32, path: &Path) -> Result<u16> {
    let mut parts = value.split('.');
    let book_id = parts.next().unwrap_or_default();
    let chapter = parts.next().unwrap_or_default();
    if parts.next().is_some() || !same_book(book_id, book) {
        bail!(
            "OSIS_INVALID_COORDINATE: invalid chapter ID `{value}` on line {line} of {}",
            path.display()
        );
    }
    let chapter = positive_number(chapter, "chapter", line, path)?;
    if VersificationRegistry::lookup(book.id, chapter, 1).is_none() {
        bail!(
            "OSIS_INVALID_COORDINATE: invalid chapter `{value}` on line {line} of {}",
            path.display()
        );
    }
    Ok(chapter)
}

fn parse_verse(
    value: &str,
    book: CanonBook,
    chapter_book: CanonBook,
    chapter: u16,
    line: u32,
    path: &Path,
) -> Result<u16> {
    if book.id != chapter_book.id {
        bail!(
            "OSIS_INVALID_COORDINATE: chapter book mismatch on line {line} of {}",
            path.display()
        );
    }
    let mut parts = value.split('.');
    let book_id = parts.next().unwrap_or_default();
    let chapter_id = parts.next().unwrap_or_default();
    let verse = parts.next().unwrap_or_default();
    if parts.next().is_some() || !same_book(book_id, book) || chapter_id != chapter.to_string() {
        bail!(
            "OSIS_INVALID_COORDINATE: invalid verse ID `{value}` on line {line} of {}",
            path.display()
        );
    }
    let verse = positive_number(verse, "verse", line, path)?;
    if VersificationRegistry::lookup(book.id, chapter, verse).is_none() {
        bail!(
            "OSIS_INVALID_COORDINATE: invalid verse `{value}` on line {line} of {}",
            path.display()
        );
    }
    Ok(verse)
}

fn evidence_unit(data: VerseData, path: &Path) -> EvidenceUnit {
    let components = vec![
        ReferenceComponent {
            level: "book".to_string(),
            value: data.book.name.to_string(),
            ordinal: Some(data.book.ordinal as u32),
        },
        ReferenceComponent {
            level: "chapter".to_string(),
            value: data.chapter.to_string(),
            ordinal: Some(data.chapter as u32),
        },
        ReferenceComponent {
            level: "verse".to_string(),
            value: data.verse.to_string(),
            ordinal: Some(data.verse as u32),
        },
    ];
    let normalized = components
        .iter()
        .map(|component| component.value.replace(' ', "").to_lowercase())
        .collect::<Vec<_>>()
        .join(":");
    let source_id = SourceId::from_path(path);
    let locator = CanonicalLocator {
        profile_id: "bible".to_string(),
        work_id: WORK_ID.to_string(),
        version_id: None,
        canon_id: Some(CANON_VERSION.to_string()),
        versification_id: Some(VERSIFICATION_VERSION.to_string()),
        start: components,
        end: None,
        display: format!("{} {}:{}", data.book.name, data.chapter, data.verse),
        normalized,
        backing_selectors: vec![
            BackingSelector::SourceNative {
                scheme: SOURCE_NATIVE_SCHEME.to_string(),
                value: data.osis_id,
            },
            BackingSelector::LineRange {
                start: data.line_start,
                end: data.line_end,
            },
        ],
    };
    EvidenceUnit {
        id: EvidenceId(format!("{}:osis:n0", source_id.0)),
        source_id,
        kind: EvidenceKind::Verse,
        derived_from: None,
        locator: SourceLocator::Canonical { locator },
        text_hash: hex_sha256(data.text.as_bytes()),
        text: data.text,
        heading_path: Vec::new(),
        language: None,
        position: 0,
        annotations: BTreeMap::new(),
    }
}

fn attributes(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    bytes: &[u8],
    position: usize,
    path: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| malformed_xml(bytes, position, path, error))?;
        let key = xml_name(attribute.key.as_ref(), bytes, position, path)?;
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|error| malformed_xml(bytes, position, path, error))?
            .into_owned();
        result.insert(key, value);
    }
    Ok(result)
}

fn ensure_attributes(
    attrs: &BTreeMap<String, String>,
    allowed: &[&str],
    line: u32,
    path: &Path,
) -> Result<()> {
    if let Some(key) = attrs
        .keys()
        .find(|key| !key.starts_with("xmlns") && !allowed.contains(&key.as_str()))
    {
        bail!(
            "OSIS_UNEXPECTED_STRUCTURE: unsupported attribute `{key}` on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

fn require_attr<'a>(
    attrs: &'a BTreeMap<String, String>,
    name: &str,
    line: u32,
    path: &Path,
) -> Result<&'a str> {
    attrs
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "OSIS_MISSING_ATTRIBUTE: missing `{name}` on line {line} of {}",
                path.display()
            )
        })
}

fn require_osis_id(attrs: &BTreeMap<String, String>, line: u32, path: &Path) -> Result<String> {
    let value = require_attr(attrs, "osisID", line, path)?;
    if value.split_whitespace().count() != 1 {
        bail!(
            "OSIS_INVALID_ID: invalid osisID `{value}` on line {line} of {}",
            path.display()
        );
    }
    Ok(value.to_string())
}

fn positive_number(value: &str, label: &str, line: u32, path: &Path) -> Result<u16> {
    let number = value.parse::<u16>().map_err(|_| {
        anyhow!(
            "OSIS_INVALID_COORDINATE: invalid {label} `{value}` on line {line} of {}",
            path.display()
        )
    })?;
    if number == 0 {
        bail!(
            "OSIS_INVALID_COORDINATE: {label} must be positive on line {line} of {}",
            path.display()
        );
    }
    Ok(number)
}

fn same_book(value: &str, book: CanonBook) -> bool {
    value.trim().eq_ignore_ascii_case(book.id) || value.trim().eq_ignore_ascii_case(book.name)
}

fn require_name(name: &str, expected: &str, line: u32, path: &Path) -> Result<()> {
    if name != expected {
        bail!(
            "OSIS_UNEXPECTED_STRUCTURE: expected `{expected}`, found `{name}` on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

fn reject_milestone(
    name: &str,
    attrs: &BTreeMap<String, String>,
    line: u32,
    path: &Path,
) -> Result<()> {
    if name == "milestone" || attrs.keys().any(|key| key == "sID" || key == "eID") {
        bail!(
            "OSIS_UNSUPPORTED_MILESTONE: milestone elements are unsupported on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

fn reject_declarations(bytes: &[u8], path: &Path) -> Result<()> {
    for needle in [b"<!doctype".as_slice(), b"<!entity".as_slice()] {
        let lowered = bytes.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
        if let Some(position) = lowered
            .windows(needle.len())
            .position(|window| window == needle)
        {
            let line = line_number(bytes, position);
            bail!(
                "OSIS_DOCTYPE_REJECTED: DTD and external entities are not permitted on line {line} of {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn xml_name(name: &[u8], bytes: &[u8], position: usize, path: &Path) -> Result<String> {
    String::from_utf8(name.to_vec())
        .map_err(|_| malformed_xml(bytes, position, path, "invalid XML name"))
}

fn malformed_xml<E: std::fmt::Display>(
    bytes: &[u8],
    position: usize,
    path: &Path,
    error: E,
) -> anyhow::Error {
    anyhow!(
        "OSIS_MALFORMED_XML: {error} on line {} of {}",
        line_number(bytes, position),
        path.display()
    )
}

fn line_number(bytes: &[u8], position: usize) -> u32 {
    bytes[..position.min(bytes.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u32
        + 1
}

#[cfg(test)]
mod tests {
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
}
