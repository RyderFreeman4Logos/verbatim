//! Versioned publication manifest with comprehensive DiskANN3 vector-backend
//! metadata (Refs #379).
//!
//! This manifest extends the cross-index generic publication document
//! (`index_publication`) with the vector-backend-specific fields required by
//! issue #379: DiskANN3 version/source revision, provider/layout identity,
//! vector-space/document-encoder identity, metric/normalization/dimension/
//! encoding, shard list with ranges/counts/sizes/hashes, graph/build
//! parameters, candidate quantizer identity, exact-vector and ID-map hashes,
//! predicate/filter schema and ACL policy generation, update/tombstone
//! checkpoint, sampled recall report, build resource/telemetry report, and
//! compatibility / minimum reader version.

use serde::{Deserialize, Serialize};

use super::error::{
    GenerationPublicationDiagnosticCode, GenerationPublicationError, GenerationPublicationResult,
};
use super::identity::{ContentHash, PublicationGenerationId, ShardOrdinal};

/// Wire schema version for generation-publication manifest documents.
///
/// Unknown versions fail closed on decode rather than being accepted as
/// current-schema manifests.
pub const GENERATION_PUBLICATION_SCHEMA_VERSION: u32 = 1;

/// Vector distance metric bound into a published generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorMetric {
    Cosine,
    InnerProduct,
    L2,
}

impl VectorMetric {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::InnerProduct => "inner_product",
            Self::L2 => "l2",
        }
    }
}

/// Vector normalization applied before indexing and query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorNormalization {
    None,
    L2Unit,
}

/// On-disk encoding of the original float32 vectors stored alongside the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginalVectorEncoding {
    Float32,
    ScalarQuantized,
    ProductQuantized,
}

/// DiskANN3 build provider / page-layout identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorBackendProvider {
    /// Standard DiskANN3 with separate graph and vector pages.
    DiskAnn3Standard,
    /// AISAQ-style co-located graph and vectors.
    DiskAnn3Aisaq,
    /// External Qdrant backend (comparison / fallback).
    Qdrant,
    /// External LanceDB backend (comparison / fallback).
    LanceDb,
}

impl VectorBackendProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiskAnn3Standard => "diskann3_standard",
            Self::DiskAnn3Aisaq => "diskann3_aisaq",
            Self::Qdrant => "qdrant",
            Self::LanceDb => "lancedb",
        }
    }
}

/// Candidate-vector compression representation. Original float32 vectors stay
/// authoritative and are separately hashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateQuantizer {
    None,
    ScalarQuantized,
    ProductQuantized,
}

/// Lifecycle stage of a generation under the atomic publication model.
///
/// Mirrors the lifecycle in issue #379: snapshot fixed → stage → validate →
/// gates → manifest → promote → serve → retain → gc. Only `Ready` generations
/// may be promoted; only `Active` generations serve queries; `Retained`
/// generations remain readable under live leases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStage {
    /// Authoritative source snapshot is fixed; staging has not begun.
    SnapshotFixed,
    /// Artifacts are being written into the staging generation.
    Staging,
    /// fsync / checksum / structural validation in progress.
    Validating,
    /// Conformance and sampled quality gates passed; eligible for promotion.
    Ready,
    /// Currently the active, query-serving generation.
    Active,
    /// Previously active; retained for leases / cursors / rollback.
    Retained,
    /// Reclaimed after all leases expired and retention policy permitted GC.
    GarbageCollected,
    /// Isolated as incomplete / corrupt after startup reconciliation.
    Quarantined,
}

impl PublicationStage {
    /// Returns `true` if this stage is eligible to be promoted to active.
    pub const fn can_promote(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Returns `true` if this stage is durable on disk (fsync completed).
    pub const fn is_durable(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Active | Self::Retained | Self::GarbageCollected
        )
    }

    /// Returns `true` if this stage is search-visible (serves queries).
    pub const fn serves_queries(self) -> bool {
        matches!(self, Self::Active)
    }

    /// Returns `true` if this stage is still in-flight (pre-validation).
    pub const fn is_incomplete(self) -> bool {
        matches!(self, Self::SnapshotFixed | Self::Staging | Self::Validating)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SnapshotFixed => "snapshot_fixed",
            Self::Staging => "staging",
            Self::Validating => "validating",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::Retained => "retained",
            Self::GarbageCollected => "garbage_collected",
            Self::Quarantined => "quarantined",
        }
    }
}

/// One immutable shard descriptor within a published generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardDescriptor {
    pub ordinal: ShardOrdinal,
    /// Inclusive lower bound of the vector-id range in this shard.
    pub range_start: u64,
    /// Inclusive upper bound of the vector-id range in this shard.
    pub range_end: u64,
    pub vector_count: u64,
    pub byte_size: u64,
    pub graph_degree: u16,
    pub checksum: ContentHash,
}

impl ShardDescriptor {
    /// Validates that the range is non-empty, bounded, consistent with the
    /// vector count, and that the byte size can hold the float32 vectors.
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.range_start == 0
            || self.range_end < self.range_start
            || self.vector_count == 0
            || self.vector_count > (self.range_end - self.range_start + 1)
            || self.graph_degree == 0
            || self.graph_degree > 128
        {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        // Minimum float32 bytes: vector_count * dimension * 4 is enforced by the
        // dimension-aware validator on the manifest; here we bound byte_size > 0.
        if self.byte_size == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        self.checksum.validate()?;
        Ok(())
    }
}

/// Predicate / filter schema version and the ACL policy generation bound into
/// the publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterAclBinding {
    /// Versioned predicate / filter schema identifier.
    pub filter_schema_version: u32,
    /// ACL policy generation (must match the manifest's ACL policy generation).
    pub acl_policy_generation: u32,
}

impl FilterAclBinding {
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.filter_schema_version == 0 || self.acl_policy_generation == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(())
    }
}

/// Update / tombstone checkpoint bound into the publication. A generation
/// publishes a consistent checkpoint of durable updates; searches bound to this
/// generation never see a partial update stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckpoint {
    /// Last durable mutation version applied before this generation was sealed.
    pub last_mutation_version: u64,
    /// Tombstone generation at seal time.
    pub tombstone_generation: u64,
}

impl UpdateCheckpoint {
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.last_mutation_version == 0 || self.tombstone_generation == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(())
    }
}

/// Sampled exact-vs-ANN recall report required before promotion.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SampledRecallReport {
    /// Number of sampled queries used for the recall estimate.
    pub sample_size: u32,
    /// Mean recall@10 against exact scan over the same sampled queries.
    pub recall_at_10: f64,
    /// Worst-case (minimum) recall@10 across sampled queries.
    pub min_recall_at_10: f64,
}

impl SampledRecallReport {
    /// Default minimum mean recall@10 required before promotion.
    pub const DEFAULT_MIN_RECALL: f64 = 0.90;

    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.sample_size == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        if !(0.0..=1.0).contains(&self.recall_at_10)
            || !(0.0..=1.0).contains(&self.min_recall_at_10)
            || self.min_recall_at_10 > self.recall_at_10
        {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(())
    }

    /// Returns `true` if the recall meets the default promotion threshold.
    pub const fn meets_threshold(self) -> bool {
        self.recall_at_10 >= Self::DEFAULT_MIN_RECALL
    }
}

/// Build resource / telemetry report captured at seal time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BuildResourceReport {
    /// Peak resident memory in bytes during the build.
    pub peak_memory_bytes: u64,
    /// Wall-clock build duration in microseconds.
    pub build_duration_us: u64,
    /// Total SSD bytes written (graph + vectors + filters + ID map).
    pub ssd_bytes_written: u64,
    /// CPU-seconds consumed by the build.
    pub cpu_seconds: f64,
}

impl BuildResourceReport {
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.peak_memory_bytes == 0
            || self.build_duration_us == 0
            || self.ssd_bytes_written == 0
            || self.cpu_seconds <= 0.0
        {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(())
    }
}

/// Compatibility and minimum reader version contract for a published generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityContract {
    /// DiskANN3 semantic version string (e.g. "3.2.1").
    pub diskann3_version: String,
    /// Source revision / commit hash of the DiskANN3 build.
    pub source_revision: String,
    /// Minimum reader version required to safely read this generation.
    pub minimum_reader_version: u32,
}

impl CompatibilityContract {
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.diskann3_version.trim().is_empty()
            || self.source_revision.trim().is_empty()
            || self.minimum_reader_version == 0
        {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        Ok(())
    }
}

/// Full DiskANN3 generation publication manifest for one staged / published
/// generation (Refs #379).
///
/// This is the vector-backend-specific publication document that complements the
/// generic cross-index manifest (`index_publication`). A query binds to exactly
/// one publication generation; DiskANN3 graph, vectors, filters, ID map, and
/// evidence / lexical generations cannot be mixed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationManifest {
    pub schema_version: u32,
    pub generation: PublicationGenerationId,
    /// Vector space identity (sources are members, not independent indexes).
    pub vector_space_id: String,
    /// Document / embedding encoder profile identity.
    pub encoder_profile_id: String,
    /// Vector dimension (e.g. 768, 1024, 1536).
    pub dimension: u32,
    pub metric: VectorMetric,
    pub normalization: VectorNormalization,
    pub original_vector_encoding: OriginalVectorEncoding,
    pub provider: VectorBackendProvider,
    pub shards: Vec<ShardDescriptor>,
    /// Graph build parameters: max degree and search-list size.
    pub graph_max_degree: u16,
    pub build_search_list_size: u32,
    pub candidate_quantizer: CandidateQuantizer,
    /// Exact-vector file hash (original float32 vectors).
    pub exact_vector_hash: ContentHash,
    /// ID-map (external-id ↔ internal-id) file hash.
    pub id_map_hash: ContentHash,
    pub filter_acl: FilterAclBinding,
    pub update_checkpoint: UpdateCheckpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_recall: Option<SampledRecallReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_resources: Option<BuildResourceReport>,
    pub compatibility: CompatibilityContract,
    pub stage: PublicationStage,
    /// Wall-clock seal timestamp (RFC3339 or adapter-defined).
    pub sealed_at: String,
}

impl PublicationManifest {
    /// Validates all structural invariants of the manifest.
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.schema_version != GENERATION_PUBLICATION_SCHEMA_VERSION {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        if self.vector_space_id.trim().is_empty()
            || self.encoder_profile_id.trim().is_empty()
            || self.sealed_at.trim().is_empty()
        {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        if self.dimension == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        if self.graph_max_degree == 0
            || self.graph_max_degree > 128
            || self.build_search_list_size == 0
        {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        if self.shards.is_empty() {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::MissingComponent,
            ));
        }
        // Check for duplicate ordinals and validate each shard.
        let mut seen_ordinals = std::collections::HashSet::with_capacity(self.shards.len());
        let mut total_vectors: u64 = 0;
        for shard in &self.shards {
            if !seen_ordinals.insert(shard.ordinal.value()) {
                return Err(GenerationPublicationError::contract(
                    GenerationPublicationDiagnosticCode::DuplicateShard,
                ));
            }
            shard.validate()?;
            // Byte-size lower bound: vector_count * dimension * 4.
            let min_bytes = shard
                .vector_count
                .checked_mul(u64::from(self.dimension))
                .and_then(|b| b.checked_mul(4));
            match min_bytes {
                None => {
                    return Err(GenerationPublicationError::contract(
                        GenerationPublicationDiagnosticCode::InvalidBounds,
                    ));
                }
                Some(min) if shard.byte_size < min => {
                    return Err(GenerationPublicationError::contract(
                        GenerationPublicationDiagnosticCode::InvalidBounds,
                    ));
                }
                _ => {}
            }
            total_vectors = total_vectors.saturating_add(shard.vector_count);
        }
        if total_vectors == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::MissingComponent,
            ));
        }
        self.exact_vector_hash.validate()?;
        self.id_map_hash.validate()?;
        self.filter_acl.validate()?;
        self.update_checkpoint.validate()?;
        self.compatibility.validate()?;
        if let Some(recall) = self.sampled_recall {
            recall.validate()?;
        }
        if let Some(resources) = self.build_resources {
            resources.validate()?;
        }
        Ok(())
    }

    /// Returns the total vector count across all shards.
    pub fn total_vector_count(&self) -> u64 {
        self.shards.iter().map(|s| s.vector_count).sum()
    }
}

/// Decode a JSON publication manifest, failing closed on unknown schema.
pub fn decode_publication_manifest_json(
    input: &str,
) -> GenerationPublicationResult<PublicationManifest> {
    let manifest: PublicationManifest = serde_json::from_str(input).map_err(|_| {
        GenerationPublicationError::contract(
            GenerationPublicationDiagnosticCode::SerializationFailed,
        )
    })?;
    manifest.validate()?;
    Ok(manifest)
}

/// Encode a publication manifest as JSON after validation.
pub fn encode_publication_manifest_json(
    manifest: &PublicationManifest,
) -> GenerationPublicationResult<String> {
    manifest.validate()?;
    serde_json::to_string(manifest).map_err(|_| {
        GenerationPublicationError::contract(
            GenerationPublicationDiagnosticCode::SerializationFailed,
        )
    })
}
