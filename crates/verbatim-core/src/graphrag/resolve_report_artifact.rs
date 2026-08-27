use super::*;
use crate::wire_schemas::{
    CanonicalIdentity, CanonicalIdentityFields, DerivedArtifactKind, WireArtifactKind,
    WireSchemaVersion, WIRE_SCHEMA_VERSION,
};
use anyhow::bail;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

/// Reconstructed manifest for a derived GraphRAG report artifact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportArtifactManifest {
    pub id: ReportArtifactId,
    pub schema_version: WireSchemaVersion,
    /// Canonical identity for this public derived artifact.
    pub identity: CanonicalIdentity,
    /// Explicitly classifies this report as derived rather than source evidence.
    pub derived_kind: DerivedArtifactKind,
    pub generation: String,
    pub content_hash: String,
    pub report: CommunityReport,
}

#[derive(Deserialize)]
struct ReportArtifactManifestRaw {
    id: ReportArtifactId,
    schema_version: WireSchemaVersion,
    identity: CanonicalIdentity,
    derived_kind: DerivedArtifactKind,
    generation: String,
    content_hash: String,
    report: CommunityReport,
}

impl ReportArtifactManifest {
    pub fn validate(&self) -> Result<()> {
        if self.generation.trim().is_empty() {
            bail!("generation must not be empty");
        }
        let expected_identity = CanonicalIdentity::new(CanonicalIdentityFields {
            kind: WireArtifactKind::DerivedArtifact,
            schema_version: self.schema_version,
            artifact_id: self.id.as_str().to_string(),
            content_hash: self.report.recompute_content_hash()?,
        })?;
        self.identity.validate()?;
        if self.identity != expected_identity {
            bail!("identity does not match report artifact manifest");
        }
        if self.derived_kind != DerivedArtifactKind::GraphReport {
            bail!("derived_kind must be graph_report");
        }
        let expected_id = ReportArtifactId::new(&self.report.id)?;
        if self.id != expected_id {
            bail!("artifact id does not match embedded report");
        }
        if self.generation != self.report.generation {
            bail!("generation does not match embedded report");
        }
        if self.content_hash != self.report.content_hash {
            bail!("content hash does not match embedded report");
        }
        if self.content_hash != expected_identity.content_hash.as_str() {
            bail!("content hash does not match recomputed report hash");
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ReportArtifactManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ReportArtifactManifestRaw::deserialize(deserializer)?;
        let value = Self {
            id: raw.id,
            schema_version: raw.schema_version,
            identity: raw.identity,
            derived_kind: raw.derived_kind,
            generation: raw.generation,
            content_hash: raw.content_hash,
            report: raw.report,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl GraphRagService<'_> {
    /// Reconstruct a derived report artifact from the current graph state.
    pub fn resolve_report_artifact(
        &self,
        id: &ReportArtifactId,
    ) -> Result<Option<ReportArtifactManifest>> {
        for source_filter in std::iter::once(None).chain(
            self.store
                .list_sources()?
                .iter()
                .map(|source| Some(&source.id)),
        ) {
            if let Some(report) =
                self.community_reports(source_filter)?
                    .into_iter()
                    .find(|report| {
                        ReportArtifactId::new(&report.id).is_ok_and(|report_id| report_id == *id)
                    })
            {
                let content_hash = report.recompute_content_hash()?;
                let manifest = ReportArtifactManifest {
                    id: id.clone(),
                    schema_version: WIRE_SCHEMA_VERSION,
                    identity: CanonicalIdentity::new(CanonicalIdentityFields {
                        kind: WireArtifactKind::DerivedArtifact,
                        schema_version: WIRE_SCHEMA_VERSION,
                        artifact_id: id.as_str().to_string(),
                        content_hash: content_hash.clone(),
                    })?,
                    derived_kind: DerivedArtifactKind::GraphReport,
                    generation: report.generation.clone(),
                    content_hash,
                    report,
                };
                let stored_payload = self
                    .store
                    .connection()
                    .query_row(
                        "SELECT payload_json FROM report_artifacts
                         WHERE report_id = ?1 AND generation = ?2 AND content_hash = ?3",
                        [
                            manifest.id.as_str(),
                            manifest.generation.as_str(),
                            manifest.content_hash.as_str(),
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if let Some(payload) = stored_payload {
                    let manifest = ReportArtifactManifest {
                        report: serde_json::from_str(&payload)?,
                        ..manifest
                    };
                    manifest.validate()?;
                    return Ok(Some(manifest));
                }
                self.store.connection().execute(
                    "INSERT INTO report_artifacts (report_id, generation, content_hash, payload_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    [
                        manifest.id.as_str(),
                        manifest.generation.as_str(),
                        manifest.content_hash.as_str(),
                        &serde_json::to_string(&manifest.report)?,
                    ],
                )?;
                return Ok(Some(manifest));
            }
        }
        Ok(None)
    }
}
