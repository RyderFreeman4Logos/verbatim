use serde::{Deserialize, Serialize};

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
            (
                "generated_interpretation.text",
                OutputTextPlane::GeneratedInterpretation,
            ),
            (
                "citations[].label",
                OutputTextPlane::DeterministicInterfaceText,
            ),
            ("citations[].text_preview", OutputTextPlane::Evidence),
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
        ])
    }

    pub fn retrieve_response() -> Self {
        Self::from_fields(&[
            ("results[].snippet", OutputTextPlane::Evidence),
            (
                "results[].label",
                OutputTextPlane::DeterministicInterfaceText,
            ),
            ("task_id", OutputTextPlane::Metadata),
            ("query", OutputTextPlane::Metadata),
            ("source_id", OutputTextPlane::Metadata),
            ("embedding_profile_id", OutputTextPlane::Metadata),
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
            ("results[].derived_from", OutputTextPlane::Metadata),
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
        ])
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
            ("language", OutputTextPlane::Metadata),
        ])
    }

    fn from_fields(fields: &[(&str, OutputTextPlane)]) -> Self {
        Self {
            version: Self::VERSION,
            fields: fields
                .iter()
                .map(|(field, plane)| TextFieldTaxonomy {
                    field: (*field).into(),
                    plane: *plane,
                })
                .collect(),
        }
    }
}
