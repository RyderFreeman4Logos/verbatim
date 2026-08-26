use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{EvidenceResponse, ImageArtifactResponse, OutputTextPlane, TextFieldTaxonomy};
use crate::types::SourceLocator;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EvidenceIdentityBody {
    pub id: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    pub source_bounded: bool,
    pub text_hash: String,
    pub kind: String,
    #[serde(default)]
    pub derived_from: Option<String>,
    pub locator: String,
    pub structured_locator: SourceLocator,
    pub text: String,
    pub heading_path: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub position: u32,
    #[serde(default)]
    pub image_artifact: Option<ImageArtifactResponse>,
}

impl EvidenceIdentityBody {
    pub(super) fn from_response(evidence: &EvidenceResponse) -> Self {
        Self {
            id: evidence.id.clone(),
            source_id: evidence.source_id.clone(),
            source_hash: evidence.source_hash.clone(),
            source_bounded: evidence.source_bounded,
            text_hash: evidence.text_hash.clone(),
            kind: evidence.kind.clone(),
            derived_from: evidence.derived_from.clone(),
            locator: evidence.locator.clone(),
            structured_locator: evidence.structured_locator.clone(),
            text: evidence.text.clone(),
            heading_path: evidence.heading_path.clone(),
            language: evidence.language.clone(),
            position: evidence.position,
            image_artifact: evidence.image_artifact.clone(),
        }
    }
}

pub(super) fn stamp_evidence_identity(
    body: &EvidenceIdentityBody,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::Evidence,
        WIRE_SCHEMA_VERSION,
        body.id.clone(),
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("evidence identity does not match the executed evidence row");
        }
    }
    Ok(expected)
}

const EVIDENCE_IDENTITY_FIELDS: &[&str] = &[
    "identity.artifact_id",
    "identity.content_hash",
    "identity.kind",
];

pub(super) struct EvidenceIdentityTaxonomy {
    synthesize: bool,
    emitted: bool,
}

impl EvidenceIdentityTaxonomy {
    pub(super) fn start(object: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            synthesize: should_synthesize_evidence_identity(object),
            emitted: false,
        }
    }

    pub(super) fn emit_before(
        &mut self,
        path: &str,
        key: &str,
        fields: &mut Vec<TextFieldTaxonomy>,
    ) {
        if self.synthesize && !self.emitted && key > "identity" {
            push_evidence_identity_fields(path, fields);
            self.emitted = true;
        }
    }

    pub(super) fn emit_after(&mut self, path: &str, fields: &mut Vec<TextFieldTaxonomy>) {
        if self.synthesize && !self.emitted {
            push_evidence_identity_fields(path, fields);
            self.emitted = true;
        }
    }
}

fn should_synthesize_evidence_identity(
    object: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    !object.contains_key("identity")
        && object.contains_key("text")
        && object.contains_key("text_hash")
        && object.contains_key("structured_locator")
        && object.contains_key("source_bounded")
        && !object.contains_key("query")
        && !object.contains_key("answer")
        && !object.contains_key("results")
}

fn push_evidence_identity_fields(path: &str, fields: &mut Vec<TextFieldTaxonomy>) {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}.")
    };
    for field in EVIDENCE_IDENTITY_FIELDS {
        fields.push(TextFieldTaxonomy {
            field: format!("{prefix}{field}"),
            plane: OutputTextPlane::Metadata,
        });
    }
}
