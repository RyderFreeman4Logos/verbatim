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
            ("citations[].metadata", OutputTextPlane::Metadata),
            ("collection_filter", OutputTextPlane::Metadata),
            ("retrieval", OutputTextPlane::Metadata),
        ])
    }

    pub fn retrieve_response() -> Self {
        Self::from_fields(&[
            ("results[].snippet", OutputTextPlane::Evidence),
            (
                "results[].label",
                OutputTextPlane::DeterministicInterfaceText,
            ),
            ("metadata", OutputTextPlane::Metadata),
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
            ("metadata", OutputTextPlane::Metadata),
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
