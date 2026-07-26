//! Canonical identity and content-hash hooks for wire envelopes.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::common::{
    validate_wire_schema_version, WireArtifactKind, WireSchemaVersion, WIRE_SCHEMA_VERSION,
};
use super::ser::wire_content_hash;

/// Opaque non-empty content hash (hex SHA-256 preferred).
///
/// Whitespace and empty digests are rejected so identity cannot collapse to a
/// blank key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        validate_content_hash(&raw)?;
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn validate(&self) -> Result<()> {
        validate_content_hash(&self.0)
    }
}

impl AsRef<str> for ContentHash {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

fn validate_content_hash(digest: &str) -> Result<()> {
    if digest.trim().is_empty() {
        bail!("content hash must not be empty");
    }
    if digest.chars().any(|c| c.is_whitespace()) {
        bail!("content hash must not contain whitespace");
    }
    Ok(())
}

/// Canonical identity for a public wire artifact.
///
/// Identity is kind + schema version + artifact id + content hash. Equivalent
/// values produce the same identity; adapters must not key caches or signatures
/// on ad-hoc Rust struct layout.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalIdentity {
    pub kind: WireArtifactKind,
    pub schema_version: WireSchemaVersion,
    /// Stable artifact id (opaque, non-empty).
    pub artifact_id: String,
    /// Content-addressed hash of the canonical payload body.
    pub content_hash: ContentHash,
}

/// Field bundle for [`CanonicalIdentity::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalIdentityFields {
    pub kind: WireArtifactKind,
    pub schema_version: WireSchemaVersion,
    pub artifact_id: String,
    pub content_hash: String,
}

impl CanonicalIdentity {
    pub fn new(fields: CanonicalIdentityFields) -> Result<Self> {
        let artifact_id = fields.artifact_id;
        if artifact_id.trim().is_empty() {
            bail!("artifact_id must not be empty");
        }
        validate_wire_schema_version(fields.schema_version)?;
        let content_hash = ContentHash::new(fields.content_hash)?;
        Ok(Self {
            kind: fields.kind,
            schema_version: fields.schema_version,
            artifact_id,
            content_hash,
        })
    }

    /// Build identity from body bytes using [`wire_content_hash`].
    pub fn from_body(
        kind: WireArtifactKind,
        schema_version: WireSchemaVersion,
        artifact_id: impl Into<String>,
        body_bytes: &[u8],
    ) -> Result<Self> {
        Self::new(CanonicalIdentityFields {
            kind,
            schema_version,
            artifact_id: artifact_id.into(),
            content_hash: wire_content_hash(body_bytes),
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.artifact_id.trim().is_empty() {
            bail!("artifact_id must not be empty");
        }
        validate_wire_schema_version(self.schema_version)?;
        self.content_hash.validate()?;
        Ok(())
    }
}

/// Shared header present on every wire envelope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WireEnvelopeHeader {
    /// Envelope document schema version (must equal [`WIRE_SCHEMA_VERSION`]).
    pub schema_version: WireSchemaVersion,
    pub identity: CanonicalIdentity,
    /// Optional generation fence (source/index/publication generation id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Optional retrieval/embed profile reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
}

/// Field bundle for [`WireEnvelopeHeader::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WireEnvelopeHeaderFields {
    pub identity: CanonicalIdentity,
    pub generation: Option<String>,
    pub profile_ref: Option<String>,
}

impl WireEnvelopeHeader {
    pub fn new(fields: WireEnvelopeHeaderFields) -> Result<Self> {
        fields.identity.validate()?;
        if fields.identity.schema_version != WIRE_SCHEMA_VERSION {
            bail!(
                "identity schema_version {} must match wire envelope {}",
                fields.identity.schema_version,
                WIRE_SCHEMA_VERSION
            );
        }
        if let Some(gen) = fields.generation.as_ref() {
            if gen.trim().is_empty() {
                bail!("generation must not be empty when present");
            }
        }
        if let Some(profile) = fields.profile_ref.as_ref() {
            if profile.trim().is_empty() {
                bail!("profile_ref must not be empty when present");
            }
        }
        Ok(Self {
            schema_version: WIRE_SCHEMA_VERSION,
            identity: fields.identity,
            generation: fields.generation,
            profile_ref: fields.profile_ref,
        })
    }

    pub fn validate(&self) -> Result<()> {
        validate_wire_schema_version(self.schema_version)?;
        self.identity.validate()?;
        if self.identity.schema_version != self.schema_version {
            bail!(
                "identity schema_version {} mismatches header {}",
                self.identity.schema_version,
                self.schema_version
            );
        }
        if let Some(gen) = self.generation.as_ref() {
            if gen.trim().is_empty() {
                bail!("generation must not be empty when present");
            }
        }
        if let Some(profile) = self.profile_ref.as_ref() {
            if profile.trim().is_empty() {
                bail!("profile_ref must not be empty when present");
            }
        }
        Ok(())
    }
}
