use anyhow::{bail, Result};
use serde::{Deserialize, Deserializer};

use super::{ChunkId, GraphExpansionStep, RetrievalOrigin, RetrievalProvenance, SourceId};
use crate::types::report_artifact::ReportArtifactId;
use crate::wire_schemas::{
    CanonicalIdentity, CanonicalIdentityFields, WireArtifactKind, WireSchemaVersion,
};

impl RetrievalProvenance {
    pub fn validate(&self) -> Result<()> {
        let (Some(report_artifact_id), Some(schema_version), Some(generation), Some(content_hash)) = (
            self.report_artifact_id.as_ref(),
            self.report_artifact_schema_version,
            self.report_artifact_generation.as_deref(),
            self.report_artifact_content_hash.as_deref(),
        ) else {
            if self.report_artifact_id.is_none()
                && self.report_artifact_schema_version.is_none()
                && self.report_artifact_generation.is_none()
                && self.report_artifact_content_hash.is_none()
            {
                if self.origin == RetrievalOrigin::GraphReport {
                    bail!("graph report provenance must carry complete report identity");
                }
                return Ok(());
            }
            bail!("report artifact identity fields must be all present or all absent");
        };
        if matches!(
            self.origin,
            RetrievalOrigin::Seed | RetrievalOrigin::GraphExpansion
        ) {
            bail!("seed and graph expansion provenance must not carry report identity");
        }
        if generation.trim().is_empty() {
            bail!("generation must not be empty");
        }
        CanonicalIdentity::new(CanonicalIdentityFields {
            kind: WireArtifactKind::DerivedArtifact,
            schema_version,
            artifact_id: report_artifact_id.as_str().to_string(),
            content_hash: content_hash.to_string(),
        })?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct RetrievalProvenanceRaw {
    #[serde(default)]
    origin: RetrievalOrigin,
    #[serde(default)]
    report_artifact_id: Option<ReportArtifactId>,
    #[serde(default)]
    report_artifact_schema_version: Option<WireSchemaVersion>,
    #[serde(default)]
    report_artifact_generation: Option<String>,
    #[serde(default)]
    report_artifact_content_hash: Option<String>,
    #[serde(default)]
    result_rank: usize,
    #[serde(default)]
    seed_rank: Option<usize>,
    #[serde(default)]
    seed_chunk_id: Option<ChunkId>,
    #[serde(default)]
    seed_source_id: Option<SourceId>,
    #[serde(default)]
    hop_distance: u32,
    #[serde(default)]
    graph_path: Vec<GraphExpansionStep>,
}

impl<'de> Deserialize<'de> for RetrievalProvenance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RetrievalProvenanceRaw::deserialize(deserializer)?;
        let value = Self {
            origin: raw.origin,
            report_artifact_id: raw.report_artifact_id,
            report_artifact_schema_version: raw.report_artifact_schema_version,
            report_artifact_generation: raw.report_artifact_generation,
            report_artifact_content_hash: raw.report_artifact_content_hash,
            result_rank: raw.result_rank,
            seed_rank: raw.seed_rank,
            seed_chunk_id: raw.seed_chunk_id,
            seed_source_id: raw.seed_source_id,
            hop_distance: raw.hop_distance,
            graph_path: raw.graph_path,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}
