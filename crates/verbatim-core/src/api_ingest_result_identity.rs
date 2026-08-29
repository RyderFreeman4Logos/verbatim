use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize)]
struct IngestResponseBody {
    ingested: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct IngestResponseWire {
    ingested: usize,
    identity: CanonicalIdentity,
}

fn ingest_result_identity(ingested: usize) -> Result<CanonicalIdentity> {
    let body = IngestResponseBody { ingested };
    CanonicalIdentity::from_body(
        WireArtifactKind::IngestResult,
        WIRE_SCHEMA_VERSION,
        "ingest-result",
        &encode_wire_document(&body)?,
    )
}

fn expected_ingest_result_identity(response: &super::IngestResponse) -> Result<CanonicalIdentity> {
    ingest_result_identity(response.ingested)
}

fn validate_ingest_result_identity(response: &super::IngestResponse) -> Result<()> {
    response.identity.validate()?;
    let expected = expected_ingest_result_identity(response)?;
    if response.identity != expected {
        anyhow::bail!("ingest-result identity does not match the ingest response body");
    }
    Ok(())
}

impl super::IngestResponse {
    pub fn new(ingested: usize) -> Result<Self> {
        Ok(Self {
            ingested,
            identity: ingest_result_identity(ingested)?,
        })
    }
}

impl Serialize for super::IngestResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_ingest_result_identity(self).map_err(serde::ser::Error::custom)?;
        IngestResponseWire {
            ingested: self.ingested,
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for super::IngestResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let response = IngestResponseWire::deserialize(deserializer)?;
        let response = Self {
            ingested: response.ingested,
            identity: response.identity,
        };
        validate_ingest_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
