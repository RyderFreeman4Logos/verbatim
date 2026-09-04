use std::path::Path;

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::usj_spans::{SourceScan, SourceSpan};
use super::VerseData;

const FOOTNOTE_TEXT_MARKERS: &[&str] = &["ft", "fr", "fq", "fqa", "fk", "fl", "fw", "fv", "fp"];
const CROSS_REFERENCE_TEXT_MARKERS: &[&str] = &["xt", "xo", "xk", "xq", "xdc", "xot", "xnt"];

#[derive(Debug)]
pub(super) struct ParsedNote {
    pub(super) note_type: &'static str,
    pub(super) text: String,
    pub(super) source_span: SourceSpan,
}

pub(super) fn parse_note(
    node_value: &Value,
    open: &mut Option<VerseData>,
    scan: &SourceScan<'_>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let verse = open.as_mut().ok_or_else(|| {
        anyhow!(
            "USJ_UNEXPECTED_STRUCTURE: note precedes verse on line {line} of {}",
            path.display()
        )
    })?;
    let object = node_value.as_object().ok_or_else(|| {
        super::diagnostic(
            "USJ_UNEXPECTED_STRUCTURE",
            "note node must be an object",
            line,
            path,
        )
    })?;
    let marker = super::require_string(object, "marker", "USJ_INCOMPLETE_NODE", line, path)?;
    let (note_type, allowed_markers) = match marker {
        "f" => ("footnote", FOOTNOTE_TEXT_MARKERS),
        "fe" => ("endnote", FOOTNOTE_TEXT_MARKERS),
        "x" => ("cross_reference", CROSS_REFERENCE_TEXT_MARKERS),
        _ => bail!(
            "USJ_UNKNOWN_MARKER: unsupported note marker `{marker}` on line {line} of {}",
            path.display()
        ),
    };
    let content = object.get("content").ok_or_else(|| {
        super::diagnostic(
            "USJ_INCOMPLETE_NODE",
            "note content is required",
            line,
            path,
        )
    })?;
    let content = content.as_array().ok_or_else(|| {
        super::diagnostic(
            "USJ_INCOMPLETE_NODE",
            "note content must be an array",
            line,
            path,
        )
    })?;
    let mut text = String::new();
    let mut char_marker = None;
    for item in content {
        append_note_content(
            item,
            note_type,
            allowed_markers,
            &mut char_marker,
            &mut text,
            line,
            path,
        )?;
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!(
            "USJ_UNEXPECTED_STRUCTURE: note requires note text on line {line} of {}",
            path.display()
        );
    }
    verse.notes.push(ParsedNote {
        note_type,
        text,
        source_span: scan.span(node_value, line, path)?,
    });
    Ok(())
}

fn append_note_content(
    item: &Value,
    note_type: &str,
    allowed_markers: &[&str],
    char_marker: &mut Option<String>,
    text: &mut String,
    line: u32,
    path: &Path,
) -> Result<()> {
    match item {
        Value::String(value) => {
            let keep = match char_marker.as_deref() {
                Some(marker) if note_type == "cross_reference" => marker == "xt",
                Some(marker) => matches!(marker, "ft" | "fq" | "fqa" | "fk" | "fw"),
                None => true,
            };
            if keep {
                text.push_str(value);
            }
            Ok(())
        }
        Value::Object(node) if super::node_type(node) == Some("char") => {
            let marker = super::require_string(node, "marker", "USJ_INCOMPLETE_NODE", line, path)?;
            if !allowed_markers.contains(&marker) {
                bail!(
                    "USJ_UNKNOWN_MARKER: unknown note marker `{marker}` on line {line} of {}",
                    path.display()
                );
            }
            *char_marker = Some(marker.to_string());
            let content = node.get("content").ok_or_else(|| {
                super::diagnostic(
                    "USJ_INCOMPLETE_NODE",
                    "character content is required",
                    line,
                    path,
                )
            })?;
            let content = content.as_array().ok_or_else(|| {
                super::diagnostic(
                    "USJ_INCOMPLETE_NODE",
                    "character content must be an array",
                    line,
                    path,
                )
            })?;
            for item in content {
                append_note_content(
                    item,
                    note_type,
                    allowed_markers,
                    char_marker,
                    text,
                    line,
                    path,
                )?;
            }
            Ok(())
        }
        Value::Object(node) => {
            let node_type = super::node_type(node).unwrap_or("<missing>");
            bail!(
                "USJ_UNKNOWN_CRITICAL: unsupported note node type `{node_type}` on line {line} of {}",
                path.display()
            )
        }
        _ => bail!(
            "USJ_UNEXPECTED_STRUCTURE: note content must contain strings or nodes on line {line} of {}",
            path.display()
        ),
    }
}
