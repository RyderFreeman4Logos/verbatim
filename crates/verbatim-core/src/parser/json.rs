use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::traits::Parser;
use crate::types::{hex_sha256, EvidenceId, EvidenceKind, EvidenceUnit, SourceId, SourceLocator};

pub struct JsonParser;

impl Parser for JsonParser {
    fn name(&self) -> &str {
        "json"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["json"]
    }

    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read JSON: {}", path.display()))?;
        let value: Value = serde_json::from_str(&content)
            .with_context(|| format!("parse JSON: {}", path.display()))?;
        let mut scalars = Vec::new();
        collect_scalar_text("", &value, &mut scalars);

        let source_id = SourceId::from_path(path);
        let path_str = path.to_string_lossy().to_string();
        let line_count = content.lines().count().max(1) as u32;
        Ok(scalars
            .into_iter()
            .enumerate()
            .map(|(index, scalar)| scalar.into_evidence(&source_id, &path_str, line_count, index))
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonScalarText {
    path: String,
    text: String,
}

impl JsonScalarText {
    fn into_evidence(
        self,
        source_id: &SourceId,
        path_str: &str,
        line_count: u32,
        position: usize,
    ) -> EvidenceUnit {
        let text = if self.path.is_empty() {
            self.text
        } else {
            format!("{}: {}", self.path, self.text)
        };
        EvidenceUnit {
            id: EvidenceId(format!("{}:json:n{}", source_id.0, position)),
            source_id: source_id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: path_str.to_string(),
                line_start: 1,
                line_end: (line_count > 1).then_some(line_count),
            },
            text_hash: hex_sha256(text.as_bytes()),
            heading_path: if self.path.is_empty() {
                Vec::new()
            } else {
                vec![self.path]
            },
            text,
            language: None,
            position: position.try_into().unwrap_or(u32::MAX),
        }
    }
}

fn collect_scalar_text(path: &str, value: &Value, scalars: &mut Vec<JsonScalarText>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                collect_scalar_text(&join_object_path(path, key), child, scalars);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_scalar_text(&join_array_path(path, index), child, scalars);
            }
        }
        Value::String(text) => {
            let text = text.trim();
            if !text.is_empty() {
                scalars.push(JsonScalarText {
                    path: path.to_string(),
                    text: text.to_string(),
                });
            }
        }
        Value::Number(number) => scalars.push(JsonScalarText {
            path: path.to_string(),
            text: number.to_string(),
        }),
        Value::Bool(value) => scalars.push(JsonScalarText {
            path: path.to_string(),
            text: value.to_string(),
        }),
        Value::Null => {}
    }
}

fn join_object_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn join_array_path(prefix: &str, index: usize) -> String {
    if prefix.is_empty() {
        format!("[{index}]")
    } else {
        format!("{prefix}[{index}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn extracts_scalar_text_with_json_paths() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(
            f,
            r#"{{"title":"Alpha","tags":["beta","gamma"],"draft":false,"empty":null}}"#
        )
        .unwrap();

        let units = JsonParser.parse(f.path()).unwrap();

        let mut texts = units
            .iter()
            .map(|unit| unit.text.as_str())
            .collect::<Vec<_>>();
        texts.sort_unstable();
        assert_eq!(
            texts,
            vec![
                "draft: false",
                "tags[0]: beta",
                "tags[1]: gamma",
                "title: Alpha"
            ]
        );
        assert!(units
            .iter()
            .any(|unit| unit.heading_path == vec!["draft".to_string()]));
    }
}
