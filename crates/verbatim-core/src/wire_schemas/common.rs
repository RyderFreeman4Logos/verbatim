//! Shared schema version and artifact kind for wire envelopes.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Wire schema version for documents in this module.
///
/// Unknown versions must fail closed during serialization and deserialization.
pub const WIRE_SCHEMA_VERSION: WireSchemaVersion = WireSchemaVersion::new(1, 0, 0);

/// Semantic wire schema version (`major.minor.patch`).
///
/// Distinct from storage/migration schema stamps: this versions the public
/// artifact envelopes exchanged among clients, coordinator, storage, and
/// workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireSchemaVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSchemaVersionWire {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Serialize for WireSchemaVersion {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_wire_schema_version(*self).map_err(serde::ser::Error::custom)?;
        WireSchemaVersionWire {
            major: self.major,
            minor: self.minor,
            patch: self.patch,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WireSchemaVersion {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WireSchemaVersionWire::deserialize(deserializer)?;
        let version = Self::new(wire.major, wire.minor, wire.patch);
        validate_wire_schema_version(version).map_err(serde::de::Error::custom)?;
        Ok(version)
    }
}

impl WireSchemaVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub fn as_dotted(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }

    /// Parse `"major.minor.patch"`; rejects empty / non-numeric / extra parts.
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split('.');
        let major = parse_component(parts.next(), "major")?;
        let minor = parse_component(parts.next(), "minor")?;
        let patch = parse_component(parts.next(), "patch")?;
        if parts.next().is_some() {
            bail!("wire schema version must have exactly three components: {text}");
        }
        Ok(Self::new(major, minor, patch))
    }

    /// Whether this version is accepted by the current walking skeleton.
    ///
    /// Only exact match of [`WIRE_SCHEMA_VERSION`] is supported; dual-shape
    /// decode for older minors is residual.
    pub fn is_supported(self) -> bool {
        self == WIRE_SCHEMA_VERSION
    }
}

impl fmt::Display for WireSchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_dotted())
    }
}

fn parse_component(part: Option<&str>, name: &str) -> Result<u32> {
    let Some(raw) = part else {
        bail!("wire schema version missing {name} component");
    };
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        bail!("wire schema version {name} component is not a non-negative integer: {raw}");
    }
    raw.parse::<u32>()
        .map_err(|err| anyhow::anyhow!("wire schema version {name} overflow: {err}"))
}

/// Kind of public R/A/G or workflow wire artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireArtifactKind {
    QueryPlan,
    EvidencePack,
    ContextPack,
    DerivedArtifact,
    WorkflowEnvelope,
}

impl WireArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QueryPlan => "query_plan",
            Self::EvidencePack => "evidence_pack",
            Self::ContextPack => "context_pack",
            Self::DerivedArtifact => "derived_artifact",
            Self::WorkflowEnvelope => "workflow_envelope",
        }
    }
}

impl fmt::Display for WireArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reject unknown or unsupported wire schema versions.
pub fn validate_wire_schema_version(version: WireSchemaVersion) -> Result<()> {
    if !version.is_supported() {
        bail!("unsupported wire schema version {version}; expected {WIRE_SCHEMA_VERSION}");
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn wire_schema_version_parse_and_order() {
        assert_eq!(
            WireSchemaVersion::parse("1.0.0").unwrap(),
            WireSchemaVersion::new(1, 0, 0)
        );
        assert!(WireSchemaVersion::parse("1.0").is_err());
        assert!(WireSchemaVersion::parse("1.0.0.1").is_err());
        assert!(WireSchemaVersion::parse("a.b.c").is_err());
        assert!(WireSchemaVersion::new(1, 0, 0) < WireSchemaVersion::new(1, 1, 0));
        assert_eq!(WIRE_SCHEMA_VERSION.as_dotted(), "1.0.0");
    }

    #[test]
    fn unsupported_version_fails_closed() {
        let err = validate_wire_schema_version(WireSchemaVersion::new(9, 0, 0))
            .expect_err("must refuse unknown version");
        assert!(err.to_string().contains("unsupported"), "{err}");
    }
}
