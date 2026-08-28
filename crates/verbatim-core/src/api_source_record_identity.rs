use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::types::SourceIngestDiagnostics;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq)]
pub struct SourceResponse {
    pub id: String,
    pub path: String,
    pub status: String,
    pub hash: String,
    pub parser_used: Option<String>,
    pub last_ingested_at: Option<String>,
    pub diagnostics: Option<SourceIngestDiagnostics>,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Clone, Serialize)]
struct SourceResponseBody<'a> {
    id: &'a str,
    path: &'a str,
    status: &'a str,
    hash: &'a str,
    parser_used: Option<&'a str>,
    last_ingested_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<&'a SourceIngestDiagnostics>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SourceResponseWire {
    id: String,
    path: String,
    status: String,
    hash: String,
    parser_used: Option<String>,
    last_ingested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diagnostics: Option<SourceIngestDiagnostics>,
    identity: CanonicalIdentity,
}

fn source_record_identity(
    id: &str,
    path: &str,
    status: &str,
    hash: &str,
    parser_used: Option<&str>,
    last_ingested_at: Option<&str>,
    diagnostics: Option<&SourceIngestDiagnostics>,
) -> Result<CanonicalIdentity> {
    let body = SourceResponseBody {
        id,
        path,
        status,
        hash,
        parser_used,
        last_ingested_at,
        diagnostics,
    };
    CanonicalIdentity::from_body(
        WireArtifactKind::SourceRecord,
        WIRE_SCHEMA_VERSION,
        id,
        &encode_wire_document(&body)?,
    )
}

fn expected_source_record_identity(source: &SourceResponse) -> Result<CanonicalIdentity> {
    source_record_identity(
        &source.id,
        &source.path,
        &source.status,
        &source.hash,
        source.parser_used.as_deref(),
        source.last_ingested_at.as_deref(),
        source.diagnostics.as_ref(),
    )
}

fn validate_source_record_identity(source: &SourceResponse) -> Result<()> {
    source.identity.validate()?;
    let expected = expected_source_record_identity(source)?;
    if source.identity != expected {
        anyhow::bail!("source-record identity does not match the source-record response body");
    }
    Ok(())
}

impl SourceResponse {
    pub fn new(
        id: impl Into<String>,
        path: impl Into<String>,
        status: impl Into<String>,
        hash: impl Into<String>,
        parser_used: Option<String>,
        last_ingested_at: Option<String>,
        diagnostics: Option<SourceIngestDiagnostics>,
    ) -> Result<Self> {
        let id = id.into();
        let path = path.into();
        let status = status.into();
        let hash = hash.into();
        let identity = source_record_identity(
            &id,
            &path,
            &status,
            &hash,
            parser_used.as_deref(),
            last_ingested_at.as_deref(),
            diagnostics.as_ref(),
        )?;
        Ok(Self {
            id,
            path,
            status,
            hash,
            parser_used,
            last_ingested_at,
            diagnostics,
            identity,
        })
    }
}

impl Serialize for SourceResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_source_record_identity(self).map_err(serde::ser::Error::custom)?;
        SourceResponseWire {
            id: self.id.clone(),
            path: self.path.clone(),
            status: self.status.clone(),
            hash: self.hash.clone(),
            parser_used: self.parser_used.clone(),
            last_ingested_at: self.last_ingested_at.clone(),
            diagnostics: self.diagnostics.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SourceResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let source = SourceResponseWire::deserialize(deserializer)?;
        let source = Self {
            id: source.id,
            path: source.path,
            status: source.status,
            hash: source.hash,
            parser_used: source.parser_used,
            last_ingested_at: source.last_ingested_at,
            diagnostics: source.diagnostics,
            identity: source.identity,
        };
        validate_source_record_identity(&source).map_err(serde::de::Error::custom)?;
        Ok(source)
    }
}
