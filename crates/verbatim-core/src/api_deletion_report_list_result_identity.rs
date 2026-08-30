use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::deletion::PersistedDeletionReport;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq)]
pub struct DeletionReportListResponse {
    pub reports: Vec<PersistedDeletionReport>,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct DeletionReportListResponseBody<'a> {
    reports: &'a [PersistedDeletionReport],
}

fn deletion_report_list_result_identity(
    reports: &[PersistedDeletionReport],
) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::DeletionReportListResult,
        WIRE_SCHEMA_VERSION,
        "deletion-reports",
        &encode_wire_document(&DeletionReportListResponseBody { reports })?,
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeletionReportListResponseWire {
    reports: Vec<PersistedDeletionReport>,
    identity: CanonicalIdentity,
}

impl DeletionReportListResponse {
    pub fn new(reports: Vec<PersistedDeletionReport>) -> Result<Self> {
        let identity = deletion_report_list_result_identity(&reports)?;
        Ok(Self { reports, identity })
    }

    fn stamp_identity(&self) -> Result<CanonicalIdentity> {
        let expected = deletion_report_list_result_identity(&self.reports)?;
        self.identity.validate()?;
        if self.identity != expected {
            anyhow::bail!(
                "deletion-report-list identity does not match the deletion report list body"
            );
        }
        Ok(expected)
    }
}

impl Serialize for DeletionReportListResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = self.stamp_identity().map_err(serde::ser::Error::custom)?;
        DeletionReportListResponseWire {
            reports: self.reports.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeletionReportListResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = DeletionReportListResponseWire::deserialize(deserializer)?;
        let response = Self {
            reports: wire.reports,
            identity: wire.identity,
        };
        response
            .stamp_identity()
            .map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_deletion_report_list_result_identity_wire_tests.rs"]
mod wire_tests;
