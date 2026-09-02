use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::profiles::bible::canon_registry::{CanonRegistry, VERSION as CANON_VERSION};
use crate::profiles::bible::versification_registry::{
    VersificationRegistry, VERSION as VERSIFICATION_VERSION,
};
use crate::traits::Parser;
use crate::types::{
    hex_sha256, BackingSelector, CanonicalLocator, EvidenceId, EvidenceKind, EvidenceUnit,
    ReferenceComponent, SourceId, SourceLocator,
};

/// The deliberately small USFM vocabulary supported by this walking skeleton.
pub const SUPPORTED_MARKERS: &[&str] = &["id", "c", "v"];
const SOURCE_NATIVE_SCHEME: &str = "usfm";
const WORK_ID: &str = "USFM";

pub struct UsfmParser;

impl Parser for UsfmParser {
    fn name(&self) -> &str {
        "usfm"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["usfm"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read USFM source {}", path.display()))?;
        parse_content(&content, path)
    }
}

fn parse_content(content: &str, path: &Path) -> Result<Vec<EvidenceUnit>> {
    let mut book = None;
    let mut chapter = None;
    let mut verse = None;
    let mut verse_text = None;
    let mut verse_line = None;
    let mut last_line = 0;

    for (index, raw_line) in content.lines().enumerate() {
        let line = index as u32 + 1;
        last_line = line;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (marker, payload) = marker_line(trimmed, path, line)?;
        match marker {
            "id" => {
                if book.is_some() || chapter.is_some() || verse.is_some() {
                    bail!(
                        "USFM_DUPLICATE_MARKER: duplicate \\id on line {line} of {}",
                        path.display()
                    );
                }
                let id = single_token(payload, path, line, "id")?;
                book = Some(CanonRegistry::by_id(id).ok_or_else(|| {
                    anyhow::anyhow!(
                        "USFM_INVALID_COORDINATE: unknown book {id} on line {line} of {}",
                        path.display()
                    )
                })?);
            }
            "c" => {
                if book.is_none() {
                    bail!(
                        "USFM_MALFORMED_MARKER: \\c precedes \\id on line {line} of {}",
                        path.display()
                    );
                }
                if chapter.is_some() || verse.is_some() {
                    bail!(
                        "USFM_DUPLICATE_MARKER: duplicate \\c on line {line} of {}",
                        path.display()
                    );
                }
                chapter = Some(number(
                    single_token(payload, path, line, "c")?,
                    path,
                    line,
                    "c",
                )?);
            }
            "v" => {
                let book = book.ok_or_else(|| {
                    anyhow::anyhow!(
                        "USFM_MALFORMED_MARKER: \\v precedes \\id on line {line} of {}",
                        path.display()
                    )
                })?;
                let chapter = chapter.ok_or_else(|| {
                    anyhow::anyhow!(
                        "USFM_MALFORMED_MARKER: \\v precedes \\c on line {line} of {}",
                        path.display()
                    )
                })?;
                if verse.is_some() {
                    bail!(
                        "USFM_DUPLICATE_COORDINATE: duplicate verse on line {line} of {}",
                        path.display()
                    );
                }
                let (verse_token, text) = verse_payload(payload, path, line)?;
                let verse_number = number(verse_token, path, line, "v")?;
                let address = VersificationRegistry::lookup(book.id, chapter, verse_number)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "USFM_INVALID_COORDINATE: {} {}:{} on line {line} of {}",
                            book.id,
                            chapter,
                            verse_number,
                            path.display()
                        )
                    })?;
                verse = Some(address.verse);
                verse_text = Some(text.to_string());
                verse_line = Some(line);
            }
            _ => {
                bail!(
                    "USFM_UNKNOWN_MARKER: unknown marker \\{marker} on line {line} of {}",
                    path.display()
                );
            }
        }
    }

    let book = book.ok_or_else(|| {
        anyhow::anyhow!(
            "USFM_MALFORMED_MARKER: missing \\id on line {} of {}",
            last_line.max(1),
            path.display()
        )
    })?;
    let chapter = chapter.ok_or_else(|| {
        anyhow::anyhow!(
            "USFM_MALFORMED_MARKER: missing \\c on line {} of {}",
            last_line.max(1),
            path.display()
        )
    })?;
    let verse = verse.ok_or_else(|| {
        anyhow::anyhow!(
            "USFM_MALFORMED_MARKER: missing \\v on line {} of {}",
            last_line.max(1),
            path.display()
        )
    })?;
    let text = verse_text.expect("verse text is set with verse");
    let line = verse_line.expect("verse line is set with verse");
    let display = format!("{} {chapter}:{verse}", book.name);
    let components = vec![
        ReferenceComponent {
            level: "book".to_string(),
            value: book.name.to_string(),
            ordinal: Some(book.ordinal as u32),
        },
        ReferenceComponent {
            level: "chapter".to_string(),
            value: chapter.to_string(),
            ordinal: Some(chapter as u32),
        },
        ReferenceComponent {
            level: "verse".to_string(),
            value: verse.to_string(),
            ordinal: Some(verse as u32),
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
        display,
        normalized,
        backing_selectors: vec![
            BackingSelector::SourceNative {
                scheme: SOURCE_NATIVE_SCHEME.to_string(),
                value: format!("{} {}:{}", book.id, chapter, verse),
            },
            BackingSelector::LineRange {
                start: line,
                end: line,
            },
        ],
    };

    Ok(vec![EvidenceUnit {
        id: EvidenceId(format!("{}:usfm:n0", source_id.0)),
        source_id,
        kind: EvidenceKind::Verse,
        derived_from: None,
        locator: SourceLocator::Canonical { locator },
        text_hash: hex_sha256(text.as_bytes()),
        text,
        heading_path: Vec::new(),
        language: None,
        position: 0,
        annotations: BTreeMap::new(),
    }])
}

fn marker_line<'a>(line: &'a str, path: &Path, line_number: u32) -> Result<(&'a str, &'a str)> {
    let Some(body) = line.strip_prefix('\\') else {
        bail!(
            "USFM_MALFORMED_MARKER: expected a supported marker on line {line_number} of {}",
            path.display()
        );
    };
    let Some(marker_end) = body.find(char::is_whitespace) else {
        if body.is_empty() {
            bail!(
                "USFM_UNTERMINATED_MARKER: empty marker on line {line_number} of {}",
                path.display()
            );
        }
        if !SUPPORTED_MARKERS.contains(&body) {
            bail!(
                "USFM_UNKNOWN_MARKER: unknown marker \\{body} on line {line_number} of {}",
                path.display()
            );
        }
        bail!(
            "USFM_MALFORMED_MARKER: marker \\{body} has no payload on line {line_number} of {}",
            path.display()
        );
    };
    let marker = &body[..marker_end];
    let payload = body[marker_end..].trim();
    if marker.is_empty() || payload.contains('\\') {
        bail!(
            "USFM_UNTERMINATED_MARKER: unterminated marker on line {line_number} of {}",
            path.display()
        );
    }
    Ok((marker, payload))
}

fn single_token<'a>(payload: &'a str, path: &Path, line: u32, marker: &str) -> Result<&'a str> {
    let mut tokens = payload.split_whitespace();
    match (tokens.next(), tokens.next()) {
        (Some(token), None) => Ok(token),
        _ => bail!(
            "USFM_MALFORMED_MARKER: \\{marker} requires one value on line {line} of {}",
            path.display()
        ),
    }
}

fn verse_payload<'a>(payload: &'a str, path: &Path, line: u32) -> Result<(&'a str, &'a str)> {
    let Some(separator) = payload.find(char::is_whitespace) else {
        bail!(
            "USFM_MALFORMED_MARKER: \\v requires verse text on line {line} of {}",
            path.display()
        );
    };
    let verse = &payload[..separator];
    let text = payload[separator..].trim();
    if verse.is_empty() || text.is_empty() {
        bail!(
            "USFM_MALFORMED_MARKER: \\v requires verse number and text on line {line} of {}",
            path.display()
        );
    }
    Ok((verse, text))
}

fn number(token: &str, path: &Path, line: u32, marker: &str) -> Result<u16> {
    token.parse::<u16>().map_err(|_| {
        anyhow::anyhow!(
            "USFM_MALFORMED_MARKER: \\{marker} requires a numeric value on line {line} of {}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::UsfmParser;
    use crate::traits::Parser;
    use crate::types::{BackingSelector, EvidenceKind, SourceLocator};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn fixture(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::with_suffix(".usfm").unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn parses_one_verse_with_canonical_usfm_backing() {
        let file = fixture("\\id JHN\n\\c 3\n\\v 16 For God so loved the world.\n");
        let units = UsfmParser.parse(file.path()).unwrap();

        assert_eq!(units.len(), 1);
        let unit = &units[0];
        assert_eq!(unit.kind, EvidenceKind::Verse);
        assert_eq!(unit.text, "For God so loved the world.");
        match &unit.locator {
            SourceLocator::Canonical { locator } => {
                assert_eq!(locator.profile_id, "bible");
                assert_eq!(locator.display, "John 3:16");
                assert_eq!(locator.normalized, "john:3:16");
                assert!(locator
                    .backing_selectors
                    .contains(&BackingSelector::SourceNative {
                        scheme: "usfm".to_string(),
                        value: "JHN 3:16".to_string(),
                    }));
                assert!(locator
                    .backing_selectors
                    .contains(&BackingSelector::LineRange { start: 3, end: 3 }));
            }
            locator => panic!("expected canonical locator, got {locator:?}"),
        }
    }

    #[test]
    fn unknown_marker_rejects_file_with_line_diagnostic() {
        let file = fixture("\\id JHN\n\\c 3\n\\foo nope\n\\v 16 text\n");
        let error = UsfmParser.parse(file.path()).unwrap_err().to_string();

        assert!(error.contains("USFM_UNKNOWN_MARKER"));
        assert!(error.contains("line 3"));
        assert!(error.contains("\\foo"));
    }

    #[test]
    fn malformed_and_duplicate_coordinates_reject_file() {
        for contents in [
            "\\id JHN\n\\c 3\n\\v 16\n",
            "\\id JHN\n\\c 3\n\\v 16 first\n\\v 16 second\n",
            "\\id JHN\n\\c 3\n\\v 37 out of bounds\n",
        ] {
            assert!(UsfmParser.parse(&fixture(contents).path()).is_err());
        }
    }
}
