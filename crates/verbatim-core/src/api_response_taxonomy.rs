use serde::{Deserialize, Serialize};

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
        Self::ask_response_with_citations(&[])
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
            ("collection_filter.warnings[]", OutputTextPlane::Metadata),
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
        Self::retrieve_response_with_results(&[])
    }

    pub fn retrieve_response_with_results(results: &[RetrieveResultResponse]) -> Self {
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
            ("collection_filter.warnings[]", OutputTextPlane::Metadata),
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
        let text_plane = if source_bounded {
            OutputTextPlane::Evidence
        } else {
            OutputTextPlane::GeneratedInterpretation
        };
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
}

fn text_plane_for_kind(kind: &str, role: Option<&str>) -> OutputTextPlane {
    if kind == "generated"
        || matches!(role, Some("generated" | "image_caption_generated"))
        || matches!(kind, "image_caption_generated")
    {
        OutputTextPlane::GeneratedInterpretation
    } else {
        OutputTextPlane::Evidence
    }
}
