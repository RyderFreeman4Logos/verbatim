use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::api::{CheckStaleResponse, IndexStatusResponse};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize)]
struct CheckStaleResponseBody<'a> {
    stale: &'a [String],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_status: Option<&'a IndexStatusResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckStaleResponseWire {
    stale: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_status: Option<IndexStatusResponse>,
    identity: CanonicalIdentity,
}

fn check_stale_result_identity(
    stale: &[String],
    profile_status: Option<&IndexStatusResponse>,
) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::CheckStaleResult,
        WIRE_SCHEMA_VERSION,
        "sources-check",
        &encode_wire_document(&CheckStaleResponseBody {
            stale,
            profile_status,
        })?,
    )
}

fn expected_check_stale_result_identity(
    response: &CheckStaleResponse,
) -> Result<CanonicalIdentity> {
    check_stale_result_identity(&response.stale, response.profile_status.as_ref())
}

fn validate_check_stale_result_identity(response: &CheckStaleResponse) -> Result<()> {
    response.identity.validate()?;
    let expected = expected_check_stale_result_identity(response)?;
    if response.identity != expected {
        anyhow::bail!("check-stale-result identity does not match the check stale response body");
    }
    Ok(())
}

impl CheckStaleResponse {
    pub fn new(stale: Vec<String>, profile_status: Option<IndexStatusResponse>) -> Result<Self> {
        let identity = check_stale_result_identity(&stale, profile_status.as_ref())?;
        Ok(Self {
            stale,
            profile_status,
            identity,
        })
    }
}

impl Serialize for CheckStaleResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_check_stale_result_identity(self).map_err(serde::ser::Error::custom)?;
        CheckStaleResponseWire {
            stale: self.stale.clone(),
            profile_status: self.profile_status.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CheckStaleResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CheckStaleResponseWire::deserialize(deserializer)?;
        let response = Self {
            stale: wire.stale,
            profile_status: wire.profile_status,
            identity: wire.identity,
        };
        validate_check_stale_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
