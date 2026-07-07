use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::traits::Parser;
use crate::types::{
    hex_sha256, BackingSelector, CanonicalLocator, EvidenceId, EvidenceKind, EvidenceUnit,
    ReferenceComponent, SourceId, SourceLocator,
};

pub struct CanonicalJsonlParser;

impl Parser for CanonicalJsonlParser {
    fn name(&self) -> &str {
        "canonical_jsonl"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["jsonl"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read JSONL: {}", path.display()))?;
        let source_id = SourceId::from_path(path);
        let mut units = Vec::new();

        for (index, raw_line) in content.lines().enumerate() {
            let line_no = (index as u32) + 1; // 1-based
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: JsonlEntry = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid JSON on line {line_no} of {}", path.display()))?;

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
                start: components,
                end: None,
                display,
                normalized,
                backing_selectors: vec![BackingSelector::LineRange {
                    start: line_no,
                    end: line_no,
                }],
            };

            units.push(EvidenceUnit {
                id: EvidenceId(format!("{}:cjson:n{}", source_id.0, index)),
                source_id: source_id.clone(),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: SourceLocator::Canonical { locator },
                text: entry.text.clone(),
                text_hash,
                heading_path,
                position: index as u32,
            });
        }

        Ok(units)
    }
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

// ---- Serde structs for JSONL deserialization ----

#[derive(Debug, Deserialize)]
struct JsonlEntry {
    #[serde(default)]
    source_profile: String,
    #[serde(default)]
    work_id: String,
    #[serde(default)]
    version_id: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    components: Vec<JsonlComponent>,
    #[serde(default)]
    display_citation: Option<String>,
    #[serde(default)]
    text: String,
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
