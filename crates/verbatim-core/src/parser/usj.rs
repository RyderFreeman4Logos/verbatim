use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};

use super::read_bounded_source;

use crate::profiles::bible::canon_registry::{CanonBook, CanonRegistry, VERSION as CANON_VERSION};
use crate::profiles::bible::versification_registry::{
    VersificationRegistry, VERSION as VERSIFICATION_VERSION,
};
use crate::traits::Parser;
use crate::types::{
    hex_sha256, BackingSelector, CanonicalLocator, EvidenceId, EvidenceKind, EvidenceUnit,
    ReferenceComponent, SourceId, SourceLocator,
};

const SOURCE_NATIVE_SCHEME: &str = "usj";
const WORK_ID: &str = "USJ";
const VERSION: &str = "3.1";

/// A deliberately narrow USJ 3.1 parser for canonical verse content.
pub struct UsjParser;

impl Parser for UsjParser {
    fn name(&self) -> &str {
        "usj"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["usj"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let file = fs::File::open(path)
            .with_context(|| format!("failed to read USJ source {}", path.display()))?;
        let bytes = read_bounded_source(file, "USJ", path)?;
        parse_bytes(&bytes, path)
    }
}

#[derive(Debug)]
struct VerseData {
    book: CanonBook,
    chapter: u16,
    verse: u16,
    sid: String,
    source_span: SourceSpan,
    text: String,
    notes: Vec<ParsedNote>,
}

#[path = "usj_spans.rs"]
mod usj_spans;
use usj_spans::{line_at, SourceScan, SourceSpan};

#[path = "usj_notes.rs"]
mod usj_notes;
use usj_notes::{parse_note, ParsedNote};

fn parse_bytes(bytes: &[u8], path: &Path) -> Result<Vec<EvidenceUnit>> {
    let source = std::str::from_utf8(bytes).map_err(|error| {
        diagnostic(
            "USJ_INVALID_UTF8",
            error.to_string(),
            line_at_bytes(bytes, error.valid_up_to()),
            path,
        )
    })?;
    let document: Value = serde_json::from_str(source).map_err(|error| {
        diagnostic(
            "USJ_INVALID_JSON",
            error.to_string(),
            error.line().max(1) as u32,
            path,
        )
    })?;
    let root = document
        .as_object()
        .ok_or_else(|| diagnostic("USJ_INCOMPLETE_ROOT", "root must be an object", 1, path))?;
    let scan = SourceScan::new(source, &document, path)?;
    let root_line = scan.node_line(&document, path)?;
    if require_string(root, "type", "USJ_INCOMPLETE_ROOT", root_line, path)? != "USJ" {
        return Err(diagnostic(
            "USJ_INCOMPLETE_ROOT",
            "root type must be USJ",
            root_line,
            path,
        ));
    }
    let version = require_string(root, "version", "USJ_INCOMPLETE_ROOT", root_line, path)?;
    if version != VERSION {
        bail!(
            "USJ_UNSUPPORTED_VERSION: only USJ version {VERSION} is supported on line {root_line} of {}",
            path.display()
        );
    }
    let content = root.get("content").ok_or_else(|| {
        diagnostic(
            "USJ_INCOMPLETE_ROOT",
            "root content is required",
            root_line,
            path,
        )
    })?;
    let content = content.as_array().ok_or_else(|| {
        diagnostic(
            "USJ_INCOMPLETE_ROOT",
            "root content must be an array",
            root_line,
            path,
        )
    })?;

    let mut book = None;
    let mut chapter = None;
    let mut verses = Vec::new();
    let mut seen_verses = BTreeSet::new();
    for node in content {
        let line = scan.node_line(node, path)?;
        let object = node.as_object().ok_or_else(|| {
            diagnostic(
                "USJ_UNEXPECTED_STRUCTURE",
                "root content entries must be objects",
                line,
                path,
            )
        })?;
        let node_type = node_type(object).unwrap_or("<missing>");
        match node_type {
            "book" => parse_book(object, &mut book, line, &scan, path)?,
            "chapter" => {
                let parsed = parse_chapter(object, book, line, path)?;
                chapter = Some(parsed);
            }
            "para" => parse_para(
                object,
                book,
                chapter,
                &mut verses,
                &mut seen_verses,
                &scan,
                line,
                path,
            )?,
            _ => {
                bail!(
                    "USJ_UNKNOWN_CRITICAL: unsupported node type `{node_type}` on line {line} of {}",
                    path.display()
                );
            }
        }
    }

    if book.is_none() || chapter.is_none() || verses.is_empty() {
        bail!(
            "USJ_INCOMPLETE_ROOT: expected one book, chapter, and verse on line {} of {}",
            line_at(source, source.len()),
            path.display()
        );
    }

    let mut units = Vec::new();
    let mut position = 0;
    for verse in verses {
        units.extend(evidence_units(verse, &scan, path, &mut position)?);
    }
    Ok(units)
}

fn parse_book(
    object: &Map<String, Value>,
    book: &mut Option<CanonBook>,
    line: u32,
    scan: &SourceScan<'_>,
    path: &Path,
) -> Result<()> {
    if book.is_some() {
        bail!(
            "USJ_UNEXPECTED_STRUCTURE: duplicate book node on line {line} of {}",
            path.display()
        );
    }
    if require_string(object, "marker", "USJ_INCOMPLETE_NODE", line, path)? != "id" {
        bail!(
            "USJ_INVALID_COORDINATE: book marker must be id on line {line} of {}",
            path.display()
        );
    }
    let code = require_string(object, "code", "USJ_INCOMPLETE_NODE", line, path)?;
    if code.len() != 3
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        bail!(
            "USJ_INVALID_COORDINATE: invalid book code `{code}` on line {line} of {}",
            path.display()
        );
    }
    if let Some(content) = object.get("content") {
        let content = content.as_array().ok_or_else(|| {
            diagnostic(
                "USJ_INCOMPLETE_NODE",
                "book content must be an array",
                line,
                path,
            )
        })?;
        for item in content {
            if let Value::Object(node) = item {
                let node_type = node_type(node).unwrap_or("<missing>");
                let node_line = scan.node_line(item, path)?;
                bail!(
                    "USJ_UNKNOWN_CRITICAL: unsupported book node type `{node_type}` on line {node_line} of {}",
                    path.display()
                );
            } else if !item.is_string() {
                bail!(
                    "USJ_UNEXPECTED_STRUCTURE: book content must contain strings on line {line} of {}",
                    path.display()
                );
            }
        }
    }
    *book = Some(*CanonRegistry::by_id(code).ok_or_else(|| {
        diagnostic(
            "USJ_INVALID_COORDINATE",
            format!("unknown book {code}"),
            line,
            path,
        )
    })?);
    Ok(())
}

fn parse_chapter(
    object: &Map<String, Value>,
    book: Option<CanonBook>,
    line: u32,
    path: &Path,
) -> Result<(CanonBook, u16)> {
    let book =
        book.ok_or_else(|| diagnostic("USJ_INCOMPLETE_ROOT", "chapter precedes book", line, path))?;
    if require_string(object, "marker", "USJ_INCOMPLETE_NODE", line, path)? != "c" {
        bail!(
            "USJ_INVALID_COORDINATE: chapter marker must be c on line {line} of {}",
            path.display()
        );
    }
    let number = parse_number(
        require_string(object, "number", "USJ_INCOMPLETE_NODE", line, path)?,
        "chapter",
        line,
        path,
    )?;
    let sid = require_string(object, "sid", "USJ_INCOMPLETE_NODE", line, path)?;
    let expected = format!("{} {number}", book.id);
    if sid != expected || VersificationRegistry::lookup(book.id, number, 1).is_none() {
        bail!(
            "USJ_INVALID_COORDINATE: invalid chapter sid `{sid}` on line {line} of {}",
            path.display()
        );
    }
    Ok((book, number))
}

#[allow(clippy::too_many_arguments)]
fn parse_para(
    object: &Map<String, Value>,
    book: Option<CanonBook>,
    chapter: Option<(CanonBook, u16)>,
    verses: &mut Vec<VerseData>,
    seen_verses: &mut BTreeSet<String>,
    scan: &SourceScan<'_>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let content = object.get("content").ok_or_else(|| {
        diagnostic(
            "USJ_INCOMPLETE_NODE",
            "paragraph content is required",
            line,
            path,
        )
    })?;
    let content = content.as_array().ok_or_else(|| {
        diagnostic(
            "USJ_INCOMPLETE_NODE",
            "paragraph content must be an array",
            line,
            path,
        )
    })?;
    let mut open: Option<VerseData> = None;
    for item in content {
        match item {
            Value::String(text) => {
                if let Some(verse) = open.as_mut() {
                    verse.text.push_str(text);
                    scan.extend(&mut verse.source_span, item, line, path)?;
                } else {
                    let text_line = scan.node_line(item, path)?;
                    bail!(
                        "USJ_UNEXPECTED_STRUCTURE: text is outside a verse on line {text_line} of {}",
                        path.display()
                    );
                }
            }
            Value::Object(node) => {
                let node_type = node_type(node).unwrap_or("<missing>");
                let node_line = scan.node_line(item, path)?;
                match node_type {
                    "verse" => {
                        if let Some(verse) = open.take() {
                            finish_verse(verse, verses, node_line, path)?;
                        }
                        let verse = start_verse(item, book, chapter, node_line, scan, path)?;
                        if !seen_verses.insert(verse.sid.clone()) {
                            bail!(
                                "USJ_DUPLICATE_VERSE: duplicate verse `{}` on line {node_line} of {}",
                                verse.sid,
                                path.display()
                            );
                        }
                        open = Some(verse);
                        if let Some(content) = node.get("content") {
                            let content = content.as_array().ok_or_else(|| {
                                diagnostic(
                                    "USJ_INCOMPLETE_NODE",
                                    "verse content must be an array",
                                    node_line,
                                    path,
                                )
                            })?;
                            for item in content {
                                append_content(item, &mut open, scan, node_line, path)?;
                            }
                        }
                    }
                    "char" => append_char(item, &mut open, scan, node_line, path)?,
                    "note" => parse_note(item, &mut open, scan, node_line, path)?,
                    _ => {
                        bail!(
                            "USJ_UNKNOWN_CRITICAL: unsupported node type `{node_type}` on line {node_line} of {}",
                            path.display()
                        );
                    }
                }
            }
            _ => {
                bail!(
                    "USJ_UNEXPECTED_STRUCTURE: paragraph content must contain strings or nodes on line {line} of {}",
                    path.display()
                );
            }
        }
    }
    if let Some(verse) = open {
        finish_verse(verse, verses, line, path)?;
    }
    Ok(())
}

fn append_content(
    item: &Value,
    open: &mut Option<VerseData>,
    scan: &SourceScan<'_>,
    line: u32,
    path: &Path,
) -> Result<()> {
    match item {
        Value::String(text) => {
            if let Some(verse) = open.as_mut() {
                verse.text.push_str(text);
                scan.extend(&mut verse.source_span, item, line, path)?;
            } else {
                let text_line = scan.node_line(item, path)?;
                bail!(
                    "USJ_UNEXPECTED_STRUCTURE: text is outside a verse on line {text_line} of {}",
                    path.display()
                );
            }
            Ok(())
        }
        Value::Object(node) if node_type(node) == Some("char") => {
            append_char(item, open, scan, line, path)
        }
        Value::Object(node) if node_type(node) == Some("note") => {
            parse_note(item, open, scan, line, path)
        }
        Value::Object(node) => {
            let node_type = node_type(node).unwrap_or("<missing>");
            bail!(
                "USJ_UNKNOWN_CRITICAL: unsupported node type `{node_type}` on line {line} of {}",
                path.display()
            )
        }
        _ => bail!(
            "USJ_UNEXPECTED_STRUCTURE: node content must contain strings or nodes on line {line} of {}",
            path.display()
        ),
    }
}

fn append_char(
    node_value: &Value,
    open: &mut Option<VerseData>,
    scan: &SourceScan<'_>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let node = node_value.as_object().ok_or_else(|| {
        diagnostic(
            "USJ_UNEXPECTED_STRUCTURE",
            "character node must be an object",
            line,
            path,
        )
    })?;
    require_string(node, "marker", "USJ_INCOMPLETE_NODE", line, path)?;
    let content = node.get("content").ok_or_else(|| {
        diagnostic(
            "USJ_INCOMPLETE_NODE",
            "character content is required",
            line,
            path,
        )
    })?;
    let content = content.as_array().ok_or_else(|| {
        diagnostic(
            "USJ_INCOMPLETE_NODE",
            "character content must be an array",
            line,
            path,
        )
    })?;
    for item in content {
        append_content(item, open, scan, line, path)?;
    }
    if let Some(verse) = open.as_mut() {
        scan.extend(&mut verse.source_span, node_value, line, path)?;
    }
    Ok(())
}

fn start_verse(
    node: &Value,
    book: Option<CanonBook>,
    chapter: Option<(CanonBook, u16)>,
    line: u32,
    scan: &SourceScan<'_>,
    path: &Path,
) -> Result<VerseData> {
    let object = node.as_object().ok_or_else(|| {
        diagnostic(
            "USJ_UNEXPECTED_STRUCTURE",
            "verse node must be an object",
            line,
            path,
        )
    })?;
    let book =
        book.ok_or_else(|| diagnostic("USJ_INCOMPLETE_ROOT", "verse precedes book", line, path))?;
    let (chapter_book, chapter_number) = chapter
        .ok_or_else(|| diagnostic("USJ_INCOMPLETE_ROOT", "verse precedes chapter", line, path))?;
    if chapter_book.id != book.id {
        bail!(
            "USJ_INVALID_COORDINATE: chapter book does not match verse book on line {line} of {}",
            path.display()
        );
    }
    if require_string(object, "marker", "USJ_INCOMPLETE_NODE", line, path)? != "v" {
        bail!(
            "USJ_INVALID_COORDINATE: verse marker must be v on line {line} of {}",
            path.display()
        );
    }
    let verse = parse_number(
        require_string(object, "number", "USJ_INCOMPLETE_NODE", line, path)?,
        "verse",
        line,
        path,
    )?;
    let sid = require_string(object, "sid", "USJ_INCOMPLETE_NODE", line, path)?;
    let expected = format!("{} {}:{verse}", book.id, chapter_number);
    if sid != expected || VersificationRegistry::lookup(book.id, chapter_number, verse).is_none() {
        bail!(
            "USJ_INVALID_COORDINATE: invalid verse sid `{sid}` on line {line} of {}",
            path.display()
        );
    }
    Ok(VerseData {
        book,
        chapter: chapter_number,
        verse,
        sid: sid.to_string(),
        source_span: scan.span(node, line, path)?,
        text: String::new(),
        notes: Vec::new(),
    })
}

fn finish_verse(
    mut verse: VerseData,
    verses: &mut Vec<VerseData>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let text = verse.text.trim().to_string();
    if text.is_empty() {
        bail!(
            "USJ_INCOMPLETE_NODE: verse has no text on line {line} of {}",
            path.display()
        );
    }
    verse.text = text;
    verses.push(verse);
    Ok(())
}

fn evidence_units(
    verse: VerseData,
    scan: &SourceScan<'_>,
    path: &Path,
    position: &mut usize,
) -> Result<Vec<EvidenceUnit>> {
    let source_id = SourceId::from_path(path);
    let components = vec![
        ReferenceComponent {
            level: "book".to_string(),
            value: verse.book.name.to_string(),
            ordinal: Some(verse.book.ordinal as u32),
        },
        ReferenceComponent {
            level: "chapter".to_string(),
            value: verse.chapter.to_string(),
            ordinal: Some(verse.chapter as u32),
        },
        ReferenceComponent {
            level: "verse".to_string(),
            value: verse.verse.to_string(),
            ordinal: Some(verse.verse as u32),
        },
    ];
    let normalized = components
        .iter()
        .map(|component| component.value.replace(' ', "").to_lowercase())
        .collect::<Vec<_>>()
        .join(":");
    let mut locator = CanonicalLocator::single_unit(
        "bible",
        WORK_ID,
        components,
        format!("{} {}:{}", verse.book.name, verse.chapter, verse.verse),
        normalized,
    );
    locator.canon_id = Some(CANON_VERSION.to_string());
    locator.versification_id = Some(VERSIFICATION_VERSION.to_string());
    let (line_start, line_end) = scan.line_range(verse.source_span, path)?;
    locator.backing_selectors = vec![
        BackingSelector::SourceNative {
            scheme: SOURCE_NATIVE_SCHEME.to_string(),
            value: verse.sid,
        },
        BackingSelector::LineRange {
            start: line_start,
            end: line_end,
        },
    ];
    let verse_position = *position;
    *position += 1;
    let verse_id = EvidenceId(format!("{}:usj:n{verse_position}", source_id.0));
    let verse_text = verse.text;
    let mut units = vec![EvidenceUnit {
        id: verse_id.clone(),
        source_id: source_id.clone(),
        kind: EvidenceKind::Verse,
        derived_from: None,
        locator: SourceLocator::Canonical {
            locator: locator.clone(),
        },
        text_hash: hex_sha256(verse_text.as_bytes()),
        text: verse_text,
        heading_path: Vec::new(),
        language: None,
        position: verse_position as u32,
        annotations: BTreeMap::new(),
    }];
    for note in verse.notes {
        let note_position = *position;
        *position += 1;
        let (note_line_start, note_line_end) = scan.line_range(note.source_span, path)?;
        let mut note_locator = locator.clone();
        note_locator.display = format!("{} note {note_position}", note_locator.display);
        note_locator.backing_selectors = vec![BackingSelector::LineRange {
            start: note_line_start,
            end: note_line_end,
        }];
        let mut annotations = BTreeMap::new();
        annotations.insert("note_type".to_string(), note.note_type.to_string());
        units.push(EvidenceUnit {
            id: EvidenceId(format!("{}:usj:n{note_position}", source_id.0)),
            source_id: source_id.clone(),
            kind: EvidenceKind::Footnote,
            derived_from: Some(verse_id.clone()),
            locator: SourceLocator::Canonical {
                locator: note_locator,
            },
            text_hash: hex_sha256(note.text.as_bytes()),
            text: note.text,
            heading_path: Vec::new(),
            language: None,
            position: note_position as u32,
            annotations,
        });
    }
    Ok(units)
}

fn node_type(object: &Map<String, Value>) -> Option<&str> {
    object.get("type").and_then(Value::as_str)
}

fn require_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    code: &str,
    line: u32,
    path: &Path,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| diagnostic(code, format!("missing string field `{field}`"), line, path))
}

fn parse_number(value: &str, kind: &str, line: u32, path: &Path) -> Result<u16> {
    let number = value.parse::<u16>().map_err(|_| {
        diagnostic(
            "USJ_INVALID_COORDINATE",
            format!("{kind} number `{value}` is not numeric"),
            line,
            path,
        )
    })?;
    if number == 0 {
        bail!(
            "USJ_INVALID_COORDINATE: {kind} number must be positive on line {line} of {}",
            path.display()
        );
    }
    Ok(number)
}

fn diagnostic(code: &str, detail: impl std::fmt::Display, line: u32, path: &Path) -> anyhow::Error {
    anyhow!("{code}: {detail} on line {line} of {}", path.display())
}

fn line_at_bytes(source: &[u8], offset: usize) -> u32 {
    source[..offset.min(source.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u32
        + 1
}

#[cfg(test)]
#[path = "usj_tests.rs"]
mod tests;
