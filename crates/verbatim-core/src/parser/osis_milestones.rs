use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use super::{parse_chapter, parse_verse, CanonBook, Counts, Element, VerseData};

#[derive(Debug)]
pub(super) enum Milestone {
    Chapter {
        id: String,
        book: CanonBook,
        chapter: u16,
    },
    Verse {
        id: String,
        osis_id: String,
        book: CanonBook,
        chapter: u16,
        verse: u16,
        line_start: u32,
        text: String,
    },
}

enum Marker {
    ChapterStart { id: String, osis_id: String },
    ChapterEnd(String),
    VerseStart { id: String, osis_id: String },
    VerseEnd(String),
}

pub(super) fn validate_start(
    name: &str,
    attrs: &BTreeMap<String, String>,
    milestones_open: bool,
    line: u32,
    path: &Path,
) -> Result<()> {
    if marker(name, attrs, line, path)?.is_some() {
        bail!(
            "OSIS_MALFORMED_MILESTONE: milestone `{name}` must be an empty element on line {line} of {}",
            path.display()
        );
    }
    if milestones_open {
        bail!(
            "OSIS_MALFORMED_MILESTONE: nested XML is not permitted while a milestone is open on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn handle_empty(
    name: &str,
    attrs: &BTreeMap<String, String>,
    stack: &[Element],
    milestones: &mut Vec<Milestone>,
    counts: &mut Counts,
    verse: &mut Option<VerseData>,
    location: (u32, &Path),
) -> Result<()> {
    let (line, path) = location;
    match marker(name, attrs, line, path)? {
        Some(marker) => handle(marker, stack, milestones, counts, verse, line, path),
        None if !milestones.is_empty() => bail!(
            "OSIS_MALFORMED_MILESTONE: nested XML is not permitted while a milestone is open on line {line} of {}",
            path.display()
        ),
        None => bail!(
            "OSIS_UNEXPECTED_STRUCTURE: empty `{name}` element is unsupported on line {line} of {}",
            path.display()
        ),
    }
}

pub(super) fn validate_end(
    name: &str,
    milestones_open: bool,
    line: u32,
    path: &Path,
) -> Result<()> {
    if milestones_open {
        bail!(
            "OSIS_MALFORMED_MILESTONE: container end `{name}` occurred while a milestone is open on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn append_text(
    stack: &mut [Element],
    milestones: &mut [Milestone],
    decoded: &str,
    line: u32,
    path: &Path,
) -> Result<()> {
    if let Some(Element::Verse { text, .. }) = stack.last_mut() {
        text.push_str(decoded);
    } else if let Some(Milestone::Verse { text, .. }) = milestones.last_mut() {
        text.push_str(decoded);
    } else if !decoded.trim().is_empty() {
        if milestones.is_empty() {
            bail!(
                "OSIS_UNEXPECTED_STRUCTURE: non-whitespace text outside verse on line {line} of {}",
                path.display()
            );
        }
        bail!(
            "OSIS_MALFORMED_MILESTONE: non-whitespace text is outside an open verse on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn require_closed(milestones: &[Milestone], bytes: &[u8], path: &Path) -> Result<()> {
    if !milestones.is_empty() {
        bail!(
            "OSIS_MALFORMED_MILESTONE: unclosed milestone pair on line {} of {}",
            super::line_number(bytes, bytes.len()),
            path.display()
        );
    }
    Ok(())
}

fn marker(
    name: &str,
    attrs: &BTreeMap<String, String>,
    line: u32,
    path: &Path,
) -> Result<Option<Marker>> {
    if name == "milestone" {
        bail!(
            "OSIS_UNSUPPORTED_MILESTONE: milestone elements are unsupported on line {line} of {}",
            path.display()
        );
    }
    let has_start = attrs.contains_key("sID");
    let has_end = attrs.contains_key("eID");
    if !has_start && !has_end {
        return Ok(None);
    }
    if name != "chapter" && name != "verse" {
        bail!(
            "OSIS_MALFORMED_MILESTONE: `{name}` cannot carry sID or eID on line {line} of {}",
            path.display()
        );
    }
    if has_start == has_end {
        bail!(
            "OSIS_MALFORMED_MILESTONE: milestone `{name}` must carry exactly one of sID or eID on line {line} of {}",
            path.display()
        );
    }
    let id_name = if has_start { "sID" } else { "eID" };
    let allowed = if has_start {
        &["sID", "osisID"][..]
    } else {
        &["eID"][..]
    };
    if attrs
        .keys()
        .any(|key| !key.starts_with("xmlns") && !allowed.contains(&key.as_str()))
    {
        bail!(
            "OSIS_MALFORMED_MILESTONE: unsupported attribute on `{name}` milestone on line {line} of {}",
            path.display()
        );
    }
    let id = attrs
        .get(id_name)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "OSIS_MALFORMED_MILESTONE: invalid `{id_name}` on `{name}` milestone on line {line} of {}",
                path.display()
            )
        })?;
    let osis_id = if has_start {
        attrs
            .get("osisID")
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "OSIS_MALFORMED_MILESTONE: missing `osisID` on `{name}` milestone on line {line} of {}",
                    path.display()
                )
            })?
    } else {
        String::new()
    };
    Ok(Some(match (name, has_start) {
        ("chapter", true) => Marker::ChapterStart { id, osis_id },
        ("chapter", false) => Marker::ChapterEnd(id),
        ("verse", true) => Marker::VerseStart { id, osis_id },
        ("verse", false) => Marker::VerseEnd(id),
        _ => unreachable!(),
    }))
}

fn milestone_book(stack: &[Element], line: u32, path: &Path) -> Result<CanonBook> {
    match stack {
        [Element::Osis, Element::OsisText, Element::Book { book }] => Ok(*book),
        [
            Element::Osis,
            Element::OsisText,
            Element::Book { book },
            Element::Chapter { .. },
        ] => Ok(*book),
        _ => bail!(
            "OSIS_MALFORMED_MILESTONE: milestone is outside the book container on line {line} of {}",
            path.display()
        ),
    }
}

fn handle(
    marker: Marker,
    stack: &[Element],
    milestones: &mut Vec<Milestone>,
    counts: &mut Counts,
    verse: &mut Option<VerseData>,
    line: u32,
    path: &Path,
) -> Result<()> {
    let book = milestone_book(stack, line, path)?;
    match marker {
        Marker::ChapterStart { id, osis_id } => {
            if !milestones.is_empty() || counts.chapters != 0 {
                bail!(
                    "OSIS_MALFORMED_MILESTONE: duplicate or overlapping chapter start `{id}` on line {line} of {}",
                    path.display()
                );
            }
            let chapter = parse_chapter(&osis_id, book, line, path)?;
            counts.chapters += 1;
            milestones.push(Milestone::Chapter { id, book, chapter });
        }
        Marker::VerseStart { id, osis_id } => {
            let (chapter_book, chapter) = match milestones.last() {
                Some(Milestone::Chapter {
                    book: chapter_book,
                    chapter,
                    ..
                }) => (*chapter_book, *chapter),
                _ => match stack.last() {
                    Some(Element::Chapter {
                        book: chapter_book,
                        chapter,
                        ..
                    }) => (*chapter_book, *chapter),
                    _ => {
                        bail!(
                            "OSIS_MALFORMED_MILESTONE: verse start `{id}` has no open chapter milestone on line {line} of {}",
                            path.display()
                        )
                    }
                },
            };
            if counts.verses != 0 {
                bail!(
                    "OSIS_MALFORMED_MILESTONE: duplicate or overlapping verse start `{id}` on line {line} of {}",
                    path.display()
                );
            }
            let verse_number = parse_verse(&osis_id, book, chapter_book, chapter, line, path)?;
            counts.verses += 1;
            milestones.push(Milestone::Verse {
                id,
                osis_id,
                book,
                chapter,
                verse: verse_number,
                line_start: line,
                text: String::new(),
            });
        }
        Marker::ChapterEnd(id) => match milestones.last() {
            Some(Milestone::Chapter { id: expected, .. }) if expected == &id => {
                milestones.pop();
            }
            Some(Milestone::Chapter { .. }) => {
                bail!(
                    "OSIS_MALFORMED_MILESTONE: chapter end `{id}` does not match its start on line {line} of {}",
                    path.display()
                );
            }
            _ => {
                bail!(
                    "OSIS_MALFORMED_MILESTONE: chapter end `{id}` has no matching start on line {line} of {}",
                    path.display()
                );
            }
        },
        Marker::VerseEnd(id) => {
            match milestones.last() {
                Some(Milestone::Verse { id: expected, .. }) if expected == &id => {}
                Some(Milestone::Verse { .. }) => {
                    bail!(
                        "OSIS_MALFORMED_MILESTONE: verse end `{id}` does not match its start on line {line} of {}",
                        path.display()
                    );
                }
                _ => {
                    bail!(
                        "OSIS_MALFORMED_MILESTONE: verse end `{id}` has no matching start on line {line} of {}",
                        path.display()
                    );
                }
            }
            let (book, chapter, verse_number, osis_id, line_start, text) = match milestones.pop() {
                Some(Milestone::Verse {
                    id: _,
                    book,
                    chapter,
                    verse: verse_number,
                    osis_id,
                    line_start,
                    text,
                }) => (book, chapter, verse_number, osis_id, line_start, text),
                _ => {
                    bail!(
                        "OSIS_MALFORMED_MILESTONE: verse end `{id}` has no matching start on line {line} of {}",
                        path.display()
                    )
                }
            };
            let text = text.trim().to_owned();
            if text.is_empty() {
                bail!(
                    "OSIS_EMPTY_VERSE: verse `{osis_id}` has no text on line {line} of {}",
                    path.display()
                );
            }
            *verse = Some(VerseData {
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
    Ok(())
}
