use super::*;

const FOOTNOTE_TEXT_MARKERS: &[&str] = &["ft", "fr", "fq", "fqa", "fk", "fl", "fw", "fv", "fp"];
const CROSS_REFERENCE_TEXT_MARKERS: &[&str] = &["xt", "xo", "xk", "xq", "xdc", "xot", "xnt"];

pub(super) fn start_note(
    stack: &[Element],
    attrs: &BTreeMap<String, String>,
    open_verse: Option<&OpenVerse>,
    open_note: &mut Option<OpenNote>,
    line: u32,
    path: &Path,
) -> Result<()> {
    if stack.last() != Some(&Element::Para) {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: note must be inside a paragraph on line {line} of {}",
            path.display()
        );
    }
    if open_verse.is_none() {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: note precedes verse on line {line} of {}",
            path.display()
        );
    }
    if open_note.is_some() {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: nested note on line {line} of {}",
            path.display()
        );
    }
    ensure_attributes(attrs, &["caller", "style"], line, path)?;
    let style = require_attr(attrs, "style", line, path)?;
    let (note_type, allowed_markers) = match style {
        "f" => ("footnote", FOOTNOTE_TEXT_MARKERS),
        "fe" => ("endnote", FOOTNOTE_TEXT_MARKERS),
        "x" => ("cross_reference", CROSS_REFERENCE_TEXT_MARKERS),
        _ => bail!(
            "USX_UNKNOWN_MARKER: unsupported note style `{style}` on line {line} of {}",
            path.display()
        ),
    };
    *open_note = Some(OpenNote {
        note_type,
        allowed_markers,
        char_style: None,
        text: String::new(),
        line,
    });
    Ok(())
}

pub(super) fn start_note_char(
    attrs: &BTreeMap<String, String>,
    open_note: Option<&mut OpenNote>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let note = open_note.ok_or_else(|| {
        anyhow!(
            "USX_UNEXPECTED_STRUCTURE: note character content has no open note on line {line} of {}",
            path.display()
        )
    })?;
    require_char_attributes(attrs, line, path)?;
    let style = require_attr(attrs, "style", line, path)?;
    if !note.allowed_markers.contains(&style) {
        bail!(
            "USX_UNKNOWN_MARKER: unknown marker `{style}` on line {line} of {}",
            path.display()
        );
    }
    note.char_style = Some(style.to_string());
    Ok(())
}

pub(super) fn append_note_text(note: &mut OpenNote, decoded: &str) {
    let keep = match note.char_style.as_deref() {
        Some(style) => match note.note_type {
            "cross_reference" => style == "xt",
            _ => matches!(style, "ft" | "fq" | "fqa" | "fk" | "fw"),
        },
        None => true,
    };
    if keep {
        note.text.push_str(decoded);
    }
}

pub(super) fn close_note(
    open_verse: &mut Option<OpenVerse>,
    open_note: &mut Option<OpenNote>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let note = open_note.take().ok_or_else(|| {
        anyhow!(
            "USX_MALFORMED_XML: unexpected end element `note` on line {line} of {}",
            path.display()
        )
    })?;
    let verse = open_verse.as_mut().ok_or_else(|| {
        anyhow!(
            "USX_UNEXPECTED_STRUCTURE: note has no matching verse on line {line} of {}",
            path.display()
        )
    })?;
    let text = note.text.trim().to_string();
    if text.is_empty() {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: note requires note text on line {line} of {}",
            path.display()
        );
    }
    verse.notes.push(ParsedNote {
        note_type: note.note_type,
        text,
        line: note.line,
    });
    Ok(())
}

pub(super) fn evidence_units(
    data: VerseData,
    path: &Path,
    position: &mut usize,
) -> Vec<EvidenceUnit> {
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
    let verse_position = *position;
    *position += 1;
    let verse_id = EvidenceId(format!("{}:usx:n{verse_position}", source_id.0));
    let mut units = vec![EvidenceUnit {
        id: verse_id.clone(),
        source_id: source_id.clone(),
        kind: EvidenceKind::Verse,
        derived_from: None,
        locator: SourceLocator::Canonical {
            locator: locator.clone(),
        },
        text_hash: hex_sha256(data.text.as_bytes()),
        text: data.text,
        heading_path: Vec::new(),
        language: None,
        position: verse_position as u32,
        annotations: BTreeMap::new(),
    }];
    for note in data.notes {
        let note_position = *position;
        *position += 1;
        let mut note_locator = locator.clone();
        note_locator.display = format!("{} note {}", note_locator.display, note_position);
        note_locator.backing_selectors = vec![BackingSelector::LineRange {
            start: note.line,
            end: note.line,
        }];
        let mut annotations = BTreeMap::new();
        annotations.insert("note_type".to_string(), note.note_type.to_string());
        units.push(EvidenceUnit {
            id: EvidenceId(format!("{}:usx:n{note_position}", source_id.0)),
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
    units
}
