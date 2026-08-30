use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::deletion::{DeletionReport, PersistedDeletionReport, RetentionPolicy};
use crate::types::SourceId;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

/// Identity-bound public result for a pending source-deletion request.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletionReportResponse {
    pub source_id: SourceId,
    pub recorded_at: String,
    pub retention_policy: RetentionPolicy,
    pub report: DeletionReport,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct DeletionReportResponseBody<'a> {
    source_id: &'a SourceId,
    recorded_at: &'a str,
    retention_policy: RetentionPolicy,
    report: &'a DeletionReport,
}

fn deletion_report_result_identity(
    source_id: &SourceId,
    recorded_at: &str,
    retention_policy: RetentionPolicy,
    report: &DeletionReport,
) -> Result<CanonicalIdentity> {
    let body = DeletionReportResponseBody {
        source_id,
        recorded_at,
        retention_policy,
        report,
    };
    CanonicalIdentity::from_body(
        WireArtifactKind::DeletionReportResult,
        WIRE_SCHEMA_VERSION,
        &source_id.0,
        &encode_wire_document(&body)?,
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeletionReportResponseWire {
    source_id: SourceId,
    recorded_at: String,
    retention_policy: RetentionPolicy,
    report: DeletionReport,
    identity: CanonicalIdentity,
}

impl DeletionReportResponse {
    pub fn new(receipt: PersistedDeletionReport) -> Result<Self> {
        let identity = deletion_report_result_identity(
            &receipt.source_id,
            &receipt.recorded_at,
            receipt.retention_policy,
            &receipt.report,
        )?;
        Ok(Self {
            source_id: receipt.source_id,
            recorded_at: receipt.recorded_at,
            retention_policy: receipt.retention_policy,
            report: receipt.report,
            identity,
        })
    }

    fn stamp_identity(&self) -> Result<CanonicalIdentity> {
        let expected = deletion_report_result_identity(
            &self.source_id,
            &self.recorded_at,
            self.retention_policy,
            &self.report,
        )?;
        self.identity.validate()?;
        if self.identity != expected {
            anyhow::bail!("deletion-report identity does not match the deletion report body");
        }
        Ok(expected)
    }
}

impl Serialize for DeletionReportResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = self.stamp_identity().map_err(serde::ser::Error::custom)?;
        DeletionReportResponseWire {
            source_id: self.source_id.clone(),
            recorded_at: self.recorded_at.clone(),
            retention_policy: self.retention_policy,
            report: self.report.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeletionReportResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = DeletionReportResponseWire::deserialize(deserializer)?;
        let response = Self {
            source_id: wire.source_id,
            recorded_at: wire.recorded_at,
            retention_policy: wire.retention_policy,
            report: wire.report,
            identity: wire.identity,
        };
        response
            .stamp_identity()
            .map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
