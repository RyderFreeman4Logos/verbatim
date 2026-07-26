//! Generic DerivedArtifact envelope (generated/derived text, not direct evidence).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::common::WireArtifactKind;
use super::identity::{CanonicalIdentity, WireEnvelopeHeader, WireEnvelopeHeaderFields};
use super::ser::{encode_wire_document, wire_content_hash};

/// Classification of a derived (non-source) artifact.
///
/// Keeps generated/draft/report material out of direct-evidence identity space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedArtifactKind {
    /// Model-generated draft answer (unverified).
    DraftAnswer,
    /// Graph extraction / report output.
    GraphReport,
    /// Summarization or expansion derived from evidence.
    Summary,
    /// Other opaque derived product.
    Other,
}

impl DerivedArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DraftAnswer => "draft_answer",
            Self::GraphReport => "graph_report",
            Self::Summary => "summary",
            Self::Other => "other",
        }
    }
}

/// Minimal DerivedArtifact wire envelope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DerivedArtifactEnvelope {
    pub header: WireEnvelopeHeader,
    pub kind: DerivedArtifactKind,
    /// Content hash of the ContextPack (or evidence pack) this was derived from.
    pub source_pack_hash: String,
    /// Opaque model/deployment fingerprint.
    pub model_fingerprint: String,
}

/// Field bundle for [`DerivedArtifactEnvelope::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DerivedArtifactFields {
    pub artifact_id: String,
    pub kind: DerivedArtifactKind,
    pub source_pack_hash: String,
    pub model_fingerprint: String,
    pub generation: Option<String>,
    pub profile_ref: Option<String>,
}

impl DerivedArtifactEnvelope {
    pub fn new(fields: DerivedArtifactFields) -> Result<Self> {
        if fields.source_pack_hash.trim().is_empty() {
            bail!("source_pack_hash must not be empty");
        }
        if fields.source_pack_hash.chars().any(|c| c.is_whitespace()) {
            bail!("source_pack_hash must not contain whitespace");
        }
        if fields.model_fingerprint.trim().is_empty() {
            bail!("model_fingerprint must not be empty");
        }
        let body = DerivedBody {
            kind: fields.kind,
            source_pack_hash: fields.source_pack_hash.clone(),
            model_fingerprint: fields.model_fingerprint.clone(),
        };
        let body_bytes = encode_wire_document(&body)?;
        let identity = CanonicalIdentity::from_body(
            WireArtifactKind::DerivedArtifact,
            super::WIRE_SCHEMA_VERSION,
            fields.artifact_id,
            &body_bytes,
        )?;
        let header = WireEnvelopeHeader::new(WireEnvelopeHeaderFields {
            identity,
            generation: fields.generation,
            profile_ref: fields.profile_ref,
        })?;
        Ok(Self {
            header,
            kind: fields.kind,
            source_pack_hash: fields.source_pack_hash,
            model_fingerprint: fields.model_fingerprint,
        })
    }

    pub fn validate(&self) -> Result<()> {
        self.header.validate()?;
        if self.header.identity.kind != WireArtifactKind::DerivedArtifact {
            bail!(
                "derived artifact identity kind must be derived_artifact, got {}",
                self.header.identity.kind
            );
        }
        if self.source_pack_hash.trim().is_empty() {
            bail!("source_pack_hash must not be empty");
        }
        if self.source_pack_hash.chars().any(|c| c.is_whitespace()) {
            bail!("source_pack_hash must not contain whitespace");
        }
        if self.model_fingerprint.trim().is_empty() {
            bail!("model_fingerprint must not be empty");
        }
        let body = DerivedBody {
            kind: self.kind,
            source_pack_hash: self.source_pack_hash.clone(),
            model_fingerprint: self.model_fingerprint.clone(),
        };
        let bytes = encode_wire_document(&body)?;
        let actual = wire_content_hash(&bytes);
        if self.header.identity.content_hash.as_str() != actual {
            bail!(
                "content hash mismatch: declared {}, actual {actual}",
                self.header.identity.content_hash.as_str()
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DerivedBody {
    kind: DerivedArtifactKind,
    source_pack_hash: String,
    model_fingerprint: String,
}

/// Decode a JSON DerivedArtifact envelope and reject unknown schema / invalid fields.
pub fn decode_derived_artifact_envelope_json(bytes: &[u8]) -> Result<DerivedArtifactEnvelope> {
    let value: DerivedArtifactEnvelope = serde_json::from_slice(bytes)?;
    value.validate()?;
    Ok(value)
}
