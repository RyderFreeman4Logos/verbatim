use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use quick_xml::escape::unescape;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::profiles::bible::canon_registry::{CanonBook, CanonRegistry, VERSION as CANON_VERSION};
use crate::profiles::bible::versification_registry::{
    VersificationRegistry, VERSION as VERSIFICATION_VERSION,
};
use crate::traits::Parser;
use crate::types::{
    hex_sha256, BackingSelector, CanonicalLocator, EvidenceId, EvidenceKind, EvidenceUnit,
    ReferenceComponent, SourceId, SourceLocator,
};

const SOURCE_NATIVE_SCHEME: &str = "usx";
const WORK_ID: &str = "USX";
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[path = "usx_xml.rs"]
mod usx_xml;
use usx_xml::{
    attributes, ensure_attributes, illegal_xml_10_character, line_number, malformed_xml,
    positive_number, reject_declarations, reject_illegal_xml_10_chars, require_attr,
    require_char_attributes, require_name, require_para_attributes, validate_declaration, xml_name,
};

/// A deliberately narrow USX 3.0 parser for canonical verse milestones.
pub struct UsxParser;

impl Parser for UsxParser {
    fn name(&self) -> &str {
        "usx"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["usx"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let file = fs::File::open(path)
            .with_context(|| format!("failed to read USX source {}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(MAX_SOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read USX source {}", path.display()))?;
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            bail!(
                "USX_SOURCE_TOO_LARGE: source exceeds {MAX_SOURCE_BYTES} bytes on line 1 of {}",
                path.display()
            );
        }
        parse_bytes(&bytes, path)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Element {
    Usx,
    Book,
    Para,
    Char,
}

impl Element {
    fn name(&self) -> &str {
        match self {
            Self::Usx => "usx",
            Self::Book => "book",
            Self::Para => "para",
            Self::Char => "char",
        }
    }
}

#[derive(Default)]
struct Counts {
    roots: usize,
    books: usize,
}

struct ChapterData {
    book: CanonBook,
    number: u16,
    sid: String,
}

struct VerseData {
    book: CanonBook,
    chapter: u16,
    verse: u16,
    sid: String,
    line_start: u32,
    line_end: u32,
    text: String,
}

struct OpenVerse {
    book: CanonBook,
    chapter: u16,
    verse: u16,
    sid: String,
    line_start: u32,
    text: String,
}

fn parse_bytes(bytes: &[u8], path: &Path) -> Result<Vec<EvidenceUnit>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| malformed_xml(bytes, error.valid_up_to(), path, error))?;
    if let Some((position, character)) = illegal_xml_10_character(text) {
        return Err(malformed_xml(
            bytes,
            position,
            path,
            format!("illegal XML 1.0 character U+{:04X}", character as u32),
        ));
    }
    reject_declarations(bytes, path)?;

    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut counts = Counts::default();
    let mut book = None;
    let mut chapter = None;
    let mut open_verse = None;
    let mut verses = Vec::new();
    let mut root_closed = false;
    let mut event_seen = false;

    loop {
        let event = reader.read_event_into(&mut buffer).map_err(|error| {
            malformed_xml(bytes, reader.buffer_position() as usize, path, error)
        })?;
        let position = reader.buffer_position() as usize;
        let line = line_number(bytes, position);
        let first_event = !event_seen;
        match event {
            Event::Decl(declaration) => {
                if !first_event {
                    bail!(
                        "USX_UNEXPECTED_STRUCTURE: XML declaration must be the first event on line {line} of {}",
                        path.display()
                    );
                }
                validate_declaration(&declaration, bytes, position, line, path)?;
            }
            Event::DocType(_) => {
                bail!(
                    "USX_DOCTYPE_REJECTED: DTD and external entities are not permitted on line {line} of {}",
                    path.display()
                );
            }
            Event::GeneralRef(reference) => {
                let reference = std::str::from_utf8(reference.as_ref())
                    .map_err(|error| malformed_xml(bytes, position, path, error))?;
                if reference.starts_with('#') {
                    let escaped = format!("&{reference};");
                    let expanded = unescape(&escaped)
                        .map_err(|error| malformed_xml(bytes, position, path, error))?;
                    reject_illegal_xml_10_chars(&expanded, bytes, position, path)?;
                }
                bail!(
                    "USX_ENTITY_REJECTED: entity reference `{reference}` is not permitted on line {line} of {}",
                    path.display()
                );
            }
            Event::Start(start) => {
                let name = xml_name(start.name().as_ref(), bytes, position, path)?;
                let attrs = attributes(&start, reader.decoder(), bytes, position, path)?;
                let element =
                    start_element(&stack, &name, &attrs, &mut counts, &mut book, line, path)?;
                if matches!(element, Element::Usx) {
                    root_closed = false;
                }
                stack.push(element);
            }
            Event::Empty(empty) => {
                let name = xml_name(empty.name().as_ref(), bytes, position, path)?;
                let attrs = attributes(&empty, reader.decoder(), bytes, position, path)?;
                handle_empty(
                    &stack,
                    &name,
                    &attrs,
                    &mut counts,
                    &mut book,
                    &mut chapter,
                    &mut open_verse,
                    &mut verses,
                    line,
                    path,
                )?;
            }
            Event::End(end) => {
                let name = xml_name(end.name().as_ref(), bytes, position, path)?;
                let element = stack.pop().ok_or_else(|| {
                    anyhow!(
                        "USX_MALFORMED_XML: unexpected end element `{name}` on line {line} of {}",
                        path.display()
                    )
                })?;
                if element.name() != name {
                    bail!(
                        "USX_MALFORMED_XML: end element `{name}` does not match `{}` on line {line} of {}",
                        element.name(),
                        path.display()
                    );
                }
                if matches!(element, Element::Usx) {
                    if chapter.is_some() || open_verse.is_some() {
                        bail!(
                            "USX_INCOMPLETE_ROOT: root closed before chapter and verse milestones on line {line} of {}",
                            path.display()
                        );
                    }
                    root_closed = true;
                }
            }
            Event::Text(text) => {
                let decoded = text
                    .decode()
                    .map_err(|error| malformed_xml(bytes, position, path, error))?;
                let decoded = unescape(&decoded)
                    .map_err(|error| malformed_xml(bytes, position, path, error))?;
                reject_illegal_xml_10_chars(&decoded, bytes, position, path)?;
                match stack.last() {
                    Some(Element::Book) => {}
                    Some(Element::Para) | Some(Element::Char) => {
                        if let Some(verse) = open_verse.as_mut() {
                            verse.text.push_str(&decoded);
                        }
                    }
                    Some(Element::Usx) => {
                        if !decoded.trim().is_empty() {
                            bail!(
                                "USX_UNEXPECTED_STRUCTURE: text outside a book, chapter, or paragraph on line {line} of {}",
                                path.display()
                            );
                        }
                    }
                    None => {
                        if !decoded.trim().is_empty() {
                            bail!(
                                "USX_UNEXPECTED_STRUCTURE: text after the root element on line {line} of {}",
                                path.display()
                            );
                        }
                    }
                }
            }
            Event::CData(_) | Event::Comment(_) | Event::PI(_) => {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: unsupported XML content on line {line} of {}",
                    path.display()
                );
            }
            Event::Eof => break,
        }
        event_seen = true;
        buffer.clear();
    }

    if counts.roots != 1 || !root_closed || !stack.is_empty() {
        bail!(
            "USX_MALFORMED_XML: expected one complete root on line {} of {}",
            line_number(bytes, bytes.len()),
            path.display()
        );
    }
    if chapter.is_some() {
        bail!(
            "USX_INCOMPLETE_CHAPTER: missing chapter end milestone on line {} of {}",
            line_number(bytes, bytes.len()),
            path.display()
        );
    }
    if open_verse.is_some() {
        bail!(
            "USX_INCOMPLETE_VERSE: missing verse end milestone on line {} of {}",
            line_number(bytes, bytes.len()),
            path.display()
        );
    }
    if counts.books != 1 || book.is_none() {
        bail!(
            "USX_MISSING_BOOK: expected one book element on line {} of {}",
            line_number(bytes, bytes.len()),
            path.display()
        );
    }
    if verses.is_empty() {
        bail!(
            "USX_MISSING_VERSE: expected at least one verse milestone on line {} of {}",
            line_number(bytes, bytes.len()),
            path.display()
        );
    }

    let units = verses
        .into_iter()
        .enumerate()
        .map(|(position, verse)| evidence_unit(verse, path, position))
        .collect();
    Ok(units)
}

fn start_element(
    stack: &[Element],
    name: &str,
    attrs: &BTreeMap<String, String>,
    counts: &mut Counts,
    book: &mut Option<CanonBook>,
    line: u32,
    path: &Path,
) -> Result<Element> {
    match stack {
        [] => {
            require_name(name, "usx", line, path)?;
            ensure_attributes(attrs, &["version"], line, path)?;
            if counts.roots != 0 {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: multiple roots are not permitted on line {line} of {}",
                    path.display()
                );
            }
            if require_attr(attrs, "version", line, path)? != "3.0" {
                bail!(
                    "USX_UNSUPPORTED_VERSION: only USX version 3.0 is supported on line {line} of {}",
                    path.display()
                );
            }
            counts.roots += 1;
            Ok(Element::Usx)
        }
        [Element::Usx] if name == "book" => {
            ensure_attributes(attrs, &["code", "style"], line, path)?;
            if counts.books != 0 || book.is_some() {
                bail!(
                    "USX_DUPLICATE_BOOK: only one book element is supported on line {line} of {}",
                    path.display()
                );
            }
            if require_attr(attrs, "style", line, path)? != "id" {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: book element requires style=id on line {line} of {}",
                    path.display()
                );
            }
            let code = require_attr(attrs, "code", line, path)?;
            let parsed = parse_book(code, line, path)?;
            *book = Some(parsed);
            counts.books += 1;
            Ok(Element::Book)
        }
        [Element::Usx] if name == "para" => {
            require_para_attributes(attrs, line, path)?;
            if book.is_none() {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: paragraph precedes book on line {line} of {}",
                    path.display()
                );
            }
            Ok(Element::Para)
        }
        [Element::Usx, Element::Para] if name == "char" => {
            require_char_attributes(attrs, line, path)?;
            Ok(Element::Char)
        }
        _ => bail!(
            "USX_UNEXPECTED_STRUCTURE: unsupported element `{name}` on line {line} of {}",
            path.display()
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_empty(
    stack: &[Element],
    name: &str,
    attrs: &BTreeMap<String, String>,
    counts: &mut Counts,
    book: &mut Option<CanonBook>,
    chapter: &mut Option<ChapterData>,
    open_verse: &mut Option<OpenVerse>,
    verses: &mut Vec<VerseData>,
    line: u32,
    path: &Path,
) -> Result<()> {
    match name {
        "usx" => bail!(
            "USX_UNEXPECTED_STRUCTURE: root must be a complete element on line {line} of {}",
            path.display()
        ),
        "book" => {
            if stack != [Element::Usx] {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: book is not a direct child of usx on line {line} of {}",
                    path.display()
                );
            }
            ensure_attributes(attrs, &["code", "style"], line, path)?;
            if counts.books != 0 || book.is_some() {
                bail!(
                    "USX_DUPLICATE_BOOK: only one book element is supported on line {line} of {}",
                    path.display()
                );
            }
            if require_attr(attrs, "style", line, path)? != "id" {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: book element requires style=id on line {line} of {}",
                    path.display()
                );
            }
            *book = Some(parse_book(
                require_attr(attrs, "code", line, path)?,
                line,
                path,
            )?);
            counts.books += 1;
            Ok(())
        }
        "para" => {
            if stack != [Element::Usx] {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: paragraph is not a direct child of usx on line {line} of {}",
                    path.display()
                );
            }
            require_para_attributes(attrs, line, path)
        }
        "char" => {
            if stack != [Element::Usx, Element::Para] {
                bail!(
                    "USX_UNEXPECTED_STRUCTURE: char is not inside a paragraph on line {line} of {}",
                    path.display()
                );
            }
            require_char_attributes(attrs, line, path)
        }
        "chapter" => handle_chapter(stack, attrs, book, chapter, open_verse, line, path),
        "verse" => handle_verse(stack, attrs, chapter, open_verse, verses, line, path),
        _ => bail!(
            "USX_UNKNOWN_MARKER: unsupported element `{name}` on line {line} of {}",
            path.display()
        ),
    }
}

fn handle_chapter(
    stack: &[Element],
    attrs: &BTreeMap<String, String>,
    book: &Option<CanonBook>,
    chapter: &mut Option<ChapterData>,
    open_verse: &mut Option<OpenVerse>,
    line: u32,
    path: &Path,
) -> Result<()> {
    if stack != [Element::Usx] {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: chapter milestones must be direct children of usx on line {line} of {}",
            path.display()
        );
    }
    if let Some(eid) = attrs.get("eid") {
        ensure_attributes(attrs, &["eid"], line, path)?;
        if chapter.is_none() || open_verse.is_some() {
            bail!(
                "USX_INVALID_COORDINATE: chapter end is out of order on line {line} of {}",
                path.display()
            );
        }
        let current = chapter.take().ok_or_else(|| {
            anyhow!(
                "USX_INVALID_COORDINATE: chapter end has no matching start on line {line} of {}",
                path.display()
            )
        })?;
        if eid != &current.sid {
            bail!(
                "USX_INVALID_COORDINATE: chapter end `{eid}` does not match `{}` on line {line} of {}",
                current.sid,
                path.display()
            );
        }
        return Ok(());
    }

    ensure_attributes(
        attrs,
        &["number", "style", "altnumber", "pubnumber", "sid"],
        line,
        path,
    )?;
    if chapter.is_some() || open_verse.is_some() {
        bail!(
            "USX_INVALID_COORDINATE: chapter start is out of order on line {line} of {}",
            path.display()
        );
    }
    let book = book.ok_or_else(|| {
        anyhow!(
            "USX_UNEXPECTED_STRUCTURE: chapter precedes book on line {line} of {}",
            path.display()
        )
    })?;
    if require_attr(attrs, "style", line, path)? != "c" {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: chapter element requires style=c on line {line} of {}",
            path.display()
        );
    }
    let number = positive_number(
        require_attr(attrs, "number", line, path)?,
        "chapter",
        line,
        path,
    )?;
    let sid = require_attr(attrs, "sid", line, path)?.to_string();
    validate_reference(&sid, book, number, None, line, path)?;
    if VersificationRegistry::lookup(book.id, number, 1).is_none() {
        bail!(
            "USX_INVALID_COORDINATE: invalid chapter `{sid}` on line {line} of {}",
            path.display()
        );
    }
    *chapter = Some(ChapterData { book, number, sid });
    Ok(())
}

fn handle_verse(
    stack: &[Element],
    attrs: &BTreeMap<String, String>,
    chapter: &Option<ChapterData>,
    open_verse: &mut Option<OpenVerse>,
    verses: &mut Vec<VerseData>,
    line: u32,
    path: &Path,
) -> Result<()> {
    if !matches!(stack.last(), Some(Element::Para) | Some(Element::Char)) {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: verse milestones must be inside a paragraph on line {line} of {}",
            path.display()
        );
    }
    if let Some(eid) = attrs.get("eid") {
        ensure_attributes(attrs, &["eid"], line, path)?;
        if stack.last() != Some(&Element::Para) {
            bail!(
                "USX_UNEXPECTED_STRUCTURE: verse end must follow character content on line {line} of {}",
                path.display()
            );
        }
        let current = open_verse.take().ok_or_else(|| {
            anyhow!(
                "USX_INVALID_COORDINATE: verse end has no matching start on line {line} of {}",
                path.display()
            )
        })?;
        if eid != &current.sid {
            bail!(
                "USX_INVALID_COORDINATE: verse end `{eid}` does not match `{}` on line {line} of {}",
                current.sid,
                path.display()
            );
        }
        let text = current.text.trim().to_owned();
        if text.is_empty() {
            bail!(
                "USX_EMPTY_VERSE: verse `{}` has no text on line {line} of {}",
                current.sid,
                path.display()
            );
        }
        verses.push(VerseData {
            book: current.book,
            chapter: current.chapter,
            verse: current.verse,
            sid: current.sid,
            line_start: current.line_start,
            line_end: line,
            text,
        });
        return Ok(());
    }

    ensure_attributes(
        attrs,
        &["number", "style", "altnumber", "pubnumber", "sid"],
        line,
        path,
    )?;
    let chapter = chapter.as_ref().ok_or_else(|| {
        anyhow!(
            "USX_INVALID_COORDINATE: verse precedes chapter on line {line} of {}",
            path.display()
        )
    })?;
    if open_verse.is_some() {
        bail!(
            "USX_INVALID_COORDINATE: verse start is out of order on line {line} of {}",
            path.display()
        );
    }
    if require_attr(attrs, "style", line, path)? != "v" {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: verse element requires style=v on line {line} of {}",
            path.display()
        );
    }
    let number = positive_number(
        require_attr(attrs, "number", line, path)?,
        "verse",
        line,
        path,
    )?;
    let sid = require_attr(attrs, "sid", line, path)?.to_string();
    validate_reference(&sid, chapter.book, chapter.number, Some(number), line, path)?;
    if VersificationRegistry::lookup(chapter.book.id, chapter.number, number).is_none() {
        bail!(
            "USX_INVALID_COORDINATE: invalid verse `{sid}` on line {line} of {}",
            path.display()
        );
    }
    *open_verse = Some(OpenVerse {
        book: chapter.book,
        chapter: chapter.number,
        verse: number,
        sid,
        line_start: line,
        text: String::new(),
    });
    Ok(())
}

fn parse_book(value: &str, line: u32, path: &Path) -> Result<CanonBook> {
    CanonRegistry::by_id(value)
        .or_else(|| CanonRegistry::resolve(value))
        .ok_or_else(|| {
            anyhow!(
                "USX_INVALID_COORDINATE: unknown book `{value}` on line {line} of {}",
                path.display()
            )
        })
        .copied()
}

fn validate_reference(
    value: &str,
    book: CanonBook,
    chapter: u16,
    verse: Option<u16>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let mut parts = value.split_whitespace();
    let reference_book = parts.next().unwrap_or_default();
    let coordinate = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || !reference_book.eq_ignore_ascii_case(book.id)
        || coordinate.is_empty()
    {
        bail!(
            "USX_INVALID_COORDINATE: invalid reference `{value}` on line {line} of {}",
            path.display()
        );
    }
    let mut numbers = coordinate.split(':');
    let reference_chapter = numbers.next().unwrap_or_default();
    let reference_verse = numbers.next();
    if numbers.next().is_some() || reference_chapter != chapter.to_string() {
        bail!(
            "USX_INVALID_COORDINATE: invalid reference `{value}` on line {line} of {}",
            path.display()
        );
    }
    match (verse, reference_verse) {
        (None, None) => Ok(()),
        (Some(expected), Some(actual)) if actual == expected.to_string() => Ok(()),
        _ => bail!(
            "USX_INVALID_COORDINATE: invalid reference `{value}` on line {line} of {}",
            path.display()
        ),
    }
}

fn evidence_unit(data: VerseData, path: &Path, position: usize) -> EvidenceUnit {
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
                value: data.sid,
            },
            BackingSelector::LineRange {
                start: data.line_start,
                end: data.line_end,
            },
        ],
    };
    EvidenceUnit {
        id: EvidenceId(format!("{}:usx:n{position}", source_id.0)),
        source_id,
        kind: EvidenceKind::Verse,
        derived_from: None,
        locator: SourceLocator::Canonical { locator },
        text_hash: hex_sha256(data.text.as_bytes()),
        text: data.text,
        heading_path: Vec::new(),
        language: None,
        position: position as u32,
        annotations: BTreeMap::new(),
    }
}

#[cfg(test)]
#[path = "usx_tests.rs"]
mod tests;
