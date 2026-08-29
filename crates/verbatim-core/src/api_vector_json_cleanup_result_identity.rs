use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::VectorJsonCleanupReport;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorJsonCleanupResponse {
    pub dry_run: bool,
    pub report: VectorJsonCleanupReport,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct VectorJsonCleanupResponseBody<'a> {
    dry_run: bool,
    report: &'a VectorJsonCleanupReport,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorJsonCleanupResponseWire {
    dry_run: bool,
    report: VectorJsonCleanupReport,
    identity: CanonicalIdentity,
}

fn vector_json_cleanup_result_identity(
    dry_run: bool,
    report: &VectorJsonCleanupReport,
) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::VectorJsonCleanupResult,
        WIRE_SCHEMA_VERSION,
        "index-vector-json-cleanup",
        &encode_wire_document(&VectorJsonCleanupResponseBody { dry_run, report })?,
    )
}

fn validate_vector_json_cleanup_result_identity(
    response: &VectorJsonCleanupResponse,
) -> Result<()> {
    response.identity.validate()?;
    let expected = vector_json_cleanup_result_identity(response.dry_run, &response.report)?;
    if response.identity != expected {
        anyhow::bail!(
            "vector-json-cleanup-result identity does not match the cleanup response body"
        );
    }
    Ok(())
}

impl VectorJsonCleanupResponse {
    pub fn new(dry_run: bool, report: VectorJsonCleanupReport) -> Result<Self> {
        let identity = vector_json_cleanup_result_identity(dry_run, &report)?;
        Ok(Self {
            dry_run,
            report,
            identity,
        })
    }
}

impl Serialize for VectorJsonCleanupResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_vector_json_cleanup_result_identity(self).map_err(serde::ser::Error::custom)?;
        VectorJsonCleanupResponseWire {
            dry_run: self.dry_run,
            report: self.report,
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for VectorJsonCleanupResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = VectorJsonCleanupResponseWire::deserialize(deserializer)?;
        let response = Self {
            dry_run: wire.dry_run,
            report: wire.report,
            identity: wire.identity,
        };
        validate_vector_json_cleanup_result_identity(&response)
            .map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_vector_json_cleanup_result_identity_wire_tests.rs"]
mod api_vector_json_cleanup_result_identity_wire_tests;
