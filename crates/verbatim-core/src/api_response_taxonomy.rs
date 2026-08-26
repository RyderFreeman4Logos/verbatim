use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CitationResponse, RetrieveResultResponse};

/// Closed text planes for retrieval-only response schemas.
/// `GeneratedInterpretation` is never source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTextPlane {
    Evidence,
    Metadata,
    DeterministicInterfaceText,
    GeneratedInterpretation,
}

/// JSON field path and the plane governing its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextFieldTaxonomy {
    pub field: String,
    pub plane: OutputTextPlane,
}

/// Machine-checkable taxonomy for text-bearing retrieval output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseTextTaxonomy {
    pub version: u8,
    pub fields: Vec<TextFieldTaxonomy>,
}

impl ResponseTextTaxonomy {
    const VERSION: u8 = 1;

    pub fn ask_response() -> Self {
        Self::from_fields(&[
            ("answer", OutputTextPlane::GeneratedInterpretation),
            ("answer_kind", OutputTextPlane::Metadata),
        ])
    }

    pub fn ask_response_with_citations(citations: &[CitationResponse]) -> Self {
        let mut taxonomy = Self::from_fields(&[
            ("answer", OutputTextPlane::GeneratedInterpretation),
            ("answer_kind", OutputTextPlane::Metadata),
            (
                "generated_interpretation.text",
                OutputTextPlane::GeneratedInterpretation,
            ),
            (
                "citations[].label",
                OutputTextPlane::DeterministicInterfaceText,
            ),
            ("citations[].evidence_id", OutputTextPlane::Metadata),
            ("citations[].kind", OutputTextPlane::Metadata),
            ("citations[].derived_from", OutputTextPlane::Metadata),
            ("citations[].locator", OutputTextPlane::Metadata),
            (
                "citations[].collections[].collection_id",
                OutputTextPlane::Metadata,
            ),
            ("citations[].collections[].name", OutputTextPlane::Metadata),
            (
                "citations[].collections[].logical_path",
                OutputTextPlane::Metadata,
            ),
            (
                "citations[].collections[].source_path",
                OutputTextPlane::Metadata,
            ),
            (
                "citations[].collections[].member_updated_at",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.requested.collection_ids[]",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.requested.names[]",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.applied[].collection_id",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.applied[].name",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.applied[].last_synced_at",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.warnings[]",
                OutputTextPlane::DeterministicInterfaceText,
            ),
        ]);
        if citations.is_empty() {
            taxonomy.push("citations[].text_preview", OutputTextPlane::Evidence);
        } else {
            for (index, citation) in citations.iter().enumerate() {
                taxonomy.push(
                    format!("citations[{index}].text_preview"),
                    text_plane_for_kind(&citation.kind, None),
                );
            }
        }
        taxonomy
    }

    pub fn retrieve_response() -> Self {
        Self::retrieve_response_with_results_and_options(&[], false)
    }

    pub fn retrieve_response_with_results(results: &[RetrieveResultResponse]) -> Self {
        Self::retrieve_response_with_results_and_options(results, false)
    }

    fn retrieve_response_with_results_and_options(
        results: &[RetrieveResultResponse],
        include_optional_fields: bool,
    ) -> Self {
        let mut taxonomy = Self::from_fields(&[
            (
                "results[].label",
                OutputTextPlane::DeterministicInterfaceText,
            ),
            ("task_id", OutputTextPlane::Metadata),
            ("query", OutputTextPlane::Metadata),
            ("source_id", OutputTextPlane::Metadata),
            ("embedding_profile_id", OutputTextPlane::Metadata),
            (
                "collection_filter.requested.collection_ids[]",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.requested.names[]",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.applied[].collection_id",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.applied[].name",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.applied[].last_synced_at",
                OutputTextPlane::Metadata,
            ),
            (
                "collection_filter.warnings[]",
                OutputTextPlane::DeterministicInterfaceText,
            ),
            (
                "audit_receipt.embedding_profile_id",
                OutputTextPlane::Metadata,
            ),
            (
                "audit_receipt.results[].evidence_id",
                OutputTextPlane::Metadata,
            ),
            (
                "audit_receipt.results[].text_hash",
                OutputTextPlane::Metadata,
            ),
            (
                "audit_receipt.results[].source_hash",
                OutputTextPlane::Metadata,
            ),
            ("timings[].phase", OutputTextPlane::Metadata),
            ("results[].evidence_id", OutputTextPlane::Metadata),
            ("results[].text_hash", OutputTextPlane::Metadata),
            ("results[].source_id", OutputTextPlane::Metadata),
            ("results[].source_hash", OutputTextPlane::Metadata),
            ("results[].source_path", OutputTextPlane::Metadata),
            ("results[].chunk_id", OutputTextPlane::Metadata),
            ("results[].kind", OutputTextPlane::Metadata),
            ("results[].role", OutputTextPlane::Metadata),
            ("results[].locator", OutputTextPlane::Metadata),
            (
                "results[].collections[].collection_id",
                OutputTextPlane::Metadata,
            ),
            ("results[].collections[].name", OutputTextPlane::Metadata),
            (
                "results[].collections[].logical_path",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].collections[].source_path",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].collections[].member_updated_at",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.type",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.path_or_url",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.path",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.ocr.profile.provider",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.ocr.profile.engine",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.ocr.profile.engine_version",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.ocr.profile.language",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.ocr.profile.profile",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.ocr.profile_hash",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.ocr.text_hash",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.heading_slug",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.heading_path[].text",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.heading_path[].slug",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.profile_id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.work_id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.version_id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.display",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.normalized",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.start[].level",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.start[].value",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.end[].level",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.end[].value",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.backing_selectors[].type",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.backing_selectors[].exact",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.backing_selectors[].prefix",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.backing_selectors[].suffix",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.backing_selectors[].id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.backing_selectors[].scheme",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].structured_locator.backing_selectors[].value",
                OutputTextPlane::Metadata,
            ),
            ("results[].provenance.origin", OutputTextPlane::Metadata),
            (
                "results[].provenance.seed_chunk_id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].provenance.seed_source_id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].provenance.graph_path[].edge_type",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].provenance.graph_path[].from_node_id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].provenance.graph_path[].to_node_id",
                OutputTextPlane::Metadata,
            ),
            (
                "results[].provenance.graph_path[].direction",
                OutputTextPlane::Metadata,
            ),
            ("results[].derived_from", OutputTextPlane::Metadata),
        ]);
        if !include_optional_fields
            && !results
                .iter()
                .any(|result| result.structured_locator.is_some())
        {
            taxonomy
                .fields
                .retain(|field| !field.field.starts_with("results[].structured_locator."));
        }
        if !include_optional_fields && !results.iter().any(|result| result.provenance.is_some()) {
            taxonomy
                .fields
                .retain(|field| !field.field.starts_with("results[].provenance."));
        }
        if results.is_empty() {
            taxonomy.push("results[].snippet", OutputTextPlane::Evidence);
        } else {
            for (index, result) in results.iter().enumerate() {
                taxonomy.push(
                    format!("results[{index}].snippet"),
                    text_plane_for_kind(&result.kind, Some(&result.role)),
                );
            }
        }
        taxonomy
    }

    pub fn evidence_response(source_bounded: bool) -> Self {
        Self::evidence_response_for_kind("text", source_bounded)
    }

    pub fn evidence_response_for_kind(kind: &str, source_bounded: bool) -> Self {
        let text_plane = text_plane_for_evidence_kind(kind, source_bounded);

        Self::from_fields(&[
            ("text", text_plane),
            ("heading_path[]", text_plane),
            ("id", OutputTextPlane::Metadata),
            ("source_id", OutputTextPlane::Metadata),
            ("source_hash", OutputTextPlane::Metadata),
            ("text_hash", OutputTextPlane::Metadata),
            ("kind", OutputTextPlane::Metadata),
            ("derived_from", OutputTextPlane::Metadata),
            ("locator", OutputTextPlane::Metadata),
            ("structured_locator.type", OutputTextPlane::Metadata),
            ("structured_locator.path_or_url", OutputTextPlane::Metadata),
            ("structured_locator.path", OutputTextPlane::Metadata),
            (
                "structured_locator.ocr.profile.provider",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.ocr.profile.engine",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.ocr.profile.engine_version",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.ocr.profile.language",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.ocr.profile.profile",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.ocr.profile_hash",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.ocr.text_hash",
                OutputTextPlane::Metadata,
            ),
            ("structured_locator.heading_slug", OutputTextPlane::Metadata),
            (
                "structured_locator.heading_path[].text",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.heading_path[].slug",
                OutputTextPlane::Metadata,
            ),
            ("structured_locator.profile_id", OutputTextPlane::Metadata),
            ("structured_locator.work_id", OutputTextPlane::Metadata),
            ("structured_locator.version_id", OutputTextPlane::Metadata),
            ("structured_locator.display", OutputTextPlane::Metadata),
            ("structured_locator.normalized", OutputTextPlane::Metadata),
            (
                "structured_locator.start[].level",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.start[].value",
                OutputTextPlane::Metadata,
            ),
            ("structured_locator.end[].level", OutputTextPlane::Metadata),
            ("structured_locator.end[].value", OutputTextPlane::Metadata),
            (
                "structured_locator.backing_selectors[].type",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.backing_selectors[].exact",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.backing_selectors[].prefix",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.backing_selectors[].suffix",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.backing_selectors[].id",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.backing_selectors[].scheme",
                OutputTextPlane::Metadata,
            ),
            (
                "structured_locator.backing_selectors[].value",
                OutputTextPlane::Metadata,
            ),
            ("image_artifact.image_id", OutputTextPlane::Metadata),
            ("image_artifact.path", OutputTextPlane::Metadata),
            ("image_artifact.content_hash", OutputTextPlane::Metadata),
            ("image_artifact.mime_type", OutputTextPlane::Metadata),
            ("language", OutputTextPlane::Metadata),
        ])
    }

    fn from_fields(fields: &[(&str, OutputTextPlane)]) -> Self {
        let mut taxonomy = Self {
            version: Self::VERSION,
            fields: Vec::with_capacity(fields.len()),
        };
        for (field, plane) in fields {
            taxonomy.push((*field).to_string(), *plane);
        }
        taxonomy
    }

    fn push(&mut self, field: impl Into<String>, plane: OutputTextPlane) {
        self.fields.push(TextFieldTaxonomy {
            field: field.into(),
            plane,
        });
    }

    pub(crate) fn from_serialized_value(value: &Value) -> Self {
        let mut fields = Vec::new();
        collect_serialized_string_leaves(value, value, "", &mut fields);
        let mut seen = HashSet::new();
        fields.retain(|field| seen.insert(field.field.clone()));
        Self {
            version: Self::VERSION,
            fields,
        }
    }
}

const RETRIEVE_EVIDENCE_PACK_IDENTITY_FIELDS: &[&str] = &[
    "evidence_pack.evidence_unit_ids[]",
    "evidence_pack.header.generation",
    "evidence_pack.header.identity.artifact_id",
    "evidence_pack.header.identity.content_hash",
    "evidence_pack.header.identity.kind",
    "evidence_pack.header.profile_ref",
    "evidence_pack.query_plan_hash",
];

const ASK_CONTEXT_PACK_IDENTITY_FIELDS: &[&str] = &[
    "context_pack.evidence_pack_hash",
    "context_pack.header.generation",
    "context_pack.header.identity.artifact_id",
    "context_pack.header.identity.content_hash",
    "context_pack.header.identity.kind",
    "context_pack.header.profile_ref",
    "context_pack.selected_unit_ids[]",
];

fn collect_serialized_string_leaves(
    root: &Value,
    value: &Value,
    path: &str,
    fields: &mut Vec<TextFieldTaxonomy>,
) {
    match value {
        Value::Object(object) => {
            let synthesize_pack = should_synthesize_retrieve_evidence_pack(object);
            let synthesize_context_pack = should_synthesize_ask_context_pack(object);
            let mut emitted_pack = false;
            let mut emitted_context_pack = false;
            for (key, child) in object {
                if synthesize_pack && !emitted_pack && key.as_str() > "evidence_pack" {
                    push_retrieve_evidence_pack_identity_fields(path, fields);
                    emitted_pack = true;
                }
                if synthesize_context_pack && !emitted_context_pack && key.as_str() > "context_pack"
                {
                    push_ask_context_pack_identity_fields(path, fields);
                    emitted_context_pack = true;
                }
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if child_path == "text_taxonomy"
                    || child_path.split('.').any(|part| part == "text_taxonomy")
                {
                    continue;
                }
                collect_serialized_string_leaves(root, child, &child_path, fields);
            }
            if synthesize_pack && !emitted_pack {
                push_retrieve_evidence_pack_identity_fields(path, fields);
            }
            if synthesize_context_pack && !emitted_context_pack {
                push_ask_context_pack_identity_fields(path, fields);
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_serialized_string_leaves(root, child, &format!("{path}[{index}]"), fields);
            }
        }
        Value::String(_) => {
            let field = if path.ends_with(".text_preview") || path.ends_with(".snippet") {
                path.to_string()
            } else {
                normalize_array_indices(path)
            };
            fields.push(TextFieldTaxonomy {
                plane: text_plane_for_serialized_path(root, path),
                field,
            });
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn should_synthesize_retrieve_evidence_pack(object: &serde_json::Map<String, Value>) -> bool {
    !object.contains_key("evidence_pack") && has_non_blank_retrieve_result_ids(object)
}

fn should_synthesize_ask_context_pack(object: &serde_json::Map<String, Value>) -> bool {
    !object.contains_key("context_pack")
        && object
            .get("context")
            .and_then(Value::as_object)
            .is_some_and(has_non_blank_retrieve_result_ids)
}

fn has_non_blank_retrieve_result_ids(object: &serde_json::Map<String, Value>) -> bool {
    object.get("query").and_then(Value::as_str).is_some()
        && object
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|results| {
                !results.is_empty()
                    && results.iter().all(|result| {
                        result
                            .get("evidence_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| !id.trim().is_empty())
                    })
            })
}

fn push_retrieve_evidence_pack_identity_fields(path: &str, fields: &mut Vec<TextFieldTaxonomy>) {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}.")
    };
    for field in RETRIEVE_EVIDENCE_PACK_IDENTITY_FIELDS {
        fields.push(TextFieldTaxonomy {
            field: format!("{prefix}{field}"),
            plane: OutputTextPlane::Metadata,
        });
    }
}

fn push_ask_context_pack_identity_fields(path: &str, fields: &mut Vec<TextFieldTaxonomy>) {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}.")
    };
    for field in ASK_CONTEXT_PACK_IDENTITY_FIELDS {
        fields.push(TextFieldTaxonomy {
            field: format!("{prefix}{field}"),
            plane: OutputTextPlane::Metadata,
        });
    }
}

fn text_plane_for_serialized_path(root: &Value, path: &str) -> OutputTextPlane {
    if path == "answer" || path_has_component(path, "generated_interpretation") {
        return OutputTextPlane::GeneratedInterpretation;
    }
    if path.ends_with(".text_preview") {
        return text_plane_for_kind(
            lookup_path(root, &replace_last_component(path, "text_preview", "kind"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
            None,
        );
    }
    if path.ends_with(".snippet") {
        let kind = lookup_path(root, &replace_last_component(path, "snippet", "kind"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let role = lookup_path(root, &replace_last_component(path, "snippet", "role"))
            .and_then(Value::as_str);
        return text_plane_for_kind(kind, role);
    }
    if path_has_component(path, "collection_filter") && path_has_component(path, "warnings") {
        return OutputTextPlane::DeterministicInterfaceText;
    }
    if path.ends_with(".label")
        && (path_has_component(path, "citations")
            || path_has_component(path, "results")
            || path_has_component(path, "final_evidence_pack")
            || path_has_component(path, "display_evidence_pack"))
    {
        return OutputTextPlane::DeterministicInterfaceText;
    }
    if (path == "text" || path_has_component(path, "heading_path"))
        && !path_has_component(path, "structured_locator")
    {
        let kind = lookup_path(
            root,
            &replace_last_component(path, path.rsplit('.').next().unwrap_or(path), "kind"),
        )
        .and_then(Value::as_str)
        .unwrap_or_default();
        let source_bounded = lookup_path(
            root,
            &replace_last_component(
                path,
                path.rsplit('.').next().unwrap_or(path),
                "source_bounded",
            ),
        )
        .and_then(Value::as_bool)
        .unwrap_or(false);
        return text_plane_for_evidence_kind(kind, source_bounded);
    }
    OutputTextPlane::Metadata
}

fn path_has_component(path: &str, component: &str) -> bool {
    path.split('.')
        .any(|part| part.split('[').next() == Some(component))
}

fn replace_last_component(path: &str, component: &str, replacement: &str) -> String {
    path.strip_suffix(component)
        .map(|prefix| format!("{prefix}{replacement}"))
        .unwrap_or_else(|| path.to_string())
}

fn lookup_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = root;
    for component in path.split('.') {
        let (key, index) = component
            .strip_suffix(']')
            .and_then(|component| component.rsplit_once('['))
            .map(|(key, index)| (key, index.parse::<usize>().ok()))
            .unwrap_or((component, None));
        current = current.get(key)?;
        if let Some(index) = index {
            current = current.get(index)?;
        }
    }
    Some(current)
}

fn normalize_array_indices(path: &str) -> String {
    let mut normalized = String::new();
    let mut chars = path.chars();
    while let Some(character) = chars.next() {
        if character != '[' {
            normalized.push(character);
            continue;
        }
        for character in chars.by_ref() {
            if character == ']' {
                break;
            }
        }
        normalized.push_str("[]");
    }
    normalized
}

fn text_plane_for_kind(kind: &str, role: Option<&str>) -> OutputTextPlane {
    match role {
        None => match kind {
            "original_text" | "text" => OutputTextPlane::Evidence,
            "image" | "image_artifact" => OutputTextPlane::Metadata,
            _ => OutputTextPlane::GeneratedInterpretation,
        },
        Some("original_text") if kind == "text" => OutputTextPlane::Evidence,
        Some("image_artifact") if kind == "image" => OutputTextPlane::Metadata,
        _ => OutputTextPlane::GeneratedInterpretation,
    }
}

fn text_plane_for_evidence_kind(kind: &str, source_bounded: bool) -> OutputTextPlane {
    if !source_bounded {
        return OutputTextPlane::GeneratedInterpretation;
    }
    match kind {
        "text" => OutputTextPlane::Evidence,
        "image" | "image_artifact" => OutputTextPlane::Metadata,
        _ => OutputTextPlane::GeneratedInterpretation,
    }
}
