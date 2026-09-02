use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::traits::Parser;
use crate::types::{
    hex_sha256, BackingSelector, CanonicalLocator, EvidenceId, EvidenceKind, EvidenceUnit,
    ReferenceComponent, SourceId, SourceLocator,
};

pub struct CanonicalJsonlParser;

const GENERATED_EVIDENCE_ID_PREFIX: &str = "cjson:v1:";
const MAX_JSONL_RECORD_BYTES: usize = 1_048_576;

impl Parser for CanonicalJsonlParser {
    fn name(&self) -> &str {
        "canonical_jsonl"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["jsonl"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let file = File::open(path)
            .with_context(|| format!("failed to read JSONL: {}", path.display()))?;
        let mut reader = BufReader::new(file);
        let source_id = SourceId::from_path(path);
        let mut units = Vec::new();
        let mut identity_lines = HashMap::new();
        let mut raw_line = Vec::new();
        let mut index = 0;

        loop {
            raw_line.clear();
            let bytes_read = reader
                .by_ref()
                .take((MAX_JSONL_RECORD_BYTES + 2) as u64)
                .read_until(b'\n', &mut raw_line)
                .with_context(|| format!("failed to read JSONL: {}", path.display()))?;
            if bytes_read == 0 {
                break;
            }
            let line_no = (index as u32) + 1; // 1-based
            index += 1;
            let raw_line = raw_line.strip_suffix(b"\n").unwrap_or(&raw_line);
            let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
            if raw_line.len() > MAX_JSONL_RECORD_BYTES {
                bail!(
                    "canonical JSONL record exceeds maximum size ({MAX_JSONL_RECORD_BYTES} bytes) on line {line_no} of {}",
                    path.display()
                );
            }
            let raw_line = std::str::from_utf8(raw_line)
                .with_context(|| format!("failed to read JSONL: {}", path.display()))?;
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: JsonlEntry = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid JSON on line {line_no} of {}", path.display()))?;
            let kind = evidence_kind_from_content_kind(&entry.content_kind)?;
            let content_kind = if entry.content_kind.is_empty() {
                "text"
            } else {
                &entry.content_kind
            };

            // Reject lines missing required fields.
            if entry.source_profile.is_empty() {
                bail!(
                    "missing required field `source_profile` on line {line_no} of {}",
                    path.display()
                );
            }
            if entry.components.is_empty() {
                bail!(
                    "missing or empty required field `components` on line {line_no} of {}",
                    path.display()
                );
            }
            if entry.text.is_empty() {
                bail!(
                    "missing or empty required field `text` on line {line_no} of {}",
                    path.display()
                );
            }
            let annotations = entry
                .metadata
                .as_ref()
                .map(|metadata| metadata.annotations.clone())
                .unwrap_or_default();
            validate_annotations(&annotations, line_no, path)?;

            let components: Vec<ReferenceComponent> = entry
                .components
                .iter()
                .map(|c| ReferenceComponent {
                    level: c.level.clone(),
                    value: c.value.clone(),
                    ordinal: c.ordinal,
                })
                .collect();

            let normalized = build_normalized(&components);
            let display = entry.display_citation.unwrap_or_else(|| normalized.clone());
            let id = generated_evidence_id(
                &entry.source_profile,
                &entry.work_id,
                entry.version_id.as_deref(),
                entry.canon_id.as_deref(),
                entry.versification_id.as_deref(),
                &normalized,
                content_kind,
            );
            if let Some(first_line) = identity_lines.insert(id.0.clone(), line_no) {
                bail!(
                    "duplicate canonical JSONL logical identity on lines {first_line} and {line_no} of {}: {}",
                    path.display(),
                    id.0
                );
            }

            let heading_path = entry
                .metadata
                .as_ref()
                .and_then(|m| m.section_heading.clone())
                .map(|h| vec![h])
                .unwrap_or_default();

            let text_hash = hex_sha256(entry.text.as_bytes());

            let locator = CanonicalLocator {
                profile_id: entry.source_profile.clone(),
                work_id: entry.work_id.clone(),
                version_id: entry.version_id.clone(),
                canon_id: entry.canon_id.clone(),
                versification_id: entry.versification_id.clone(),
                start: components,
                end: None,
                display,
                normalized,
                backing_selectors: if entry.backing_selectors.is_empty() {
                    vec![BackingSelector::LineRange {
                        start: line_no,
                        end: line_no,
                    }]
                } else {
                    entry.backing_selectors
                },
            };

            units.push(EvidenceUnit {
                id,
                source_id: source_id.clone(),
                kind,
                derived_from: None,
                locator: SourceLocator::Canonical { locator },
                text: entry.text.clone(),
                text_hash,
                heading_path,
                language: entry.language.clone(),
                position: (index - 1) as u32,
                annotations,
            });
        }

        Ok(units)
    }
}

/// Derive a path- and position-independent ID from canonical logical identity.
///
/// The v1 payload is SHA-256 over unsigned 64-bit big-endian length-prefixed
/// fields in this order: domain, source profile, work, optional-version tag and
/// value, optional canon/versification tags and values, normalized locator, and
/// content kind. Absent canon and versification IDs are omitted for compatibility.
fn generated_evidence_id(
    source_profile: &str,
    work_id: &str,
    version_id: Option<&str>,
    canon_id: Option<&str>,
    versification_id: Option<&str>,
    normalized: &str,
    content_kind: &str,
) -> EvidenceId {
    let mut payload = Vec::with_capacity(128);
    append_identity_field(&mut payload, b"canonical-jsonl-evidence-id-v1");
    append_identity_field(&mut payload, source_profile.as_bytes());
    append_identity_field(&mut payload, work_id.as_bytes());
    match version_id {
        Some(version_id) => {
            payload.push(1);
            append_identity_field(&mut payload, version_id.as_bytes());
        }
        None => payload.push(0),
    }
    if canon_id.is_some() || versification_id.is_some() {
        for id in [canon_id, versification_id] {
            match id {
                Some(id) => {
                    payload.push(1);
                    append_identity_field(&mut payload, id.as_bytes());
                }
                None => payload.push(0),
            }
        }
    }
    append_identity_field(&mut payload, normalized.as_bytes());
    append_identity_field(&mut payload, content_kind.as_bytes());
    EvidenceId(format!(
        "{GENERATED_EVIDENCE_ID_PREFIX}{}",
        hex_sha256(&payload)
    ))
}

pub(crate) fn is_generated_evidence_id(id: &EvidenceId) -> bool {
    id.0.strip_prefix(GENERATED_EVIDENCE_ID_PREFIX)
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

pub(crate) fn evidence_kind_from_content_kind(content_kind: &str) -> Result<EvidenceKind> {
    match content_kind {
        "" | "text" => Ok(EvidenceKind::Text),
        "verse" => Ok(EvidenceKind::Verse),
        "footnote" => Ok(EvidenceKind::Footnote),
        _ => bail!("unknown canonical JSONL content_kind {content_kind}"),
    }
}

fn append_identity_field(payload: &mut Vec<u8>, field: &[u8]) {
    payload.extend_from_slice(&(field.len() as u64).to_be_bytes());
    payload.extend_from_slice(field);
}

/// Build a normalized key from reference components.
///
/// Lowercases all component values and joins them with `:`.
/// e.g. `[book:John, chapter:3, verse:16]` → `"john:3:16"`.
fn build_normalized(components: &[ReferenceComponent]) -> String {
    components
        .iter()
        .map(|c| c.value.replace(' ', "").to_lowercase())
        .collect::<Vec<_>>()
        .join(":")
}

fn validate_annotations(
    annotations: &BTreeMap<String, String>,
    line_no: u32,
    path: &Path,
) -> Result<()> {
    if annotations.len() > 8 {
        bail!(
            "canonical JSONL annotations exceed maximum key count (8) on line {line_no} of {}",
            path.display()
        );
    }
    for (key, value) in annotations {
        if key.len() > 64 || value.len() > 64 {
            bail!(
                "canonical JSONL annotations key or value exceeds maximum size (64 bytes) on line {line_no} of {}",
                path.display()
            );
        }
    }
    Ok(())
}

// ---- Serde structs for JSONL deserialization ----

#[derive(Debug, Deserialize)]
struct JsonlEntry {
    #[serde(default)]
    source_profile: String,
    #[serde(default)]
    work_id: String,
    #[serde(default)]
    version_id: Option<String>,
    #[serde(default)]
    canon_id: Option<String>,
    #[serde(default)]
    versification_id: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    components: Vec<JsonlComponent>,
    #[serde(default)]
    display_citation: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    content_kind: String,
    #[serde(default)]
    backing_selectors: Vec<BackingSelector>,
    #[serde(default)]
    metadata: Option<JsonlMetadata>,
}

#[derive(Debug, Deserialize)]
struct JsonlComponent {
    level: String,
    value: String,
    #[serde(default)]
    ordinal: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct JsonlMetadata {
    #[serde(default)]
    section_heading: Option<String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_jsonl(lines: &[&str]) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".jsonl").unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    fn ids_by_locator(units: &[EvidenceUnit]) -> BTreeMap<String, String> {
        units
            .iter()
            .map(|unit| match &unit.locator {
                SourceLocator::Canonical { locator } => {
                    (locator.normalized.clone(), unit.id.0.clone())
                }
                _ => panic!("expected Canonical locator"),
            })
            .collect()
    }

    const JOHN_316: &str = r#"{"source_profile":"bible","work_id":"CSB","version_id":"digital-edition-2017","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"16","ordinal":16}],"display_citation":"John 3:16","text":"For God loved the world..."}"#;
    const JOHN_317: &str = r#"{"source_profile":"bible","work_id":"CSB","version_id":"digital-edition-2017","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"17","ordinal":17}],"display_citation":"John 3:17","text":"For God did not send his Son..."}"#;

    #[test]
    fn generated_identity_survives_inserting_an_unrelated_record() {
        let f = write_jsonl(&[JOHN_316]);
        let original = CanonicalJsonlParser.parse(f.path()).unwrap();

        std::fs::write(f.path(), format!("{JOHN_317}\n{JOHN_316}\n")).unwrap();
        let inserted = CanonicalJsonlParser.parse(f.path()).unwrap();

        assert_eq!(
            ids_by_locator(&original)["john:3:16"],
            ids_by_locator(&inserted)["john:3:16"]
        );
    }

    #[test]
    fn generated_identity_survives_record_reordering() {
        let f = write_jsonl(&[JOHN_316, JOHN_317]);
        let original = CanonicalJsonlParser.parse(f.path()).unwrap();

        std::fs::write(f.path(), format!("{JOHN_317}\n{JOHN_316}\n")).unwrap();
        let reordered = CanonicalJsonlParser.parse(f.path()).unwrap();

        assert_eq!(ids_by_locator(&original), ids_by_locator(&reordered));
    }

    #[test]
    fn generated_identity_ignores_text_but_text_hash_tracks_it() {
        let f = write_jsonl(&[JOHN_316]);
        let original = CanonicalJsonlParser.parse(f.path()).unwrap();
        let edited_line = JOHN_316.replace(
            "For God loved the world...",
            "For God so loved the world...",
        );

        std::fs::write(f.path(), format!("{edited_line}\n")).unwrap();
        let edited = CanonicalJsonlParser.parse(f.path()).unwrap();

        assert_eq!(original[0].id, edited[0].id);
        assert_ne!(original[0].text_hash, edited[0].text_hash);
    }

    #[test]
    fn generated_identity_includes_every_available_identity_component() {
        let f = write_jsonl(&[
            JOHN_316,
            &JOHN_316.replace("\"bible\"", "\"scripture\""),
            &JOHN_316.replace("\"CSB\"", "\"NRSV\""),
            &JOHN_316.replace("digital-edition-2017", "digital-edition-2020"),
            &JOHN_316.replace("\"16\"", "\"18\""),
        ]);

        let units = CanonicalJsonlParser.parse(f.path()).unwrap();
        for changed in &units[1..] {
            assert_ne!(units[0].id, changed.id);
        }
    }

    #[test]
    fn generated_identity_rejects_duplicates() {
        let duplicate = JOHN_316
            .replace("John 3:16", "Jn 3.16")
            .replace("For God loved the world...", "Edited text");
        let f = write_jsonl(&[JOHN_316, &duplicate]);

        let error = CanonicalJsonlParser
            .parse(f.path())
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicate canonical JSONL logical identity"));
        assert!(error.contains("lines 1 and 2"));
    }

    #[test]
    fn generated_identity_has_a_golden_encoding() {
        let f = write_jsonl(&[JOHN_316]);

        let units = CanonicalJsonlParser.parse(f.path()).unwrap();

        assert_eq!(
            units[0].id.0,
            "cjson:v1:a71ce2432af61a12f9ea7cdd5535b1ef95b7adc9fbf218572e24c5d4b6d31d80"
        );
    }

    #[test]
    fn parses_three_line_jsonl_file() {
        let f = write_jsonl(&[
            r#"{"source_profile":"bible","work_id":"CSB","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"16","ordinal":16}],"display_citation":"John 3:16","text":"For God loved the world..."}"#,
            r#"{"source_profile":"bible","work_id":"CSB","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"17","ordinal":17}],"display_citation":"John 3:17","text":"For God did not send his Son..."}"#,
            r#"{"source_profile":"bible","work_id":"CSB","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"18","ordinal":18}],"display_citation":"John 3:18","text":"Anyone who believes in him is not condemned."}"#,
        ]);

        let units = CanonicalJsonlParser.parse(f.path()).unwrap();
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].text, "For God loved the world...");
        assert_eq!(units[1].position, 1);
        assert_eq!(units[2].position, 2);
        assert_eq!(
            units.iter().map(|unit| &unit.id.0).collect::<Vec<_>>(),
            vec![
                "cjson:v1:96a9ca1fd9a5a7fc7b39a0a278f2069cb2cc3962f25325c5dfb6943375f6e0d1",
                "cjson:v1:4442202871d7f5b827c94bfd68a7072937a803e5f68f89fb3817235b3b4192c7",
                "cjson:v1:362d1bfb80ceca7a2509bd56d56a852bea977cd94e69f642887bf4993c1efe92",
            ]
        );
        assert_eq!(
            units
                .iter()
                .map(|unit| match &unit.locator {
                    SourceLocator::Canonical { locator } => locator.backing_selectors.clone(),
                    _ => panic!("expected Canonical locator"),
                })
                .collect::<Vec<_>>(),
            vec![
                vec![BackingSelector::LineRange { start: 1, end: 1 }],
                vec![BackingSelector::LineRange { start: 2, end: 2 }],
                vec![BackingSelector::LineRange { start: 3, end: 3 }],
            ]
        );
    }

    #[test]
    fn rejects_oversized_record_with_path_and_line() {
        let oversized = format!(
            r#"{{"source_profile":"bible","work_id":"CSB","components":[{{"level":"book","value":"John"}}],"text":"{}"}}"#,
            "x".repeat(1_048_576)
        );
        let f = write_jsonl(&[&oversized]);

        let error = CanonicalJsonlParser
            .parse(f.path())
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("record exceeds maximum size"),
            "error was: {error}"
        );
        assert!(error.contains("line 1"), "error was: {error}");
        assert!(
            error.contains(&f.path().display().to_string()),
            "error was: {error}"
        );
    }

    #[test]
    fn correct_locator_and_display_citation() {
        let f = write_jsonl(&[
            r#"{"source_profile":"bible","work_id":"CSB","version_id":"digital-edition-2017","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"16","ordinal":16}],"display_citation":"John 3:16","text":"For God loved the world...","metadata":{"section_heading":"Jesus and Nicodemus","testament":"NT"}}"#,
        ]);

        let units = CanonicalJsonlParser.parse(f.path()).unwrap();
        assert_eq!(units.len(), 1);
        let unit = &units[0];

        match &unit.locator {
            SourceLocator::Canonical { locator } => {
                assert_eq!(locator.profile_id, "bible");
                assert_eq!(locator.work_id, "CSB");
                assert_eq!(locator.version_id.as_deref(), Some("digital-edition-2017"));
                assert_eq!(locator.display, "John 3:16");
                assert_eq!(locator.normalized, "john:3:16");
                assert_eq!(locator.start.len(), 3);
                assert!(locator.end.is_none());
                assert_eq!(
                    locator.backing_selectors,
                    vec![BackingSelector::LineRange { start: 1, end: 1 }]
                );
            }
            _ => panic!("expected Canonical locator"),
        }

        // heading_path from metadata.section_heading
        assert_eq!(unit.heading_path, vec!["Jesus and Nicodemus".to_string()]);

        // text_hash is hex sha256
        assert_eq!(
            unit.text_hash,
            hex_sha256("For God loved the world...".as_bytes())
        );
    }

    #[test]
    fn rejects_missing_source_profile() {
        let f = write_jsonl(&[
            r#"{"work_id":"CSB","components":[{"level":"book","value":"John"}],"text":"some text"}"#,
        ]);

        let result = CanonicalJsonlParser.parse(f.path());
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("source_profile"), "msg was: {msg}");
        assert!(msg.contains("line 1"), "msg was: {msg}");
    }

    #[test]
    fn rejects_missing_text() {
        let f = write_jsonl(&[
            r#"{"source_profile":"bible","work_id":"CSB","components":[{"level":"book","value":"John"}],"display_citation":"John 1"}"#,
        ]);

        let result = CanonicalJsonlParser.parse(f.path());
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("text"), "msg was: {msg}");
        assert!(msg.contains("line 1"), "msg was: {msg}");
    }

    #[test]
    fn rejects_missing_components() {
        let f = write_jsonl(&[r#"{"source_profile":"bible","work_id":"CSB","text":"some text"}"#]);

        let result = CanonicalJsonlParser.parse(f.path());
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("components"), "msg was: {msg}");
    }

    #[test]
    fn skips_blank_lines() {
        let f = write_jsonl(&[
            r#""#,
            r#"{"source_profile":"bible","work_id":"CSB","components":[{"level":"book","value":"John"},{"level":"chapter","value":"3"},{"level":"verse","value":"16"}],"display_citation":"John 3:16","text":"verse text"}"#,
            r#"   "#,
        ]);

        let units = CanonicalJsonlParser.parse(f.path()).unwrap();
        assert_eq!(units.len(), 1);
    }

    #[test]
    fn normalized_strips_spaces_and_lowercases() {
        let f = write_jsonl(&[
            r#"{"source_profile":"bible","work_id":"CSB","components":[{"level":"book","value":"1 John"},{"level":"chapter","value":"4"},{"level":"verse","value":"8"}],"display_citation":"1 John 4:8","text":"God is love."}"#,
        ]);

        let units = CanonicalJsonlParser.parse(f.path()).unwrap();
        assert_eq!(units.len(), 1);
        match &units[0].locator {
            SourceLocator::Canonical { locator } => {
                assert_eq!(locator.normalized, "1john:4:8");
            }
            _ => panic!("expected Canonical locator"),
        }
    }
}
