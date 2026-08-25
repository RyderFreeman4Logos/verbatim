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
        CanonicalIdentity::new(CanonicalIdentityFields {
            kind: WireArtifactKind::DerivedArtifact,
            schema_version: self.schema_version,
            artifact_id: self.id.as_str().to_string(),
            content_hash: self.content_hash.clone(),
        })?;
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
                let manifest = ReportArtifactManifest {
                    id: id.clone(),
                    schema_version: WIRE_SCHEMA_VERSION,
                    derived_kind: DerivedArtifactKind::GraphReport,
                    generation: report.generation.clone(),
                    content_hash: report.content_hash.clone(),
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
                    return Ok(Some(ReportArtifactManifest {
                        report: serde_json::from_str(&payload)?,
                        ..manifest
                    }));
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
