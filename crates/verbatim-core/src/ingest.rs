use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "qdrant")]
use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::{
    ChunkingProfileStatusResponse, EmbeddingCapabilityStatusResponse, IndexStatusResponse,
    IndexStatusResponseFields,
};
use crate::canonical_chunker::{CanonicalChunkerConfig, CANONICAL_CHUNKER_VERSION};

use crate::chunker::{ChunkerConfig, CHUNKER_VERSION};
use crate::collection::{
    discover_collection_members, CollectionSyncPathInput, CollectionSyncReport,
    CollectionSyncSettings, DEFAULT_COLLECTION_SYNC_MAX_DEPTH,
};
use crate::config::{Config, GraphExtractionConfig};
use crate::context::ContextGenerator;
use crate::embed::OpenAiEmbeddingClient;
use crate::evidence_spans::ChunkEvidenceSpan;
use crate::graph_extraction::GraphExtractor;
use crate::image_limits::{
    ImageArtifactBudget, ImageArtifactLimitError, ImageArtifactLimitStage, ImageArtifactLimits,
};
use crate::index::hnsw::HnswIndex;
#[cfg(feature = "qdrant")]
use crate::index::qdrant::QdrantClient;
use crate::index::sqlite_fts::{FtsMaintenanceOutcome, SqliteFtsIndex};
use crate::index_gc::{apply_index_gc, IndexGcPolicy};
use crate::index_profile_delete::{
    apply_index_profile_delete_artifacts, apply_index_profile_delete_sqlite,
    plan_index_profile_delete, IndexProfileDeleteApplyReport, IndexProfileDeletePlan,
};
use crate::memory_budget::{MemoryBudget, MemoryReservationGuard};
use crate::ocr::{
    configured_ocr_provider, ocr_evidence_from_output, ocr_profile_stale, ocr_required_pages,
    pdf_scan_summary, pdf_scan_summary_with_page_count, source_ingest_diagnostics, OcrPageRequest,
    OcrProvider,
};
use crate::parser;
use crate::provider::openai_compatible::OpenAiCompatibleVisionModel;
use crate::provider::VisionModel;
use crate::resource::{
    global_resource_registry, ObservableResource, ResourceLimitConfig, ResourcePermit,
    TaskResourceProgress,
};
use crate::store::{
    source_relocation::remap_parser_evidence_identity, EmbeddingProfileConfig,
    SourceContentsReplacement, SourceEmbeddingCacheVector, SqliteWriteOperation, Store,
    StoredEmbeddingProfileConfig,
};
use crate::task::{
    FinishedPhaseTiming, IngestTaskStage, PhaseTiming, TaskEndpointSummary, TaskId,
    TaskProgressSnapshot, TaskStatus,
};
use crate::traits::{
    EmbeddingClient, EmbeddingEndpointCapabilities, LexicalIndex, Parser, VectorDocument,
    VectorIndex,
};
use crate::types::{
    hex_sha256, Chunk, ChunkId, ChunkType, EdgeType, EmbeddingCacheStats, EmbeddingProfileId,
    EvidenceId, EvidenceKind, EvidenceUnit, GraphEdge, GraphEdgeId, GraphNode, GraphNodeId,
    GraphNodeKind, ImageArtifact, ImageId, ParsedImageArtifact, Source, SourceEmbeddingStatus,
    SourceId, SourceLocator, SourceStatus, VectorIndexResidency,
};
use crate::vision_caption::{
    request_image_caption, vision_caption_prompt_hash, CaptionAttempt, ImageCaptionStatus,
    VISION_CAPTION_PROMPT_VERSION,
};

pub struct IngestPipeline<E = OpenAiEmbeddingClient> {
    store: Store,
    hnsw: HnswIndex,
    vector_residency: VectorIndexResidency,
    loaded_profile_id: EmbeddingProfileId,
    active_profile_id: EmbeddingProfileId,
    embedding_profile_spec: EmbeddingProfileSpec,
    embedding_enabled: bool,
    embed_client: E,
    context_gen: Option<ContextGenerator>,
    vision_model: Option<Box<dyn VisionModel>>,
    graph_extractor: Option<GraphExtractor>,
    graph_extraction_config: GraphExtractionConfig,
    #[cfg(feature = "qdrant")]
    qdrant: Option<QdrantClient>,
    #[cfg(feature = "qdrant")]
    pending_qdrant_profile_syncs: Vec<EmbeddingProfileId>,
    vision_caption_model: String,
    vision_caption_prompt_hash: String,
    ocr_provider: Option<Box<dyn OcrProvider>>,
    data_dir: PathBuf,
    image_artifact_limits: ImageArtifactLimits,
    index_gc_policy: IndexGcPolicy,
    embedding_batch_size: usize,
    embedding_max_concurrent_requests: usize,
    memory_budget: MemoryBudget,
    fts_startup_maintenance: FtsMaintenanceOutcome,
    #[cfg(test)]
    source_commit_observer: Option<SourceCommitObserver>,
    #[cfg(all(test, feature = "qdrant"))]
    qdrant_requeue_store_observer: Option<QdrantRequeueStoreObserver>,
    #[cfg(test)]
    fail_next_batched_index_stage_with_enospc: bool,
    #[cfg(test)]
    fail_next_batched_compensation_write_with_readonly: bool,
}

#[cfg(test)]
type SourceCommitObserver = Box<dyn Fn(&Store, &SourceId) + Send + Sync>;
#[cfg(all(test, feature = "qdrant"))]
type QdrantRequeueStoreObserver = Arc<dyn Fn(&Store) + Send + Sync>;

#[path = "ingest_chunk_partition.rs"]
mod chunk_partition;
#[path = "ingest_deletion.rs"]
mod ingest_deletion;
#[path = "ingest_qdrant_sync.rs"]
mod ingest_qdrant_sync;
#[cfg(test)]
#[path = "tests/issue_332_explicit_move_tests.rs"]
mod issue_332_explicit_move_tests;
#[cfg(test)]
#[path = "tests/issue_362_tests.rs"]
mod issue_362_tests;
#[cfg(test)]
#[path = "tests/issue_363_cache_purge_tests.rs"]
mod issue_363_cache_purge_tests;
#[cfg(test)]
#[path = "tests/issue_363_deletion_lifecycle_tests.rs"]
mod issue_363_deletion_lifecycle_tests;
#[cfg(all(test, not(feature = "qdrant")))]
#[path = "tests/issue_363_no_qdrant_tests.rs"]
mod issue_363_no_qdrant_tests;
#[cfg(test)]
#[path = "tests/issue_363_qdrant_mutation_fence_tests.rs"]
mod issue_363_qdrant_mutation_fence_tests;
#[cfg(test)]
#[path = "tests/issue_363_reconcile_tests.rs"]
mod issue_363_reconcile_tests;
#[cfg(test)]
#[path = "tests/issue_363_tests.rs"]
mod issue_363_tests;
#[cfg(test)]
#[path = "tests/issue_363_tombstone_fence_tests.rs"]
mod issue_363_tombstone_fence_tests;

struct PreparedIndexes {
    hnsw: HnswIndex,
    vectors: Vec<VectorDocument>,
    cache_stats: EmbeddingCacheStats,
    _memory_reservation: Option<MemoryReservationGuard>,
}

struct BatchedCommittedSource {
    source_id: SourceId,
    task_id: Option<TaskId>,
    index_generation: u64,
    vector_count: usize,
    cache_stats: EmbeddingCacheStats,
    io_telemetry: SourceCommitIoTelemetry,
    retained_image_artifacts: Vec<ImageArtifact>,
}

#[derive(Debug, Clone, Copy, Default)]
struct StagedIndexArtifactStats {
    file_count: u64,
    hnsw_bytes: u64,
    total_bytes: u64,
    manifest_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct SourceCommitIoTelemetry {
    evidence_count: u64,
    evidence_text_bytes: u64,
    chunk_count: u64,
    child_chunk_count: u64,
    chunk_text_bytes: u64,
    context_text_bytes: u64,
    link_count: u64,
    image_artifact_count: u64,
    image_artifact_file_count: u64,
    image_artifact_bytes: u64,
    image_artifact_files_written: u64,
    image_artifact_bytes_written: u64,
    graph_node_count: u64,
    graph_edge_count: u64,
    vector_count: u64,
    vector_bytes: u64,
}

struct SourceCommitIoTelemetryInputs<'a> {
    evidence: &'a [EvidenceUnit],
    chunks: &'a [Chunk],
    links: &'a [(ChunkId, EvidenceId)],
    image_artifacts: &'a PreparedImageArtifacts,
    graph_nodes: &'a [GraphNode],
    graph_edges: &'a [GraphEdge],
    vectors: &'a [VectorDocument],
    written_image_files: &'a [WrittenImageFile],
}

impl SourceCommitIoTelemetry {
    fn new(inputs: SourceCommitIoTelemetryInputs<'_>) -> Self {
        let SourceCommitIoTelemetryInputs {
            evidence,
            chunks,
            links,
            image_artifacts,
            graph_nodes,
            graph_edges,
            vectors,
            written_image_files,
        } = inputs;

        let evidence_text_bytes = evidence.iter().fold(0_u64, |total, unit| {
            total.saturating_add(usize_to_u64(unit.text.len()))
        });
        let chunk_text_bytes = chunks.iter().fold(0_u64, |total, chunk| {
            total.saturating_add(usize_to_u64(chunk.text.len()))
        });
        let context_text_bytes = chunks.iter().fold(0_u64, |total, chunk| {
            total.saturating_add(
                chunk
                    .context_text
                    .as_ref()
                    .map(|text| usize_to_u64(text.len()))
                    .unwrap_or(0),
            )
        });
        let image_artifact_bytes = image_artifacts.files.iter().fold(0_u64, |total, file| {
            total.saturating_add(usize_to_u64(file.bytes.len()))
        });
        let image_artifact_files_written = written_image_files
            .iter()
            .filter(|file| !file.preexisting)
            .count();
        let image_artifact_bytes_written =
            image_artifacts.files.iter().fold(0_u64, |total, file| {
                let was_written = written_image_files.iter().any(|written| {
                    !written.preexisting && written.absolute_path == file.absolute_path
                });
                if was_written {
                    total.saturating_add(usize_to_u64(file.bytes.len()))
                } else {
                    total
                }
            });
        let vector_bytes = vectors.iter().fold(0_u64, |total, vector| {
            total.saturating_add(usize_to_u64(
                vector
                    .vector
                    .len()
                    .saturating_mul(std::mem::size_of::<f32>()),
            ))
        });

        Self {
            evidence_count: usize_to_u64(evidence.len()),
            evidence_text_bytes,
            chunk_count: usize_to_u64(chunks.len()),
            child_chunk_count: usize_to_u64(
                chunks
                    .iter()
                    .filter(|chunk| chunk.chunk_type == ChunkType::Child)
                    .count(),
            ),
            chunk_text_bytes,
            context_text_bytes,
            link_count: usize_to_u64(links.len()),
            image_artifact_count: usize_to_u64(image_artifacts.artifacts.len()),
            image_artifact_file_count: usize_to_u64(image_artifacts.files.len()),
            image_artifact_bytes,
            image_artifact_files_written: usize_to_u64(image_artifact_files_written),
            image_artifact_bytes_written,
            graph_node_count: usize_to_u64(graph_nodes.len()),
            graph_edge_count: usize_to_u64(graph_edges.len()),
            vector_count: usize_to_u64(vectors.len()),
            vector_bytes,
        }
    }

    fn db_metadata(
        &self,
        source_id: &SourceId,
        profile_id: &EmbeddingProfileId,
        generation: u64,
    ) -> serde_json::Value {
        self.db_metadata_inner(source_id, profile_id, Some(generation), 1, false)
    }

    fn batched_db_metadata(
        &self,
        source_id: &SourceId,
        profile_id: &EmbeddingProfileId,
        generation: u64,
        generation_advanced: bool,
    ) -> serde_json::Value {
        self.db_metadata_inner(
            source_id,
            profile_id,
            Some(generation),
            u64::from(generation_advanced),
            false,
        )
    }

    fn db_metadata_inner(
        &self,
        source_id: &SourceId,
        profile_id: &EmbeddingProfileId,
        generation: Option<u64>,
        index_generation_updates: u64,
        generation_deferred_to_batch: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "operation": "replace_source_contents",
            "source_id": source_id.0.as_str(),
            "embedding_profile_id": profile_id.as_str(),
            "index_generation": generation,
            "index_generation_deferred_to_batch": generation_deferred_to_batch,
            "io": {
                "scope": "source_ingest_commit",
                "storage": "sqlite",
                "replace_strategy": "delete_insert_source_cascade",
                "estimated_logical_write_rows": self.estimated_logical_db_rows(),
                "estimated_logical_payload_bytes": self.estimated_logical_payload_bytes(),
                "logical_rows": {
                    "sources": 1_u64,
                    "evidence_units": self.evidence_count,
                    "chunks": self.chunk_count,
                    "child_chunks": self.child_chunk_count,
                    "chunk_evidence_links": self.link_count,
                    "image_artifacts": self.image_artifact_count,
                    "graph_nodes": self.graph_node_count,
                    "graph_edges": self.graph_edge_count,
                    "chunk_vectors": self.vector_count,
                    "source_embedding_statuses": 1_u64,
                    "index_generation_updates": index_generation_updates,
                },
                "logical_bytes": {
                    "evidence_text": self.evidence_text_bytes,
                    "chunk_text": self.chunk_text_bytes,
                    "chunk_context_text": self.context_text_bytes,
                    "chunk_vectors": self.vector_bytes,
                    "image_artifacts_prepared": self.image_artifact_bytes,
                },
            },
        })
    }

    fn index_publish_metadata(
        &self,
        source_id: &SourceId,
        profile_id: &EmbeddingProfileId,
        generation: u64,
        staged: StagedIndexArtifactStats,
    ) -> serde_json::Value {
        serde_json::json!({
            "source_id": source_id.0.as_str(),
            "embedding_profile_id": profile_id.as_str(),
            "index_generation": generation,
            "io": {
                "scope": "source_ingest_index_publish",
                "storage": "filesystem",
                "publish_strategy": "stage_then_rename_generation_dir",
                "staged_artifact_files": staged.file_count,
                "staged_artifact_bytes": staged.total_bytes,
                "hnsw_bytes": staged.hnsw_bytes,
                "manifest_bytes": staged.manifest_bytes,
                "image_artifact_files_prepared": self.image_artifact_file_count,
                "image_artifact_files_written": self.image_artifact_files_written,
                "image_artifact_bytes_prepared": self.image_artifact_bytes,
                "image_artifact_bytes_written": self.image_artifact_bytes_written,
                "estimated_logical_write_bytes": staged
                    .total_bytes
                    .saturating_add(staged.manifest_bytes)
                    .saturating_add(self.image_artifact_bytes_written),
            },
        })
    }

    fn estimated_logical_db_rows(&self) -> u64 {
        1_u64
            .saturating_add(self.evidence_count)
            .saturating_add(self.chunk_count)
            .saturating_add(self.link_count)
            .saturating_add(self.image_artifact_count)
            .saturating_add(self.graph_node_count)
            .saturating_add(self.graph_edge_count)
            .saturating_add(self.vector_count)
            .saturating_add(1)
            .saturating_add(1)
    }

    fn estimated_logical_payload_bytes(&self) -> u64 {
        self.evidence_text_bytes
            .saturating_add(self.chunk_text_bytes)
            .saturating_add(self.context_text_bytes)
            .saturating_add(self.vector_bytes)
            .saturating_add(self.image_artifact_bytes)
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Serializes destructive Qdrant mutations against source/profile upserts.
///
/// Upserts hold a shared guard across their remote mutation and compensation;
/// deletion holds the exclusive guard. Tokio's fair `RwLock` admits the writer
/// only after already-running upserts finish and prevents later upserts from
/// starting until the deletion's remote erase is complete.
#[cfg(feature = "qdrant")]
static QDRANT_MUTATION_FENCE: LazyLock<tokio::sync::RwLock<()>> =
    LazyLock::new(|| tokio::sync::RwLock::new(()));

#[cfg(feature = "qdrant")]
fn qdrant_mutation_fence() -> &'static tokio::sync::RwLock<()> {
    &QDRANT_MUTATION_FENCE
}

fn ingest_resource(name: &'static str, kind: &'static str) -> Arc<ObservableResource> {
    global_resource_registry().resource_or_insert(
        name,
        kind,
        ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 512,
            queue_timeout: std::time::Duration::from_secs(300),
        },
    )
}

async fn acquire_ingest_resource(name: &'static str, kind: &'static str) -> Result<ResourcePermit> {
    Ok(ingest_resource(name, kind).acquire().await?)
}

fn acquire_ingest_resource_blocking(
    name: &'static str,
    kind: &'static str,
) -> Result<ResourcePermit> {
    Ok(ingest_resource(name, kind).acquire_blocking()?)
}

fn waiting_resource_progress(name: &'static str, kind: &'static str) -> TaskResourceProgress {
    TaskResourceProgress::from_snapshot(
        &ingest_resource(name, kind).snapshot(),
        "waiting",
        None,
        None,
    )
}

fn task_resource_progress(permit: &ResourcePermit, state: &'static str) -> TaskResourceProgress {
    let snapshot = permit.snapshot();
    TaskResourceProgress::from_snapshot(&snapshot, state, Some(permit.queue_wait_ms()), None)
}

fn completed_resource_progress(permit: &ResourcePermit) -> TaskResourceProgress {
    let snapshot = permit.snapshot();
    TaskResourceProgress::from_snapshot(
        &snapshot,
        "completed",
        Some(permit.queue_wait_ms()),
        Some(permit.service_ms()),
    )
}

struct PreparedVectors {
    vectors: Vec<VectorDocument>,
    cache_stats: EmbeddingCacheStats,
    cache_stats_by_source: HashMap<SourceId, EmbeddingCacheStats>,
    errors_by_source: HashMap<SourceId, String>,
    request_source_ids: Vec<SourceId>,
    request_timing: Option<FinishedPhaseTiming>,
    postprocess_timing: Option<FinishedPhaseTiming>,
    request_count: usize,
    max_vectors_per_request: usize,
}

#[derive(Debug, Clone, Copy)]
struct EmbeddingExecutionControls {
    batch_size: usize,
    max_concurrent_requests: usize,
}

struct EmbeddingBatchMetrics {
    request_count: usize,
    source_count: usize,
    input_count: usize,
    max_vectors_per_request: usize,
    max_concurrent_requests: usize,
    duration_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct IndexingOutcome {
    pub source_count: usize,
    pub skipped_missing_sources: usize,
    pub embedding_cache: EmbeddingCacheStats,
}

/// Stable machine-readable diagnostics for rejected ingest requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestDiagnosticCode {
    PdfNoUsableTextLayer,
}

impl IngestDiagnosticCode {
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::PdfNoUsableTextLayer => "pdf_no_usable_text_layer",
        }
    }
}

impl std::fmt::Display for IngestDiagnosticCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.diagnostic_code())
    }
}

impl std::error::Error for IngestDiagnosticCode {}

/// Result for one source inside a multi-source background ingest batch.
#[derive(Debug, Clone)]
pub struct SourceIngestOutcome {
    pub source_id: SourceId,
    pub task_id: TaskId,
    pub result: std::result::Result<EmbeddingCacheStats, String>,
}

/// Freshness state for deciding whether an ingest task is necessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceIngestFreshness {
    Fresh,
    Missing,
    NeedsIngest(SourceIngestStaleReason),
}

/// Current source freshness plus the file fingerprint used to decide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIngestSnapshot {
    pub freshness: SourceIngestFreshness,
    pub current_hash: Option<String>,
}

impl SourceIngestFreshness {
    /// Stable machine-readable reason for logs and task metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Missing => "missing",
            Self::NeedsIngest(reason) => reason.as_str(),
        }
    }

    pub fn needs_ingest(self) -> bool {
        matches!(self, Self::NeedsIngest(_))
    }
}

/// Reason a source needs parse/vector ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceIngestStaleReason {
    HashChanged,
    NotIndexed,
    VectorsStale,
    OcrStale,
}

impl SourceIngestStaleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HashChanged => "hash_changed",
            Self::NotIndexed => "not_indexed",
            Self::VectorsStale => "vectors_stale",
            Self::OcrStale => "ocr_stale",
        }
    }
}

#[derive(Debug, Clone)]
struct EmbeddingProfileSpec {
    provider: String,
    model: String,
    dimension: usize,
    normalize: bool,
    endpoint_identity: Option<String>,
    requested_model: Option<String>,
    served_model: Option<String>,
    max_context_tokens: Option<usize>,
    dtype: Option<String>,
    quantization: Option<String>,
    weight_identity: Option<String>,
    chunker_config: ChunkerConfig,
    canonical_chunker_config: CanonicalChunkerConfig,
    embedding_input_budget_tokens: Option<usize>,
    query_instruction: String,
    document_instruction: String,
}

impl EmbeddingProfileSpec {
    fn from_config(config: &crate::config::EmbeddingConfig) -> Self {
        let mut spec = Self {
            provider: config.provider.clone(),
            model: config.model.clone(),
            dimension: config.dimension,
            normalize: config.normalize,
            endpoint_identity: sanitized_endpoint_identity(&config.base_url),
            requested_model: trimmed_optional_string(Some(&config.model)),
            served_model: trimmed_optional_string(config.served_model.as_deref()),
            max_context_tokens: config.context_window_tokens,
            dtype: lowercase_optional_string(config.dtype.as_deref()),
            quantization: lowercase_optional_string(config.quantization.as_deref()),
            weight_identity: trimmed_optional_string(config.weight_identity.as_deref()),
            chunker_config: ChunkerConfig::default(),
            canonical_chunker_config: CanonicalChunkerConfig {
                target_tokens: config.canonical_target_tokens,
                overlap_units: config.canonical_overlap_units,
                max_units_per_child: config.canonical_max_units_per_child,
            },
            embedding_input_budget_tokens: None,
            query_instruction: config.query_instruction.clone(),
            document_instruction: config.document_instruction.clone(),
        };
        spec.recompute_chunking_policy();
        spec
    }

    fn apply_endpoint_capabilities(&mut self, capabilities: EmbeddingEndpointCapabilities) {
        self.endpoint_identity = capabilities
            .endpoint_identity
            .and_then(|value| sanitized_endpoint_identity(&value))
            .or_else(|| self.endpoint_identity.clone());
        self.requested_model = trimmed_optional_string(capabilities.requested_model.as_deref())
            .or_else(|| self.requested_model.clone());
        self.served_model = trimmed_optional_string(capabilities.served_model.as_deref())
            .or_else(|| self.served_model.clone());
        self.max_context_tokens = capabilities.max_context_tokens.or(self.max_context_tokens);
        self.dtype =
            lowercase_optional_string(capabilities.dtype.as_deref()).or_else(|| self.dtype.clone());
        self.quantization = lowercase_optional_string(capabilities.quantization.as_deref())
            .or_else(|| self.quantization.clone());
        self.weight_identity = trimmed_optional_string(capabilities.weight_identity.as_deref())
            .or_else(|| self.weight_identity.clone());
        self.recompute_chunking_policy();
    }

    fn apply_stored_profile_config(
        &mut self,
        stored: &StoredEmbeddingProfileConfig,
        preserve_live_canonical_chunker_config: bool,
    ) {
        self.endpoint_identity = self
            .endpoint_identity
            .clone()
            .or_else(|| stored.endpoint_identity.clone());
        self.requested_model = self
            .requested_model
            .clone()
            .or_else(|| stored.requested_model.clone());
        self.served_model = self
            .served_model
            .clone()
            .or_else(|| stored.served_model.clone());
        let preserve_stored_chunking = self.max_context_tokens.is_none()
            && (stored.max_context_tokens.is_some()
                || stored.embedding_input_budget_tokens.is_some());
        self.max_context_tokens = self.max_context_tokens.or(stored.max_context_tokens);
        self.dtype = self.dtype.clone().or_else(|| stored.dtype.clone());
        self.quantization = self
            .quantization
            .clone()
            .or_else(|| stored.quantization.clone());
        self.weight_identity = self
            .weight_identity
            .clone()
            .or_else(|| stored.weight_identity.clone());
        if !preserve_live_canonical_chunker_config {
            self.canonical_chunker_config = CanonicalChunkerConfig {
                target_tokens: stored.canonical_target_tokens,
                overlap_units: stored.canonical_overlap_units,
                max_units_per_child: stored.canonical_max_units_per_child,
            };
        }
        if preserve_stored_chunking {
            self.chunker_config = ChunkerConfig {
                child_target_tokens: stored.child_target_tokens,
                child_overlap_tokens: stored.child_overlap_tokens,
                parent_children_count: stored.parent_children_count,
            };
            self.embedding_input_budget_tokens = stored.embedding_input_budget_tokens;
        } else {
            self.recompute_chunking_policy();
        }
    }

    fn recompute_chunking_policy(&mut self) {
        let default = ChunkerConfig::default();
        let budget = self.max_context_tokens.map(safe_embedding_input_budget);
        let child_target_tokens = budget
            .map(|budget| default.child_target_tokens.min(budget.max(1)))
            .unwrap_or(default.child_target_tokens)
            .max(1);
        let child_overlap_tokens = if budget.is_some() {
            default
                .child_overlap_tokens
                .min(child_target_tokens.saturating_div(4).max(1))
        } else {
            default.child_overlap_tokens
        };
        self.chunker_config = ChunkerConfig {
            child_target_tokens,
            child_overlap_tokens,
            parent_children_count: default.parent_children_count,
        };
        self.embedding_input_budget_tokens = budget;
    }

    fn as_store_config(&self) -> EmbeddingProfileConfig<'_> {
        EmbeddingProfileConfig {
            provider: &self.provider,
            model: &self.model,
            dimension: self.dimension,
            normalize: self.normalize,
            endpoint_identity: self.endpoint_identity.as_deref(),
            requested_model: self.requested_model.as_deref(),
            served_model: self.served_model.as_deref(),
            max_context_tokens: self.max_context_tokens,
            dtype: self.dtype.as_deref(),
            quantization: self.quantization.as_deref(),
            weight_identity: self.weight_identity.as_deref(),
            chunker_version: CHUNKER_VERSION,
            child_target_tokens: self.chunker_config.child_target_tokens,
            child_overlap_tokens: self.chunker_config.child_overlap_tokens,
            parent_children_count: self.chunker_config.parent_children_count,
            canonical_chunker_version: CANONICAL_CHUNKER_VERSION,
            canonical_target_tokens: self.canonical_chunker_config.target_tokens,
            canonical_overlap_units: self.canonical_chunker_config.overlap_units,
            canonical_max_units_per_child: self.canonical_chunker_config.max_units_per_child,
            embedding_input_budget_tokens: self.embedding_input_budget_tokens,
            query_instruction: &self.query_instruction,
            document_instruction: &self.document_instruction,
        }
    }

    fn config_hash(&self) -> String {
        self.as_store_config().config_hash()
    }
}

#[cfg(test)]
fn test_embedding_profile_spec(dimension: usize) -> EmbeddingProfileSpec {
    EmbeddingProfileSpec {
        provider: "test".to_string(),
        model: "test-embedding".to_string(),
        dimension,
        normalize: true,
        endpoint_identity: None,
        requested_model: Some("test-embedding".to_string()),
        served_model: None,
        max_context_tokens: None,
        dtype: None,
        quantization: None,
        weight_identity: None,
        chunker_config: ChunkerConfig::default(),
        canonical_chunker_config: CanonicalChunkerConfig::default(),
        embedding_input_budget_tokens: None,
        query_instruction: String::new(),
        document_instruction: String::new(),
    }
}

fn safe_embedding_input_budget(max_context_tokens: usize) -> usize {
    max_context_tokens
        .saturating_mul(3)
        .saturating_div(4)
        .max(1)
}

fn trimmed_optional_string(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn lowercase_optional_string(value: Option<&str>) -> Option<String> {
    trimmed_optional_string(value).map(|value| value.to_ascii_lowercase())
}

fn sanitized_endpoint_identity(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        return None;
    }
    if let Ok(mut url) = url::Url::parse(value) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return Some(url.as_str().trim_end_matches('/').to_string());
    }
    Some(
        value
            .split(['?', '#'])
            .next()
            .unwrap_or(value)
            .trim_end_matches('/')
            .to_string(),
    )
}

struct EmbeddingInput {
    chunk_id: ChunkId,
    source_id: SourceId,
    hash: String,
    text: String,
}

struct PreparedEmbeddingVector {
    embedding_input_hash: String,
    document: VectorDocument,
}

struct PreparedSourceIngest {
    task_id: Option<TaskId>,
    source: Source,
    evidence: Vec<EvidenceUnit>,
    chunks: Vec<Chunk>,
    links: Vec<(ChunkId, EvidenceId)>,
    evidence_spans: Vec<ChunkEvidenceSpan>,
    image_artifacts: PreparedImageArtifacts,
    graph_nodes: Vec<GraphNode>,
    graph_edges: Vec<GraphEdge>,
    child_chunks: Vec<Chunk>,
    embedding_phase: PhaseTiming,
}

impl PreparedSourceIngest {
    fn prepared_artifact_bytes(&self) -> usize {
        self.image_artifacts
            .files
            .iter()
            .fold(0_usize, |total, file| {
                total.saturating_add(file.bytes.len())
            })
    }
}

#[derive(Default)]
struct PendingPreparedSources {
    sources: Vec<PreparedSourceIngest>,
    child_count: usize,
    artifact_bytes: usize,
}

impl PendingPreparedSources {
    fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.sources.len()
    }

    #[cfg(test)]
    fn artifact_bytes(&self) -> usize {
        self.artifact_bytes
    }

    fn should_flush_before_push(
        &self,
        next_artifact_bytes: usize,
        artifact_byte_limit: usize,
    ) -> bool {
        !self.is_empty()
            && self.artifact_bytes > 0
            && next_artifact_bytes > 0
            && self.artifact_bytes.saturating_add(next_artifact_bytes) > artifact_byte_limit.max(1)
    }

    fn should_flush_after_push(
        &self,
        embedding_window_limit: usize,
        artifact_byte_limit: usize,
    ) -> bool {
        if self.is_empty() {
            return false;
        }
        let embedding_window_limit = embedding_window_limit.max(1);
        self.child_count >= embedding_window_limit
            || self.sources.len() >= embedding_window_limit
            || (self.artifact_bytes > 0 && self.artifact_bytes >= artifact_byte_limit.max(1))
    }

    fn push(&mut self, source: PreparedSourceIngest) {
        self.child_count = self.child_count.saturating_add(source.child_chunks.len());
        self.artifact_bytes = self
            .artifact_bytes
            .saturating_add(source.prepared_artifact_bytes());
        self.sources.push(source);
    }

    fn take(&mut self) -> Vec<PreparedSourceIngest> {
        self.child_count = 0;
        self.artifact_bytes = 0;
        std::mem::take(&mut self.sources)
    }
}

#[derive(Clone, Copy)]
enum BatchCancellationScope {
    SharedTask {
        completed_sources_before_batch: usize,
        total_sources: usize,
    },
    PerSourceTask,
}

impl BatchCancellationScope {
    fn progress(self, committed_sources_in_batch: usize) -> (usize, usize) {
        match self {
            Self::SharedTask {
                completed_sources_before_batch,
                total_sources,
            } => (
                completed_sources_before_batch.saturating_add(committed_sources_in_batch),
                total_sources,
            ),
            Self::PerSourceTask => (0, 1),
        }
    }
}

struct PreparedSourceOutcome {
    source_id: SourceId,
    task_id: Option<TaskId>,
    result: std::result::Result<EmbeddingCacheStats, String>,
}

fn collect_prepared_source_outcomes(
    outcome_slots: Vec<Option<PreparedSourceOutcome>>,
) -> Vec<PreparedSourceOutcome> {
    outcome_slots
        .into_iter()
        .map(|outcome| outcome.expect("prepared source outcome slot populated"))
        .collect()
}

#[derive(Debug)]
struct PreparedImageArtifacts {
    evidence: Vec<EvidenceUnit>,
    artifacts: Vec<ImageArtifact>,
    files: Vec<PreparedImageFile>,
    text_proximities: Vec<ImageTextProximity>,
}

#[derive(Debug)]
struct ImageTextProximity {
    image_id: ImageId,
    nearby_text_before: Option<String>,
    nearby_text_after: Option<String>,
}

#[derive(Debug)]
struct PreparedImageFile {
    absolute_path: PathBuf,
    bytes: Vec<u8>,
    content_hash: String,
    page: u32,
    image_index: u32,
}

#[derive(Debug)]
struct WrittenImageFile {
    absolute_path: PathBuf,
    preexisting: bool,
}

const IMAGE_ARTIFACTS_DIR: &str = "image-artifacts";
const COMPONENT_HASH_LEN: usize = 16;

#[derive(Debug, Deserialize, Serialize)]
struct IndexManifest {
    generation: u64,
}

fn validate_qdrant_runtime_support(enabled: bool, compiled_support: bool) -> Result<()> {
    if enabled && !compiled_support {
        bail!("qdrant.enabled=true requires a binary built with the verbatim-core/qdrant feature");
    }
    Ok(())
}

impl IngestPipeline<OpenAiEmbeddingClient> {
    pub fn new(config: &Config, data_dir: &Path) -> Result<Self> {
        validate_qdrant_runtime_support(config.qdrant.enabled, cfg!(feature = "qdrant"))?;
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create data dir: {}", data_dir.display()))?;

        let db_path = data_dir.join("verbatim.db");
        let store = Store::new_with_durability_profile(&db_path, config.store.durability)?;
        let fts_startup_maintenance = SqliteFtsIndex::new(&store)
            .maintain_startup()
            .context("sqlite FTS startup maintenance")?;

        let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
        let active_profile_id = config.embedding.profile_id.clone();
        let mut embedding_profile_spec = EmbeddingProfileSpec::from_config(&config.embedding);
        if let Some(stored) = store.load_embedding_profile_config(&active_profile_id)? {
            embedding_profile_spec.apply_stored_profile_config(&stored, true);
        }
        let profile_reset_on_open = store.ensure_embedding_profile(
            &active_profile_id,
            embedding_profile_spec.as_store_config(),
        )?;
        #[cfg(not(feature = "qdrant"))]
        let _ = profile_reset_on_open;

        let hnsw = load_vector_index_for_residency(
            config.vector_index.residency,
            data_dir,
            &store,
            &active_profile_id,
        )?;

        let context_gen = if config.context.enabled {
            Some(ContextGenerator::new(&config.chat))
        } else {
            None
        };
        let (vision_model, vision_caption_model) = configured_vision_model(config);
        let ocr_provider = configured_ocr_provider(&config.ocr)?;
        let graph_extraction_config = config.graph.extraction.clone();
        let graph_extractor = if graph_extraction_config.enabled && config.chat.enabled {
            Some(GraphExtractor::from_config(&config.chat))
        } else {
            None
        };
        #[cfg(feature = "qdrant")]
        let qdrant = QdrantClient::from_config(&config.qdrant);
        #[cfg(feature = "qdrant")]
        let pending_qdrant_profile_syncs = profile_reset_on_open
            .then(|| active_profile_id.clone())
            .into_iter()
            .collect();

        Ok(Self {
            store,
            hnsw,
            vector_residency: config.vector_index.residency,
            loaded_profile_id: active_profile_id.clone(),
            active_profile_id,
            embedding_profile_spec,
            embedding_enabled: config.embedding.enabled,
            embed_client,
            context_gen,
            vision_model,
            graph_extractor,
            graph_extraction_config,
            #[cfg(feature = "qdrant")]
            qdrant,
            #[cfg(feature = "qdrant")]
            pending_qdrant_profile_syncs,
            vision_caption_model,
            vision_caption_prompt_hash: vision_caption_prompt_hash(),
            ocr_provider,
            data_dir: data_dir.to_path_buf(),
            image_artifact_limits: config.parser.image_artifacts,
            index_gc_policy: config.index_gc.policy(),
            embedding_batch_size: config.embedding.batch_size.max(1),
            embedding_max_concurrent_requests: config
                .embedding
                .endpoint_runtime
                .bounded()
                .max_concurrent_requests,
            memory_budget: MemoryBudget::from_config(&config.daemon.resources),
            fts_startup_maintenance,
            #[cfg(test)]
            source_commit_observer: None,
            #[cfg(all(test, feature = "qdrant"))]
            qdrant_requeue_store_observer: None,
            #[cfg(test)]
            fail_next_batched_index_stage_with_enospc: false,
            #[cfg(test)]
            fail_next_batched_compensation_write_with_readonly: false,
        })
    }

    pub fn open_readonly(config: &Config, data_dir: &Path) -> Result<Self> {
        validate_qdrant_runtime_support(config.qdrant.enabled, cfg!(feature = "qdrant"))?;
        let db_path = data_dir.join("verbatim.db");
        let store = Store::open_existing_readonly_with_durability_profile(
            &db_path,
            config.store.durability,
        )?;

        let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
        let active_profile_id = config.embedding.profile_id.clone();
        let mut embedding_profile_spec = EmbeddingProfileSpec::from_config(&config.embedding);
        if let Some(stored) = store.load_embedding_profile_config(&active_profile_id)? {
            embedding_profile_spec.apply_stored_profile_config(&stored, false);
        }

        let hnsw = load_vector_index_for_residency(
            config.vector_index.residency,
            data_dir,
            &store,
            &active_profile_id,
        )?;

        let context_gen = if config.context.enabled {
            Some(ContextGenerator::new(&config.chat))
        } else {
            None
        };
        let (vision_model, vision_caption_model) = configured_vision_model(config);
        let ocr_provider = configured_ocr_provider(&config.ocr)?;
        let graph_extraction_config = config.graph.extraction.clone();
        let graph_extractor = if graph_extraction_config.enabled && config.chat.enabled {
            Some(GraphExtractor::from_config(&config.chat))
        } else {
            None
        };
        #[cfg(feature = "qdrant")]
        let qdrant = QdrantClient::from_config(&config.qdrant);
        #[cfg(feature = "qdrant")]
        let pending_qdrant_profile_syncs = Vec::new();

        Ok(Self {
            store,
            hnsw,
            vector_residency: config.vector_index.residency,
            loaded_profile_id: active_profile_id.clone(),
            active_profile_id,
            embedding_profile_spec,
            embedding_enabled: config.embedding.enabled,
            embed_client,
            context_gen,
            vision_model,
            graph_extractor,
            graph_extraction_config,
            #[cfg(feature = "qdrant")]
            qdrant,
            #[cfg(feature = "qdrant")]
            pending_qdrant_profile_syncs,
            vision_caption_model,
            vision_caption_prompt_hash: vision_caption_prompt_hash(),
            ocr_provider,
            data_dir: data_dir.to_path_buf(),
            image_artifact_limits: config.parser.image_artifacts,
            index_gc_policy: config.index_gc.policy(),
            embedding_batch_size: config.embedding.batch_size.max(1),
            embedding_max_concurrent_requests: config
                .embedding
                .endpoint_runtime
                .bounded()
                .max_concurrent_requests,
            memory_budget: MemoryBudget::from_config(&config.daemon.resources),
            fts_startup_maintenance: FtsMaintenanceOutcome::default(),
            #[cfg(test)]
            source_commit_observer: None,
            #[cfg(all(test, feature = "qdrant"))]
            qdrant_requeue_store_observer: None,
            #[cfg(test)]
            fail_next_batched_index_stage_with_enospc: false,
            #[cfg(test)]
            fail_next_batched_compensation_write_with_readonly: false,
        })
    }

    pub fn reload_runtime_config(&mut self, config: &Config) -> Result<()> {
        validate_qdrant_runtime_support(config.qdrant.enabled, cfg!(feature = "qdrant"))?;
        self.embed_client = OpenAiEmbeddingClient::new(&config.embedding);
        self.embedding_enabled = config.embedding.enabled;
        self.context_gen = if config.context.enabled {
            Some(ContextGenerator::new(&config.chat))
        } else {
            None
        };
        let (vision_model, vision_caption_model) = configured_vision_model(config);
        self.vision_model = vision_model;
        self.vision_caption_model = vision_caption_model;
        self.index_gc_policy = config.index_gc.policy();
        self.embedding_batch_size = config.embedding.batch_size.max(1);
        self.embedding_max_concurrent_requests = config
            .embedding
            .endpoint_runtime
            .bounded()
            .max_concurrent_requests;
        self.memory_budget
            .configure_from(&config.daemon.resources)?;
        self.graph_extractor = if self.graph_extraction_config.enabled && config.chat.enabled {
            Some(GraphExtractor::from_config(&config.chat))
        } else {
            None
        };
        Ok(())
    }
}

impl<E> IngestPipeline<E>
where
    E: EmbeddingClient,
{
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Refuse a write-heavy ingest or index operation before it consumes the
    /// configured SQLite filesystem reserve.
    pub fn ensure_write_capacity(&self, operation: SqliteWriteOperation) -> Result<()> {
        self.store.ensure_write_capacity(operation)
    }

    /// Run the profile's scheduled WAL maintenance after an ingest/index job.
    pub fn checkpoint_wal(&self) -> Result<()> {
        self.store.checkpoint_wal().map(|_| ())
    }

    pub fn hnsw(&self) -> &HnswIndex {
        &self.hnsw
    }

    pub fn vector_residency(&self) -> VectorIndexResidency {
        self.vector_residency
    }

    pub fn active_embedding_profile_id(&self) -> &EmbeddingProfileId {
        &self.active_profile_id
    }

    pub fn memory_budget(&self) -> MemoryBudget {
        self.memory_budget.clone()
    }

    pub fn plan_embedding_profile_delete(
        &self,
        profile_id: &EmbeddingProfileId,
    ) -> Result<IndexProfileDeletePlan> {
        plan_index_profile_delete(
            &self.data_dir,
            &self.store,
            profile_id,
            &self.active_profile_id,
        )
    }

    pub fn delete_embedding_profile_index_data(
        &mut self,
        profile_id: &EmbeddingProfileId,
        allow_active: bool,
    ) -> Result<(IndexProfileDeletePlan, IndexProfileDeleteApplyReport)> {
        let plan = plan_index_profile_delete(
            &self.data_dir,
            &self.store,
            profile_id,
            &self.active_profile_id,
        )?;
        if plan.active_profile && !allow_active {
            bail!(
                "refusing to delete active embedding profile {}; pass allow_active to clear active profile artifacts",
                profile_id
            );
        }

        let index_publish_permit =
            acquire_ingest_resource_blocking("index_publish", "index_publish")?;
        let mut apply = apply_index_profile_delete_artifacts(&self.data_dir, profile_id, &plan)?;
        drop(index_publish_permit);

        let sqlite_write_permit =
            acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write")?;
        apply_index_profile_delete_sqlite(&self.store, profile_id, &mut apply)?;
        drop(sqlite_write_permit);

        if self.loaded_profile_id == *profile_id || self.active_profile_id == *profile_id {
            self.hnsw.clear();
            self.loaded_profile_id = profile_id.clone();
        }
        Ok((plan, apply))
    }

    pub fn active_ocr_profile(&self) -> Option<crate::types::OcrProfile> {
        self.ocr_provider
            .as_ref()
            .map(|provider| provider.profile())
    }

    pub async fn refresh_embedding_profile_capabilities(&mut self) -> Result<bool> {
        if !self.embedding_enabled {
            return Ok(false);
        }
        self.sync_pending_qdrant_profile_resets().await;
        let previous_hash = self.embedding_profile_spec.config_hash();
        let capabilities = self.embed_client.endpoint_capabilities().await?;
        self.embedding_profile_spec
            .apply_endpoint_capabilities(capabilities);
        let new_hash = self.embedding_profile_spec.config_hash();
        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        let reset_vectors = self.store.ensure_embedding_profile(
            &self.active_profile_id,
            self.embedding_profile_spec.as_store_config(),
        )?;
        drop(sqlite_write_permit);
        if reset_vectors {
            self.hnsw.clear();
            self.loaded_profile_id = self.active_profile_id.clone();
            #[cfg(feature = "qdrant")]
            {
                let profile_id = self.active_profile_id.clone();
                self.sync_qdrant_profile_all(&profile_id).await;
            }
        }
        Ok(previous_hash != new_hash)
    }

    /// Apply already-discovered embedding endpoint capabilities without doing
    /// provider I/O while the live pipeline slot is held.
    pub fn apply_embedding_profile_capabilities(
        &mut self,
        capabilities: EmbeddingEndpointCapabilities,
    ) -> Result<bool> {
        if !self.embedding_enabled {
            return Ok(false);
        }
        let previous_hash = self.embedding_profile_spec.config_hash();
        self.embedding_profile_spec
            .apply_endpoint_capabilities(capabilities);
        let new_hash = self.embedding_profile_spec.config_hash();
        let sqlite_write_permit =
            acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write")?;
        let reset_vectors = self.store.ensure_embedding_profile(
            &self.active_profile_id,
            self.embedding_profile_spec.as_store_config(),
        )?;
        drop(sqlite_write_permit);
        if reset_vectors {
            self.hnsw.clear();
            self.loaded_profile_id = self.active_profile_id.clone();
            #[cfg(feature = "qdrant")]
            self.pending_qdrant_profile_syncs
                .push(self.active_profile_id.clone());
        }
        Ok(previous_hash != new_hash)
    }

    pub fn vector_index(&self) -> &dyn VectorIndex {
        &self.hnsw
    }

    pub fn vector_index_for_profile(
        &mut self,
        profile_id: &EmbeddingProfileId,
    ) -> Result<&dyn VectorIndex> {
        self.select_embedding_profile(profile_id)?;
        Ok(&self.hnsw)
    }

    pub fn select_embedding_profile(&mut self, profile_id: &EmbeddingProfileId) -> Result<()> {
        let sqlite_write_permit =
            acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write")?;
        let _ = self.ensure_embedding_profile(profile_id)?;
        drop(sqlite_write_permit);
        self.load_embedding_profile_index(profile_id)
    }

    fn load_embedding_profile_index(&mut self, profile_id: &EmbeddingProfileId) -> Result<()> {
        if self.loaded_profile_id == *profile_id {
            return Ok(());
        }
        self.hnsw = load_vector_index_for_residency(
            self.vector_residency,
            &self.data_dir,
            &self.store,
            profile_id,
        )?;
        self.loaded_profile_id = profile_id.clone();
        Ok(())
    }

    pub fn select_embedding_profile_readonly(
        &mut self,
        profile_id: &EmbeddingProfileId,
    ) -> Result<()> {
        self.load_embedding_profile_index(profile_id)
    }

    pub fn lexical_index(&self) -> SqliteFtsIndex<'_> {
        SqliteFtsIndex::new(&self.store)
    }

    pub fn fts_startup_maintenance(&self) -> FtsMaintenanceOutcome {
        self.fts_startup_maintenance
    }

    fn ensure_embedding_profile(&self, profile_id: &EmbeddingProfileId) -> Result<bool> {
        self.store
            .ensure_embedding_profile(profile_id, self.embedding_profile_spec.as_store_config())
    }

    #[cfg(test)]
    fn from_parts(store: Store, hnsw: HnswIndex, embed_client: E, data_dir: PathBuf) -> Self {
        let active_profile_id = EmbeddingProfileId::default_profile();
        let embedding_profile_spec = test_embedding_profile_spec(embed_client.dimension());
        store
            .ensure_embedding_profile(&active_profile_id, embedding_profile_spec.as_store_config())
            .unwrap();
        Self {
            store,
            hnsw,
            vector_residency: VectorIndexResidency::ResidentHnsw,
            loaded_profile_id: active_profile_id.clone(),
            active_profile_id,
            embedding_profile_spec,
            embedding_enabled: true,
            embed_client,
            context_gen: None,
            vision_model: None,
            graph_extractor: None,
            graph_extraction_config: GraphExtractionConfig::default(),
            #[cfg(feature = "qdrant")]
            qdrant: None,
            #[cfg(feature = "qdrant")]
            pending_qdrant_profile_syncs: Vec::new(),
            vision_caption_model: "vision-disabled".to_string(),
            vision_caption_prompt_hash: vision_caption_prompt_hash(),
            ocr_provider: None,
            data_dir,
            image_artifact_limits: ImageArtifactLimits::default(),
            index_gc_policy: IndexGcPolicy::default(),
            embedding_batch_size: 16,
            embedding_max_concurrent_requests: 4,
            memory_budget: MemoryBudget::new(
                None,
                crate::config::MemoryBudgetEnforcement::SlowWarn,
                std::time::Duration::from_millis(500),
                25,
            ),
            fts_startup_maintenance: FtsMaintenanceOutcome::default(),
            #[cfg(test)]
            source_commit_observer: None,
            #[cfg(all(test, feature = "qdrant"))]
            qdrant_requeue_store_observer: None,
            #[cfg(test)]
            fail_next_batched_index_stage_with_enospc: false,
            #[cfg(test)]
            fail_next_batched_compensation_write_with_readonly: false,
        }
    }

    #[cfg(test)]
    fn with_embedding_controls(
        mut self,
        batch_size: usize,
        max_concurrent_requests: usize,
    ) -> Self {
        self.embedding_batch_size = batch_size.max(1);
        self.embedding_max_concurrent_requests = max_concurrent_requests.max(1);
        self
    }

    #[cfg(test)]
    fn with_embedding_enabled(mut self, enabled: bool) -> Self {
        self.embedding_enabled = enabled;
        self
    }

    #[cfg(test)]
    fn with_memory_budget(mut self, memory_budget: MemoryBudget) -> Self {
        self.memory_budget = memory_budget;
        self
    }

    #[cfg(test)]
    fn with_source_commit_observer<F>(mut self, observer: F) -> Self
    where
        F: Fn(&Store, &SourceId) + Send + Sync + 'static,
    {
        self.source_commit_observer = Some(Box::new(observer));
        self
    }

    #[cfg(all(test, feature = "qdrant"))]
    fn with_qdrant_requeue_store_observer<F>(mut self, observer: F) -> Self
    where
        F: Fn(&Store) + Send + Sync + 'static,
    {
        self.qdrant_requeue_store_observer = Some(Arc::new(observer));
        self
    }

    #[cfg(test)]
    fn fail_next_batched_index_stage_with_enospc(&mut self) {
        self.fail_next_batched_index_stage_with_enospc = true;
    }

    #[cfg(test)]
    fn fail_next_batched_compensation_write_with_readonly(&mut self) {
        self.fail_next_batched_compensation_write_with_readonly = true;
    }

    #[cfg(test)]
    fn notify_source_committed_for_test(&self, source_id: &SourceId) {
        if let Some(observer) = &self.source_commit_observer {
            observer(&self.store, source_id);
        }
    }

    #[cfg(test)]
    fn with_ocr_provider<O>(mut self, provider: O) -> Self
    where
        O: OcrProvider + 'static,
    {
        self.ocr_provider = Some(Box::new(provider));
        self
    }

    #[cfg(test)]
    fn with_vision_model<V>(mut self, model_name: impl Into<String>, vision_model: V) -> Self
    where
        V: VisionModel + 'static,
    {
        self.vision_caption_model = model_name.into();
        self.vision_model = Some(Box::new(vision_model));
        self
    }

    #[cfg(test)]
    fn with_graph_extractor(
        mut self,
        config: GraphExtractionConfig,
        graph_extractor: GraphExtractor,
    ) -> Self {
        self.graph_extraction_config = config;
        self.graph_extractor = Some(graph_extractor);
        self
    }

    #[cfg(all(test, feature = "qdrant"))]
    fn with_qdrant_client(mut self, qdrant: QdrantClient) -> Self {
        self.qdrant = Some(qdrant);
        self
    }

    pub fn add_source(&self, path: &Path) -> Result<SourceId> {
        let abs_path = std::fs::canonicalize(path)
            .with_context(|| format!("resolve path: {}", path.display()))?;
        if let Some(source) = self.store.get_source_by_path(&abs_path)? {
            return Ok(source.id);
        }
        let id = SourceId::from_path(&abs_path);
        if let Some(existing) = self.store.get_source(&id)? {
            bail!(
                "source identity conflict: {} is already bound to {}, not {}",
                id.0,
                existing.path.display(),
                abs_path.display()
            );
        }

        let hash = file_hash(&abs_path)?;
        let source = Source {
            id: id.clone(),
            path: abs_path,
            hash,
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        };
        let sqlite_write_permit =
            acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write")?;
        self.store.add_source(&source)?;
        drop(sqlite_write_permit);
        Ok(id)
    }

    pub fn sync_collection(
        &self,
        collection_name: &str,
        path_inputs: &[CollectionSyncPathInput],
        max_depth: Option<usize>,
    ) -> Result<CollectionSyncReport> {
        self.sync_collection_with_extra_ignores(collection_name, path_inputs, max_depth, &[])
    }

    pub fn sync_collection_with_extra_ignores(
        &self,
        collection_name: &str,
        path_inputs: &[CollectionSyncPathInput],
        max_depth: Option<usize>,
        extra_ignore_patterns: &[String],
    ) -> Result<CollectionSyncReport> {
        let collection = self
            .store
            .get_collection(collection_name)?
            .with_context(|| format!("collection not found: {collection_name}"))?;
        let roots = self.store.list_collection_roots(collection_name)?;
        let mut ignore_patterns = collection.ignore_patterns;
        ignore_patterns.extend(extra_ignore_patterns.iter().cloned());
        let settings = CollectionSyncSettings {
            ignore_patterns,
            max_depth: max_depth.unwrap_or(DEFAULT_COLLECTION_SYNC_MAX_DEPTH),
        };
        let discovery = discover_collection_members(&roots, path_inputs, &settings);
        for candidate in &discovery.candidates {
            let source_id = self.add_source(&candidate.source_path)?;
            if source_id != candidate.source_id {
                bail!(
                    "collection sync source id mismatch for {}",
                    candidate.source_path.display()
                );
            }
        }
        let report = CollectionSyncReport::from_discovery(&discovery, settings.max_depth);
        let sqlite_write_permit =
            acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write")?;
        let report = self.store.replace_collection_members(
            collection_name,
            &discovery.candidates,
            report,
        )?;
        drop(sqlite_write_permit);
        Ok(report)
    }

    pub fn check_stale(&self) -> Result<Vec<SourceId>> {
        let sources = self.store.list_sources()?;
        let mut current_hashes = HashMap::new();
        for source in &sources {
            if source.path.exists() {
                let hash = file_hash(&source.path)?;
                current_hashes.insert(source.id.clone(), hash);
            }
        }
        let stale = if self.embedding_enabled {
            self.store
                .find_stale_sources_for_profile(&current_hashes, &self.active_profile_id)?
        } else {
            self.store
                .find_stale_sources_for_lexical_index(&current_hashes)?
        };
        let mut stale_set = stale.into_iter().collect::<HashSet<_>>();
        if let Some(provider) = &self.ocr_provider {
            let profile = provider.profile();
            for source in &sources {
                if stale_set.contains(&source.id) {
                    continue;
                }
                let evidence = self.store.list_evidence_by_source(&source.id)?;
                let image_artifacts = self.store.list_image_artifacts_by_source(&source.id)?;
                let diagnostics = source_ingest_diagnostics(
                    &source.path,
                    &evidence,
                    &image_artifacts,
                    Some(&profile),
                );
                if ocr_profile_stale(&diagnostics, Some(&profile)) {
                    stale_set.insert(source.id.clone());
                }
            }
        }
        let mut stale = stale_set.into_iter().collect::<Vec<_>>();
        stale.sort_by(|left, right| left.0.cmp(&right.0));
        if !stale.is_empty() {
            let sqlite_write_permit =
                acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write")?;
            for id in &stale {
                self.store.update_source_status(id, &SourceStatus::Stale)?;
            }
            drop(sqlite_write_permit);
        }
        Ok(stale)
    }

    /// Return whether a source currently needs ingest work without mutating source status.
    pub fn source_ingest_freshness(&self, source_id: &SourceId) -> Result<SourceIngestFreshness> {
        Ok(self.source_ingest_snapshot(source_id)?.freshness)
    }

    /// Return current source freshness and the current file hash used for the decision.
    pub fn source_ingest_snapshot(&self, source_id: &SourceId) -> Result<SourceIngestSnapshot> {
        let Some(source) = self.store.get_source(source_id)? else {
            return Ok(SourceIngestSnapshot {
                freshness: SourceIngestFreshness::Missing,
                current_hash: None,
            });
        };
        if !source.path.exists() {
            return Ok(SourceIngestSnapshot {
                freshness: SourceIngestFreshness::Missing,
                current_hash: None,
            });
        }
        let current_hash_value = file_hash(&source.path)?;
        let current_hash = Some(current_hash_value.clone());
        if current_hash_value != source.hash {
            return Ok(SourceIngestSnapshot {
                freshness: SourceIngestFreshness::NeedsIngest(SourceIngestStaleReason::HashChanged),
                current_hash,
            });
        }
        if source.status != SourceStatus::Indexed || source.parser_used.is_none() {
            return Ok(SourceIngestSnapshot {
                freshness: SourceIngestFreshness::NeedsIngest(SourceIngestStaleReason::NotIndexed),
                current_hash,
            });
        }
        if self.embedding_enabled
            && self
                .store
                .source_vectors_stale_for_profile(&self.active_profile_id, source_id)?
        {
            return Ok(SourceIngestSnapshot {
                freshness: SourceIngestFreshness::NeedsIngest(
                    SourceIngestStaleReason::VectorsStale,
                ),
                current_hash,
            });
        }
        if let Some(provider) = &self.ocr_provider {
            let profile = provider.profile();
            let evidence = self.store.list_evidence_by_source(source_id)?;
            let image_artifacts = self.store.list_image_artifacts_by_source(source_id)?;
            let diagnostics = source_ingest_diagnostics(
                &source.path,
                &evidence,
                &image_artifacts,
                Some(&profile),
            );
            if ocr_profile_stale(&diagnostics, Some(&profile)) {
                return Ok(SourceIngestSnapshot {
                    freshness: SourceIngestFreshness::NeedsIngest(
                        SourceIngestStaleReason::OcrStale,
                    ),
                    current_hash,
                });
            }
        }
        Ok(SourceIngestSnapshot {
            freshness: SourceIngestFreshness::Fresh,
            current_hash,
        })
    }

    pub fn index_status(&self) -> Result<IndexStatusResponse> {
        let sources = self.store.list_sources()?;
        let mut current_hashes = HashMap::new();
        for source in &sources {
            if source.path.exists() {
                let hash = file_hash(&source.path)?;
                current_hashes.insert(source.id.clone(), hash);
            }
        }
        let stale_source_ids = if self.embedding_enabled {
            self.store
                .find_stale_sources_for_profile(&current_hashes, &self.active_profile_id)?
        } else {
            self.store
                .find_stale_sources_for_lexical_index(&current_hashes)?
        };
        let mut messages = Vec::new();
        if self.embedding_enabled {
            messages.push(
                "Embedding profile compatibility is capability-fingerprinted: endpoint identity, requested/served model, dimension, normalize flag, dtype, quantization, weight identity, context window, and chunking policy are part of the active profile."
                    .to_string(),
            );
            if let Some(context) = self.embedding_profile_spec.max_context_tokens {
                let budget = self
                    .embedding_profile_spec
                    .embedding_input_budget_tokens
                    .unwrap_or_else(|| safe_embedding_input_budget(context));
                messages.push(format!(
                    "Chunking uses a safe embedding input budget of {budget} token(s), derived from the effective {context}-token context window. Context shrink requires reingest/reindex; context growth is a quality reindex opportunity."
                ));
            } else {
                messages.push(
                    "No embedding context window is known, so chunking uses the default parent/child policy until the endpoint exposes a context window or [embedding].context_window_tokens is configured."
                        .to_string(),
                );
            }
            if !stale_source_ids.is_empty() {
                messages.push(format!(
                    "{} source(s) are stale for the current capability/chunking profile and should be ingested or reindexed.",
                    stale_source_ids.len()
                ));
            }
        } else {
            messages.push(
                "Embedding is disabled; index status is BM25/lexical-only and no embedding capability fingerprint is required."
                    .to_string(),
            );
        }
        let chunker_config = &self.embedding_profile_spec.chunker_config;
        IndexStatusResponse::new(IndexStatusResponseFields(
            self.embedding_enabled,
            self.active_profile_id.as_str().to_string(),
            sources.len(),
            stale_source_ids.len(),
            stale_source_ids.into_iter().map(|id| id.0).collect(),
            EmbeddingCapabilityStatusResponse {
                provider: self.embedding_profile_spec.provider.clone(),
                model: self.embedding_profile_spec.model.clone(),
                dimension: self.embedding_profile_spec.dimension,
                normalize: self.embedding_profile_spec.normalize,
                endpoint_identity: self.embedding_profile_spec.endpoint_identity.clone(),
                requested_model: self.embedding_profile_spec.requested_model.clone(),
                served_model: self.embedding_profile_spec.served_model.clone(),
                max_context_tokens: self.embedding_profile_spec.max_context_tokens,
                dtype: self.embedding_profile_spec.dtype.clone(),
                quantization: self.embedding_profile_spec.quantization.clone(),
                weight_identity: self.embedding_profile_spec.weight_identity.clone(),
            },
            ChunkingProfileStatusResponse {
                version: CHUNKER_VERSION.to_string(),
                child_target_tokens: chunker_config.child_target_tokens,
                child_overlap_tokens: chunker_config.child_overlap_tokens,
                parent_children_count: chunker_config.parent_children_count,
                embedding_input_budget_tokens: self
                    .embedding_profile_spec
                    .embedding_input_budget_tokens,
            },
            messages,
        ))
    }

    pub async fn ingest_source(&mut self, source_id: &SourceId) -> Result<EmbeddingCacheStats> {
        self.ingest_source_inner(source_id, None).await
    }

    pub async fn ingest_source_with_task(
        &mut self,
        source_id: &SourceId,
        task_id: &TaskId,
    ) -> Result<EmbeddingCacheStats> {
        self.ingest_source_inner(source_id, Some(task_id)).await
    }

    /// Ingest already-queued source tasks as one bounded cross-source embedding batch stream.
    pub async fn ingest_sources_with_tasks(
        &mut self,
        source_tasks: &[(SourceId, TaskId)],
    ) -> Vec<SourceIngestOutcome> {
        self.ingest_sources_with_tasks_reporting(source_tasks, |_| async {})
            .await
    }

    /// Ingest queued source tasks and report each source outcome as soon as it is known.
    pub async fn ingest_sources_with_tasks_reporting<F, Fut>(
        &mut self,
        source_tasks: &[(SourceId, TaskId)],
        mut report_outcome: F,
    ) -> Vec<SourceIngestOutcome>
    where
        F: FnMut(SourceIngestOutcome) -> Fut,
        Fut: Future<Output = ()>,
    {
        if let Err(error) = self.refresh_embedding_profile_capabilities().await {
            let mut outcomes = Vec::with_capacity(source_tasks.len());
            for (source_id, task_id) in source_tasks {
                push_reported_source_outcome(
                    &mut outcomes,
                    SourceIngestOutcome {
                        source_id: source_id.clone(),
                        task_id: task_id.clone(),
                        result: Err(error.to_string()),
                    },
                    &mut report_outcome,
                )
                .await;
            }
            return outcomes;
        }
        let active_profile_id = self.active_profile_id.clone();
        let mut pending = PendingPreparedSources::default();
        let mut outcomes = Vec::with_capacity(source_tasks.len());
        for (source_id, task_id) in source_tasks {
            match self.prepare_source_contents(source_id, Some(task_id)).await {
                Ok(prepared_source) => {
                    if self.should_flush_before_pending_push(
                        &pending,
                        prepared_source.prepared_artifact_bytes(),
                    ) {
                        let prepared_outcomes = self
                            .flush_pending_prepared_sources(
                                &active_profile_id,
                                &mut pending,
                                BatchCancellationScope::PerSourceTask,
                            )
                            .await;
                        push_reported_prepared_source_outcomes(
                            &mut outcomes,
                            prepared_outcomes,
                            &mut report_outcome,
                        )
                        .await;
                    }
                    pending.push(prepared_source);
                    if self.should_flush_pending_sources(&pending) {
                        let prepared_outcomes = self
                            .flush_pending_prepared_sources(
                                &active_profile_id,
                                &mut pending,
                                BatchCancellationScope::PerSourceTask,
                            )
                            .await;
                        push_reported_prepared_source_outcomes(
                            &mut outcomes,
                            prepared_outcomes,
                            &mut report_outcome,
                        )
                        .await;
                    }
                }
                Err(error) => {
                    push_reported_source_outcome(
                        &mut outcomes,
                        SourceIngestOutcome {
                            source_id: source_id.clone(),
                            task_id: task_id.clone(),
                            result: Err(error.to_string()),
                        },
                        &mut report_outcome,
                    )
                    .await;
                }
            }
        }
        if !pending.is_empty() {
            let prepared_outcomes = self
                .flush_pending_prepared_sources(
                    &active_profile_id,
                    &mut pending,
                    BatchCancellationScope::PerSourceTask,
                )
                .await;
            push_reported_prepared_source_outcomes(
                &mut outcomes,
                prepared_outcomes,
                &mut report_outcome,
            )
            .await;
        }
        outcomes
    }
    async fn ingest_source_inner(
        &mut self,
        source_id: &SourceId,
        task_id: Option<&TaskId>,
    ) -> Result<EmbeddingCacheStats> {
        self.refresh_embedding_profile_capabilities().await?;
        let active_profile_id = self.active_profile_id.clone();
        let prepared_source = self.prepare_source_contents(source_id, task_id).await?;
        let outcomes = self
            .embed_and_commit_prepared_sources(
                &active_profile_id,
                vec![prepared_source],
                BatchCancellationScope::SharedTask {
                    completed_sources_before_batch: 0,
                    total_sources: 1,
                },
            )
            .await;
        let Some(outcome) = outcomes.into_iter().next() else {
            return Ok(EmbeddingCacheStats::default());
        };
        match outcome.result {
            Ok(cache_stats) => Ok(cache_stats),
            Err(error) => bail!(error),
        }
    }

    async fn prepare_source_contents(
        &self,
        source_id: &SourceId,
        task_id: Option<&TaskId>,
    ) -> Result<PreparedSourceIngest> {
        let source = self
            .store
            .get_source(source_id)?
            .with_context(|| format!("source not found: {}", source_id.0))?;

        tracing::info!(source = %source_id.0, path = %source.path.display(), "ingesting");

        let hash = file_hash(&source.path)?;
        let mut new_source = Source {
            id: source_id.clone(),
            path: source.path.clone(),
            hash,
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        };

        let parser = parser::parser_for_extension(&source.path)?;
        tracing::info!(parser = parser.name(), "parsing");
        new_source.status = SourceStatus::Indexed;
        new_source.parser_used = Some(parser.name().to_string());
        let phase = PhaseTiming::start(IngestTaskStage::Parse.as_str());
        self.record_task_progress(
            task_id,
            phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("parsing source")
                .with_resource(waiting_resource_progress("cpu_worker", "cpu")),
        );
        let cpu_permit = acquire_ingest_resource("cpu_worker", "cpu").await?;
        let (mut evidence, prepared_image_artifacts, pdf_scan) = {
            let _cpu_permit = cpu_permit;
            let mut parsed_evidence = parser.parse(&source.path)?;
            crate::pdf_selector::attach_pdf_selectors(
                &mut parsed_evidence,
                &new_source.hash,
                parser.name(),
            );
            let evidence = remap_parser_evidence_identity(
                parsed_evidence,
                &SourceId::from_path(&source.path),
                source_id,
            )?;
            let parsed_image_artifacts = extract_image_artifacts_for_ingest(
                parser.as_ref(),
                &source.path,
                self.image_artifact_limits,
            )?;
            let prepared_image_artifacts = prepare_image_artifacts(
                &self.data_dir,
                source_id,
                evidence.len() as u32,
                parsed_image_artifacts,
                self.image_artifact_limits,
            )?;
            let pdf_scan = source_pdf_page_count(&source.path)?
                .and_then(|page_count| {
                    pdf_scan_summary_with_page_count(
                        Some(page_count),
                        &evidence,
                        &prepared_image_artifacts.artifacts,
                    )
                })
                .or_else(|| pdf_scan_summary(&evidence, &prepared_image_artifacts.artifacts));
            (evidence, prepared_image_artifacts, pdf_scan)
        };
        if pdf_scan.as_ref().is_some_and(|scan| {
            scan.page_count > 0 && scan.image_only_page_count == scan.page_count
        }) {
            bail!(IngestDiagnosticCode::PdfNoUsableTextLayer);
        }
        self.record_task_phase(
            task_id,
            phase,
            serde_json::json!({
                "source_id": source_id.0,
                "evidence_count": evidence.len(),
                "image_artifact_count": prepared_image_artifacts.artifacts.len(),
                "pdf_scan": pdf_scan,
            }),
        );
        let phase = PhaseTiming::start(IngestTaskStage::Ocr.as_str());
        self.record_task_progress(
            task_id,
            phase
                .progress_snapshot()
                .with_counter(
                    "pages",
                    pdf_scan
                        .as_ref()
                        .map(|scan| scan.pages.len() as u64)
                        .unwrap_or(0),
                    None,
                )
                .with_recent_status("checking OCR candidates"),
        );
        let ocr_evidence = self
            .ocr_scanned_pdf_pages(
                source_id,
                &source.path,
                pdf_scan.as_ref(),
                evidence.len() as u32,
                task_id,
            )
            .await?;
        self.record_task_phase(
            task_id,
            phase,
            serde_json::json!({
                "source_id": source_id.0,
                "ocr_evidence_count": ocr_evidence.len(),
                "ocr_enabled": self.ocr_provider.is_some(),
                "ocr_profile_hash": self.ocr_provider.as_ref().map(|provider| provider.profile().profile_hash()),
            }),
        );
        let phase = PhaseTiming::start(IngestTaskStage::ImageCaption.as_str());
        self.caption_prepared_image_artifacts(source_id, &prepared_image_artifacts)
            .await?;
        self.record_task_phase(
            task_id,
            phase,
            serde_json::json!({
                "operation": "image_caption",
                "caption_count": prepared_image_artifacts.artifacts.len(),
            }),
        );
        let searchable_evidence = evidence.clone();
        tracing::info!(
            evidence_count = searchable_evidence.len(),
            ocr_evidence_count = ocr_evidence.len(),
            "parsed searchable evidence"
        );

        let chunker_config = self.embedding_profile_spec.chunker_config.clone();
        let canonical_config = self.embedding_profile_spec.canonical_chunker_config.clone();
        let phase = PhaseTiming::start(IngestTaskStage::Chunk.as_str());
        self.record_task_progress(
            task_id,
            phase
                .progress_snapshot()
                .with_counter("evidence", searchable_evidence.len() as u64, None)
                .with_recent_status("chunking evidence")
                .with_resource(waiting_resource_progress("cpu_worker", "cpu")),
        );
        let cpu_permit = acquire_ingest_resource("cpu_worker", "cpu").await?;
        let partitioned = {
            let _cpu_permit = cpu_permit;
            chunk_partition::chunk_searchable_evidence_by_locator(
                source_id,
                &searchable_evidence,
                &chunker_config,
                &canonical_config,
            )?
        };
        let output = partitioned.output;
        tracing::info!(chunk_count = output.chunks.len(), "chunked");
        self.record_task_phase(
            task_id,
            phase,
            serde_json::json!({
                "source_id": source_id.0,
                "chunk_count": output.chunks.len(),
                "chunker_version": CHUNKER_VERSION,
                "canonical_evidence_count": partitioned.canonical_evidence_count,
                "noncanonical_evidence_count": partitioned.noncanonical_evidence_count,
                "chunking_strategies": partitioned.strategies_used,
                "child_target_tokens": chunker_config.child_target_tokens,
                "child_overlap_tokens": chunker_config.child_overlap_tokens,
                "parent_children_count": chunker_config.parent_children_count,
                "canonical_chunker_version": CANONICAL_CHUNKER_VERSION,
                "canonical_target_tokens": canonical_config.target_tokens,
                "canonical_overlap_units": canonical_config.overlap_units,
                "canonical_max_units_per_child": canonical_config.max_units_per_child,
                "embedding_input_budget_tokens": self.embedding_profile_spec.embedding_input_budget_tokens,
                "embedding_context_tokens": self.embedding_profile_spec.max_context_tokens,
            }),
        );

        let mut chunks = output.chunks;
        let links = output.links;
        let evidence_spans = output.evidence_spans;
        if let Some(ctx_gen) = &self.context_gen {
            let phase = PhaseTiming::start(IngestTaskStage::ContextualRetrieval.as_str());
            let title = source
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document");
            let enriched = ctx_gen.enrich_chunks(&mut chunks, title, 8).await?;
            tracing::info!(enriched, "contextual retrieval done");
            self.record_task_phase(
                task_id,
                phase,
                serde_json::json!({
                    "operation": "contextual_retrieval",
                    "enriched": enriched,
                }),
            );
        }

        let phase = PhaseTiming::start(IngestTaskStage::GraphExpansion.as_str());
        self.record_task_progress(
            task_id,
            phase
                .progress_snapshot()
                .with_recent_status("building evidence graph")
                .with_resource(waiting_resource_progress("cpu_worker", "cpu")),
        );
        let cpu_permit = acquire_ingest_resource("cpu_worker", "cpu").await?;
        evidence = searchable_evidence;
        evidence.extend(prepared_image_artifacts.evidence.clone());
        let ingest_diagnostics = source_ingest_diagnostics(
            &source.path,
            &evidence,
            &prepared_image_artifacts.artifacts,
            self.ocr_provider
                .as_ref()
                .map(|provider| provider.profile())
                .as_ref(),
        );
        let (mut graph_nodes, mut graph_edges) = build_evidence_graph(
            &new_source,
            &evidence,
            &chunks,
            &links,
            &prepared_image_artifacts.artifacts,
            &prepared_image_artifacts.text_proximities,
        );
        drop(cpu_permit);
        self.record_task_event(
            task_id,
            "diagnostic",
            "source ingest diagnostics",
            serde_json::json!({
                "source_id": source_id.0,
                "diagnostics": ingest_diagnostics,
            }),
        );
        self.append_generated_graph(&new_source, &chunks, &mut graph_nodes, &mut graph_edges)
            .await;
        self.record_task_phase(
            task_id,
            phase,
            serde_json::json!({
                "source_id": source_id.0,
                "graph_node_count": graph_nodes.len(),
                "graph_edge_count": graph_edges.len(),
            }),
        );

        let active_profile_config_hash = self.embedding_profile_spec.config_hash();
        self.assign_embedding_input_hashes(&mut chunks, &active_profile_config_hash);
        let child_chunks = chunks
            .iter()
            .filter(|chunk| chunk.chunk_type == ChunkType::Child)
            .cloned()
            .collect::<Vec<_>>();
        let embedding_phase = PhaseTiming::start(IngestTaskStage::EmbeddingQueueWait.as_str());
        self.record_task_progress(
            task_id,
            embedding_phase
                .progress_snapshot()
                .with_counter("embedding_vectors", 0, Some(child_chunks.len() as u64))
                .with_counter("embedding_cache_hits", 0, Some(child_chunks.len() as u64))
                .with_counter("embedding_cache_misses", 0, Some(child_chunks.len() as u64))
                .with_counter(
                    "batches",
                    0,
                    Some(embedding_request_count(
                        child_chunks.len(),
                        self.embedding_batch_size,
                    )),
                )
                .with_counter("embedding_batch_sources", 1, None)
                .with_counter(
                    "embedding_in_flight_limit",
                    self.embedding_max_concurrent_requests as u64,
                    None,
                )
                .with_wait_reason("embedding_batch")
                .with_recent_status("waiting for embedding batch"),
        );
        Ok(PreparedSourceIngest {
            task_id: task_id.cloned(),
            source: new_source,
            evidence,
            chunks,
            links,
            evidence_spans,
            image_artifacts: prepared_image_artifacts,
            graph_nodes,
            graph_edges,
            child_chunks,
            embedding_phase,
        })
    }

    fn prepared_source_is_noop_ingest(
        &self,
        profile_id: &EmbeddingProfileId,
        source: &Source,
        child_chunk_count: usize,
        cache_stats: &EmbeddingCacheStats,
    ) -> Result<bool> {
        if !self.embedding_enabled
            || cache_stats.changed_chunks != 0
            || cache_stats.cache_misses != 0
            || cache_stats.embedded_chunks != 0
            || cache_stats.reused_chunks != child_chunk_count
        {
            return Ok(false);
        }
        let Some(existing) = self.store.get_source(&source.id)? else {
            return Ok(false);
        };
        if existing.status != SourceStatus::Indexed
            || existing.hash != source.hash
            || existing.parser_used != source.parser_used
        {
            return Ok(false);
        }
        Ok(!self
            .store
            .source_vectors_stale_for_profile(profile_id, &source.id)?)
    }

    fn prepared_source_has_fresh_committed_vectors(
        &self,
        profile_id: &EmbeddingProfileId,
        source: &Source,
    ) -> Result<bool> {
        if !self.embedding_enabled {
            return Ok(false);
        }
        let Some(existing) = self.store.get_source(&source.id)? else {
            return Ok(false);
        };
        if existing.status != SourceStatus::Indexed
            || existing.hash != source.hash
            || existing.parser_used != source.parser_used
        {
            return Ok(false);
        }
        Ok(!self
            .store
            .source_vectors_stale_for_profile(profile_id, &source.id)?)
    }

    fn noop_embedding_cache_stats(child_chunk_count: usize) -> EmbeddingCacheStats {
        EmbeddingCacheStats {
            cache_hits: child_chunk_count,
            reused_chunks: child_chunk_count,
            ..EmbeddingCacheStats::default()
        }
    }

    fn record_noop_source_ingest(
        &self,
        task_id: Option<&TaskId>,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
        child_chunk_count: usize,
        cache_stats: &EmbeddingCacheStats,
    ) {
        tracing::info!(
            source = %source_id.0,
            embedding_profile_id = %profile_id,
            reused_chunks = cache_stats.reused_chunks,
            changed_chunks = cache_stats.changed_chunks,
            "skipping unchanged source ingest before vector index publish"
        );
        self.record_task_progress(
            task_id,
            TaskProgressSnapshot::phase(IngestTaskStage::TaskTerminalize.as_str())
                .with_counter("sources", 1, Some(1))
                .with_counter("reused_chunks", cache_stats.reused_chunks as u64, None)
                .with_counter("changed_chunks", cache_stats.changed_chunks as u64, None)
                .with_recent_status("source unchanged; vector index publish skipped")
                .with_active_worker_kind("ingest"),
        );
        self.record_task_event(
            task_id,
            "noop",
            "source ingest skipped because content and metadata are unchanged",
            serde_json::json!({
                "operation": "source_ingest_noop",
                "reason": "unchanged_source",
                "source_id": source_id.0.as_str(),
                "embedding_profile_id": profile_id.as_str(),
                "child_chunks": child_chunk_count,
                "embedding_cache": cache_stats,
                "vector_index_published": false,
            }),
        );
    }

    async fn commit_prepared_source(
        &mut self,
        profile_id: &EmbeddingProfileId,
        prepared_source: PreparedSourceIngest,
        vectors: Vec<VectorDocument>,
        cache_stats: EmbeddingCacheStats,
    ) -> Result<EmbeddingCacheStats> {
        if self.prepared_source_is_noop_ingest(
            profile_id,
            &prepared_source.source,
            prepared_source.child_chunks.len(),
            &cache_stats,
        )? {
            self.record_noop_source_ingest(
                prepared_source.task_id.as_ref(),
                profile_id,
                &prepared_source.source.id,
                prepared_source.child_chunks.len(),
                &cache_stats,
            );
            return Ok(cache_stats);
        }
        let PreparedSourceIngest {
            task_id,
            source,
            evidence,
            chunks,
            links,
            evidence_spans,
            image_artifacts,
            graph_nodes,
            graph_edges,
            child_chunks: _,
            embedding_phase: _,
        } = prepared_source;
        let source_id = source.id.clone();
        let task_id = task_id.as_ref();
        let vector_build_phase = PhaseTiming::start(IngestTaskStage::VectorIndex.as_str());
        self.record_task_progress(
            task_id,
            vector_build_phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("building vector index")
                .with_active_worker_kind("ingest")
                .with_resource(waiting_resource_progress("cpu_worker", "cpu")),
        );
        let cpu_permit = acquire_ingest_resource("cpu_worker", "cpu").await?;
        let prepared =
            self.prepare_source_indexes_from_vectors(profile_id, &source_id, vectors, cache_stats)?;
        let completed_cpu_resource = completed_resource_progress(&cpu_permit);
        drop(cpu_permit);
        self.record_resource_timing(task_id, completed_cpu_resource);
        self.record_task_progress(
            task_id,
            vector_build_phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("staging index artifacts")
                .with_active_worker_kind("ingest")
                .with_resource(waiting_resource_progress("index_publish", "index_publish")),
        );
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        let staged = self.stage_prepared_index_artifacts_for_residency(&prepared)?;
        let completed_index_stage_resource = completed_resource_progress(&index_publish_permit);
        drop(index_publish_permit);
        self.record_resource_timing(task_id, completed_index_stage_resource);
        self.record_task_phase(
            task_id,
            vector_build_phase,
            serde_json::json!({
                "operation": "build_and_stage_vector_index",
                "embedding_profile_id": profile_id.as_str(),
                "source_id": source_id.0.as_str(),
                "source_vector_count": prepared.vectors.len(),
            }),
        );
        let written_image_files =
            match write_image_artifact_files(&image_artifacts.files, self.image_artifact_limits) {
                Ok(written) => written,
                Err(err) => {
                    remove_staged_index_artifacts(&staged);
                    return Err(err);
                }
            };
        let io_telemetry = SourceCommitIoTelemetry::new(SourceCommitIoTelemetryInputs {
            evidence: &evidence,
            chunks: &chunks,
            links: &links,
            image_artifacts: &image_artifacts,
            graph_nodes: &graph_nodes,
            graph_edges: &graph_edges,
            vectors: &prepared.vectors,
            written_image_files: &written_image_files,
        });
        let db_phase = PhaseTiming::start(IngestTaskStage::SqliteWrite.as_str());
        self.record_task_progress(
            task_id,
            db_phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("committing source contents")
                .with_active_worker_kind("ingest"),
        );
        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        self.record_task_progress_with_writer_permit(
            task_id,
            db_phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("committing source contents")
                .with_active_worker_kind("ingest")
                .with_resource(task_resource_progress(&sqlite_write_permit, "active")),
            &sqlite_write_permit,
        );
        let replacement_report =
            match self
                .store
                .replace_source_contents(SourceContentsReplacement {
                    source: &source,
                    evidence: &evidence,
                    chunks: &chunks,
                    embedding_profile_id: profile_id,
                    vectors: &prepared.vectors,
                    links: &links,
                    evidence_spans: &evidence_spans,
                    image_artifacts: &image_artifacts.artifacts,
                    graph_nodes: &graph_nodes,
                    graph_edges: &graph_edges,
                }) {
                Ok(generation) => generation,
                Err(err) => {
                    cleanup_written_image_files(&written_image_files);
                    remove_staged_index_artifacts(&staged);
                    return Err(err);
                }
            };
        let completed_sqlite_resource = completed_resource_progress(&sqlite_write_permit);
        drop(sqlite_write_permit);
        self.record_resource_timing(task_id, completed_sqlite_resource);
        let generation = replacement_report.generation;
        let lexical_update = replacement_report.lexical_update;
        self.record_task_phase(
            task_id,
            db_phase,
            io_telemetry.db_metadata(&source_id, profile_id, generation),
        );
        self.record_task_progress(
            task_id,
            TaskProgressSnapshot::phase(IngestTaskStage::Bm25Index.as_str())
                .with_counter(
                    "indexed_child_chunks",
                    usize_to_u64(lexical_update.indexed_child_chunks),
                    None,
                )
                .with_recent_status("lexical index updated")
                .with_active_worker_kind("ingest"),
        );
        self.record_finished_task_phase(
            task_id,
            FinishedPhaseTiming {
                phase: IngestTaskStage::Bm25Index.as_str().into(),
                started_at: lexical_update.started_at,
                duration_ms: lexical_update.duration_ms,
                metadata: serde_json::json!({
                    "operation": "sqlite_fts_triggered_chunk_index",
                    "backend": "sqlite_fts5",
                    "source_id": source_id.0.as_str(),
                    "triggered_by": ["delete_source_cascade", "insert_child_chunks"],
                    "deleted_child_chunks": lexical_update.deleted_child_chunks,
                    "indexed_child_chunks": lexical_update.indexed_child_chunks,
                }),
            },
        );
        let staged_index_stats = staged_index_artifact_stats(staged.as_deref(), generation);
        let cache_stats = prepared.cache_stats.clone();
        let vector_publish_phase = PhaseTiming::start(IngestTaskStage::VectorIndex.as_str());
        self.record_task_progress(
            task_id,
            vector_publish_phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("publishing index artifacts")
                .with_active_worker_kind("ingest"),
        );
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        self.publish_committed_indexes(profile_id, generation, staged, prepared)?;
        let completed_index_publish_resource = completed_resource_progress(&index_publish_permit);
        drop(index_publish_permit);
        self.record_resource_timing(task_id, completed_index_publish_resource);
        self.record_task_progress(
            task_id,
            vector_publish_phase
                .progress_snapshot()
                .with_counter("sources", 1, Some(1))
                .with_wait_reason("post_publish_cleanup")
                .with_recent_status("index publishing complete")
                .with_active_worker_kind("ingest"),
        );
        self.record_task_phase(
            task_id,
            vector_publish_phase,
            io_telemetry.index_publish_metadata(
                &source_id,
                profile_id,
                generation,
                staged_index_stats,
            ),
        );
        #[cfg(feature = "qdrant")]
        if self.qdrant.is_some() {
            let qdrant_phase = PhaseTiming::start(IngestTaskStage::QdrantSync.as_str());
            self.record_task_progress(
                task_id,
                qdrant_phase
                    .progress_snapshot()
                    .with_counter("sources", 0, Some(1))
                    .with_recent_status("syncing qdrant")
                    .with_active_worker_kind("ingest"),
            );
            self.sync_qdrant_source(&source_id).await;
            self.record_task_phase(
                task_id,
                qdrant_phase,
                serde_json::json!({
                    "operation": "qdrant_source_sync",
                    "embedding_profile_id": profile_id.as_str(),
                    "source_id": source_id.0.as_str(),
                }),
            );
        }
        cleanup_stale_source_image_artifacts(
            &self.data_dir,
            &source_id,
            &image_artifacts.artifacts,
        )
        .with_context(|| {
            format!(
                "cleanup stale image artifacts after committed source ingest: {}",
                source_id.0
            )
        })?;

        tracing::info!(source = %source_id.0, "ingest complete");
        Ok(cache_stats)
    }

    async fn commit_prepared_source_without_index_publish(
        &mut self,
        profile_id: &EmbeddingProfileId,
        batch_generation: Option<u64>,
        prepared_source: PreparedSourceIngest,
        vectors: Vec<VectorDocument>,
        cache_stats: EmbeddingCacheStats,
    ) -> Result<BatchedCommittedSource> {
        let PreparedSourceIngest {
            task_id,
            source,
            evidence,
            chunks,
            links,
            evidence_spans,
            image_artifacts,
            graph_nodes,
            graph_edges,
            child_chunks: _,
            embedding_phase: _,
        } = prepared_source;
        let source_id = source.id.clone();
        let task_id_ref = task_id.as_ref();
        let written_image_files =
            match write_image_artifact_files(&image_artifacts.files, self.image_artifact_limits) {
                Ok(written) => written,
                Err(err) => return Err(err),
            };
        let io_telemetry = SourceCommitIoTelemetry::new(SourceCommitIoTelemetryInputs {
            evidence: &evidence,
            chunks: &chunks,
            links: &links,
            image_artifacts: &image_artifacts,
            graph_nodes: &graph_nodes,
            graph_edges: &graph_edges,
            vectors: &vectors,
            written_image_files: &written_image_files,
        });
        let db_phase = PhaseTiming::start(IngestTaskStage::SqliteWrite.as_str());
        self.record_task_progress(
            task_id_ref,
            db_phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("committing source contents")
                .with_active_worker_kind("ingest"),
        );
        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        self.record_task_progress_with_writer_permit(
            task_id_ref,
            db_phase
                .progress_snapshot()
                .with_counter("sources", 0, Some(1))
                .with_recent_status("committing source contents")
                .with_active_worker_kind("ingest")
                .with_resource(task_resource_progress(&sqlite_write_permit, "active")),
            &sqlite_write_permit,
        );
        let replacement = SourceContentsReplacement {
            source: &source,
            evidence: &evidence,
            chunks: &chunks,
            embedding_profile_id: profile_id,
            vectors: &vectors,
            links: &links,
            evidence_spans: &evidence_spans,
            image_artifacts: &image_artifacts.artifacts,
            graph_nodes: &graph_nodes,
            graph_edges: &graph_edges,
        };
        // The first durable source mutation invalidates the old manifest in
        // the same transaction, before any fallible HNSW staging begins.
        let commit_result = match batch_generation {
            Some(generation) => self
                .store
                .replace_source_contents_without_generation(replacement)
                .map(|lexical_update| (lexical_update, generation, false)),
            None => self
                .store
                .replace_source_contents(replacement)
                .map(|report| (report.lexical_update, report.generation, true)),
        };
        let (lexical_update, index_generation, generation_advanced) = match commit_result {
            Ok(committed) => committed,
            Err(err) => {
                cleanup_written_image_files(&written_image_files);
                return Err(err);
            }
        };
        let completed_sqlite_resource = completed_resource_progress(&sqlite_write_permit);
        drop(sqlite_write_permit);
        self.record_resource_timing(task_id_ref, completed_sqlite_resource);
        self.record_task_phase(
            task_id_ref,
            db_phase,
            io_telemetry.batched_db_metadata(
                &source_id,
                profile_id,
                index_generation,
                generation_advanced,
            ),
        );
        self.record_task_progress(
            task_id_ref,
            TaskProgressSnapshot::phase(IngestTaskStage::Bm25Index.as_str())
                .with_counter(
                    "indexed_child_chunks",
                    usize_to_u64(lexical_update.indexed_child_chunks),
                    None,
                )
                .with_recent_status("lexical index updated")
                .with_active_worker_kind("ingest"),
        );
        self.record_finished_task_phase(
            task_id_ref,
            FinishedPhaseTiming {
                phase: IngestTaskStage::Bm25Index.as_str().into(),
                started_at: lexical_update.started_at,
                duration_ms: lexical_update.duration_ms,
                metadata: serde_json::json!({
                    "operation": "sqlite_fts_triggered_chunk_index",
                    "backend": "sqlite_fts5",
                    "source_id": source_id.0.as_str(),
                    "triggered_by": ["delete_source_cascade", "insert_child_chunks"],
                    "deleted_child_chunks": lexical_update.deleted_child_chunks,
                    "indexed_child_chunks": lexical_update.indexed_child_chunks,
                }),
            },
        );
        Ok(BatchedCommittedSource {
            source_id,
            task_id,
            index_generation,
            vector_count: vectors.len(),
            cache_stats,
            io_telemetry,
            retained_image_artifacts: image_artifacts.artifacts,
        })
    }

    fn mark_batched_committed_sources_failed_after_publish_error(
        &mut self,
        profile_id: &EmbeddingProfileId,
        committed_sources: &[BatchedCommittedSource],
        error: &str,
    ) {
        let source_vector_counts = committed_sources
            .iter()
            .map(|committed| (committed.source_id.clone(), committed.vector_count))
            .collect::<Vec<_>>();
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_batched_compensation_write_with_readonly) {
            self.store.set_query_only_for_test(true).unwrap();
        }
        let mark_result =
            self.store
                .set_source_embedding_failures(profile_id, &source_vector_counts, error);
        if let Err(mark_error) = mark_result {
            tracing::warn!(
                error = %mark_error,
                "failed to mark committed source batch stale after index publication failure"
            );
        }
        if let Err(invalidate_error) = self.invalidate_live_indexes() {
            tracing::warn!(
                error = %invalidate_error,
                "failed to invalidate live vector index after batched index publication failure"
            );
        }
    }

    async fn publish_batched_committed_source_indexes(
        &mut self,
        profile_id: &EmbeddingProfileId,
        committed_sources: &[BatchedCommittedSource],
    ) -> Result<u64> {
        let Some(first_committed) = committed_sources.first() else {
            return self.store.index_generation_for_profile(profile_id);
        };
        let generation = first_committed.index_generation;
        if committed_sources
            .iter()
            .any(|committed| committed.index_generation != generation)
        {
            bail!("batched source commits do not share one index generation");
        }
        let total_sources = usize_to_u64(committed_sources.len());
        let vector_build_phase = PhaseTiming::start(IngestTaskStage::VectorIndex.as_str());
        for committed in committed_sources {
            self.record_task_progress(
                committed.task_id.as_ref(),
                vector_build_phase
                    .progress_snapshot()
                    .with_counter("sources", 0, Some(total_sources))
                    .with_recent_status("building batched vector index")
                    .with_active_worker_kind("ingest")
                    .with_resource(waiting_resource_progress("cpu_worker", "cpu")),
            );
        }
        let cpu_permit = acquire_ingest_resource("cpu_worker", "cpu").await?;
        let memory_reservation = match self.vector_residency {
            VectorIndexResidency::LowMemory => None,
            VectorIndexResidency::ResidentHnsw => {
                let vector_count = self
                    .store
                    .count_vector_documents_for_profile(profile_id, None)?;
                let guard = self.reserve_vector_memory(
                    format!("ingest:batched_publish:{}", profile_id.as_str()),
                    "ingest:batched_publish",
                    vector_count,
                )?;
                let all_vectors = self.store.list_vector_documents_for_profile(profile_id)?;
                Some((guard, all_vectors))
            }
        };
        let prepared = match self.vector_residency {
            VectorIndexResidency::LowMemory => self.prepare_indexes_from_vectors(Vec::new())?,
            VectorIndexResidency::ResidentHnsw => {
                let (_, all_vectors) = memory_reservation
                    .as_ref()
                    .expect("resident_hnsw always produces memory_reservation");
                self.prepare_indexes_from_vectors(all_vectors.clone())?
            }
        };
        let completed_cpu_resource = completed_resource_progress(&cpu_permit);
        drop(cpu_permit);
        for committed in committed_sources {
            self.record_resource_timing(committed.task_id.as_ref(), completed_cpu_resource.clone());
        }
        let index_stage_permit = acquire_ingest_resource("index_publish", "index_publish").await?;
        for committed in committed_sources {
            self.record_task_progress(
                committed.task_id.as_ref(),
                vector_build_phase
                    .progress_snapshot()
                    .with_counter("sources", 0, Some(total_sources))
                    .with_recent_status("staging batched index artifacts")
                    .with_active_worker_kind("ingest")
                    .with_resource(task_resource_progress(&index_stage_permit, "active")),
            );
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_batched_index_stage_with_enospc) {
            let error = std::io::Error::from_raw_os_error(libc::ENOSPC);
            bail!("stage batched index artifacts: {error}");
        }
        let staged = self.stage_prepared_index_artifacts_for_residency(&prepared)?;
        let completed_index_stage_resource = completed_resource_progress(&index_stage_permit);
        drop(index_stage_permit);
        for committed in committed_sources {
            self.record_resource_timing(
                committed.task_id.as_ref(),
                completed_index_stage_resource.clone(),
            );
        }
        let persisted_generation = match self.store.index_generation_for_profile(profile_id) {
            Ok(persisted_generation) => persisted_generation,
            Err(err) => {
                remove_staged_index_artifacts(&staged);
                return Err(err);
            }
        };
        if persisted_generation != generation {
            remove_staged_index_artifacts(&staged);
            bail!(
                "batched index generation changed before publish: committed {generation}, current {persisted_generation}"
            );
        }
        let staged_index_stats = staged_index_artifact_stats(staged.as_deref(), generation);
        for committed in committed_sources {
            self.record_task_progress(
                committed.task_id.as_ref(),
                vector_build_phase
                    .progress_snapshot()
                    .with_counter("sources", 0, Some(total_sources))
                    .with_recent_status("publishing batched index artifacts")
                    .with_active_worker_kind("ingest")
                    .with_resource(waiting_resource_progress("index_publish", "index_publish")),
            );
        }
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        for committed in committed_sources {
            self.record_task_progress(
                committed.task_id.as_ref(),
                vector_build_phase
                    .progress_snapshot()
                    .with_counter("sources", 0, Some(total_sources))
                    .with_recent_status("publishing batched index artifacts")
                    .with_active_worker_kind("ingest")
                    .with_resource(task_resource_progress(&index_publish_permit, "active")),
            );
        }
        self.publish_committed_indexes(profile_id, generation, staged, prepared)?;
        let completed_index_publish_resource = completed_resource_progress(&index_publish_permit);
        drop(index_publish_permit);
        for committed in committed_sources {
            self.record_resource_timing(
                committed.task_id.as_ref(),
                completed_index_publish_resource.clone(),
            );
            self.record_task_progress(
                committed.task_id.as_ref(),
                vector_build_phase
                    .progress_snapshot()
                    .with_counter("sources", total_sources, Some(total_sources))
                    .with_wait_reason("post_publish_cleanup")
                    .with_recent_status("batched index publishing complete")
                    .with_active_worker_kind("ingest"),
            );
            self.record_task_phase(
                committed.task_id.as_ref(),
                vector_build_phase.clone(),
                committed.io_telemetry.index_publish_metadata(
                    &committed.source_id,
                    profile_id,
                    generation,
                    staged_index_stats,
                ),
            );
        }
        #[cfg(feature = "qdrant")]
        if self.qdrant.is_some() {
            for committed in committed_sources {
                let qdrant_phase = PhaseTiming::start(IngestTaskStage::QdrantSync.as_str());
                self.record_task_progress(
                    committed.task_id.as_ref(),
                    qdrant_phase
                        .progress_snapshot()
                        .with_counter("sources", 0, Some(1))
                        .with_recent_status("syncing qdrant")
                        .with_active_worker_kind("ingest"),
                );
                self.sync_qdrant_source(&committed.source_id).await;
                self.record_task_phase(
                    committed.task_id.as_ref(),
                    qdrant_phase,
                    serde_json::json!({
                        "operation": "qdrant_source_sync",
                        "embedding_profile_id": profile_id.as_str(),
                        "source_id": committed.source_id.0.as_str(),
                    }),
                );
            }
        }
        for committed in committed_sources {
            if let Err(cleanup_error) = cleanup_stale_source_image_artifacts(
                &self.data_dir,
                &committed.source_id,
                &committed.retained_image_artifacts,
            )
            .with_context(|| {
                format!(
                    "cleanup stale image artifacts after committed source ingest: {}",
                    committed.source_id.0
                )
            }) {
                tracing::warn!(
                    source = %committed.source_id.0,
                    error = %cleanup_error,
                    "stale image artifact cleanup failed after successful batched index publish; \
                     source vectors and index generation remain valid",
                );
            }
            tracing::info!(source = %committed.source_id.0, "ingest complete");
        }
        Ok(generation)
    }

    async fn embed_and_commit_prepared_sources(
        &mut self,
        profile_id: &EmbeddingProfileId,
        prepared_sources: Vec<PreparedSourceIngest>,
        cancellation_scope: BatchCancellationScope,
    ) -> Vec<PreparedSourceOutcome> {
        if prepared_sources.is_empty() {
            return Vec::new();
        }
        if !self.embedding_enabled {
            return self
                .commit_prepared_sources_without_embeddings(
                    profile_id,
                    prepared_sources,
                    cancellation_scope,
                )
                .await;
        }
        let total_source_count = prepared_sources.len();
        let mut outcome_slots = std::iter::repeat_with(|| None)
            .take(total_source_count)
            .collect::<Vec<_>>();
        let mut early_completed_sources = 0;
        let mut stopped_after_boundary_error: Option<String> = None;
        let mut sources_to_embed = Vec::with_capacity(total_source_count);
        for (index, source) in prepared_sources.into_iter().enumerate() {
            if let Some(error) = &stopped_after_boundary_error {
                outcome_slots[index] = Some(PreparedSourceOutcome {
                    source_id: source.source.id,
                    task_id: source.task_id,
                    result: Err(format!(
                        "source ingest not attempted after earlier source boundary failure: {error}"
                    )),
                });
                continue;
            }
            match self.prepared_source_has_fresh_committed_vectors(profile_id, &source.source) {
                Ok(true) => {
                    let child_chunk_count = source.child_chunks.len();
                    let cache_stats = Self::noop_embedding_cache_stats(child_chunk_count);
                    let (completed_sources, total_sources) =
                        cancellation_scope.progress(early_completed_sources);
                    match self.ensure_task_not_cancelled(
                        source.task_id.as_ref(),
                        completed_sources,
                        total_sources,
                        Some(&source.source.id),
                    ) {
                        Ok(()) => {
                            self.record_noop_source_ingest(
                                source.task_id.as_ref(),
                                profile_id,
                                &source.source.id,
                                child_chunk_count,
                                &cache_stats,
                            );
                            early_completed_sources += 1;
                            outcome_slots[index] = Some(PreparedSourceOutcome {
                                source_id: source.source.id,
                                task_id: source.task_id,
                                result: Ok(cache_stats),
                            });
                        }
                        Err(error) => {
                            let error = error.to_string();
                            stopped_after_boundary_error.get_or_insert_with(|| error.clone());
                            outcome_slots[index] = Some(PreparedSourceOutcome {
                                source_id: source.source.id,
                                task_id: source.task_id,
                                result: Err(error),
                            });
                        }
                    }
                }
                Ok(false) => sources_to_embed.push((index, source)),
                Err(error) => {
                    outcome_slots[index] = Some(PreparedSourceOutcome {
                        source_id: source.source.id,
                        task_id: source.task_id,
                        result: Err(error.to_string()),
                    });
                }
            }
        }
        if sources_to_embed.is_empty() {
            return collect_prepared_source_outcomes(outcome_slots);
        }
        let (remaining_indices, prepared_sources): (Vec<_>, Vec<_>) =
            sources_to_embed.into_iter().unzip();
        let source_count = prepared_sources.len();
        let input_count = prepared_sources
            .iter()
            .map(|source| source.child_chunks.len())
            .sum::<usize>();
        let child_chunks = prepared_sources
            .iter()
            .flat_map(|source| source.child_chunks.iter().cloned())
            .collect::<Vec<_>>();
        let source_progress = prepared_sources
            .iter()
            .filter_map(|source| {
                source.task_id.as_ref().map(|task_id| {
                    (
                        source.source.id.clone(),
                        (
                            task_id.clone(),
                            source.embedding_phase.clone(),
                            source.child_chunks.len(),
                        ),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let embedding_started = Instant::now();
        let prepared_vectors = match self
            .prepare_vectors_for_chunks_partial(
                profile_id,
                &child_chunks,
                self.embedding_execution_controls(),
                |batches, request_count, max_vectors_per_request| {
                    self.record_embedding_throughput_wait(
                        &source_progress,
                        batches,
                        request_count,
                        max_vectors_per_request,
                    );
                },
                |batches, request_count, max_vectors_per_request| {
                    self.record_embedding_request_started(
                        &source_progress,
                        batches,
                        request_count,
                        max_vectors_per_request,
                    );
                },
                |source_ids, request_count, input_count| {
                    self.record_embedding_postprocess_started(
                        &source_progress,
                        source_ids,
                        request_count,
                        input_count,
                    );
                },
            )
            .await
        {
            Ok(prepared_vectors) => prepared_vectors,
            Err(error) => {
                let error = error.to_string();
                for (index, source) in remaining_indices.into_iter().zip(prepared_sources) {
                    outcome_slots[index] = Some(PreparedSourceOutcome {
                        source_id: source.source.id,
                        task_id: source.task_id,
                        result: Err(error.clone()),
                    });
                }
                return collect_prepared_source_outcomes(outcome_slots);
            }
        };
        let duration_ms = embedding_started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let metrics = EmbeddingBatchMetrics {
            request_count: prepared_vectors.request_count,
            source_count,
            input_count,
            max_vectors_per_request: prepared_vectors.max_vectors_per_request,
            max_concurrent_requests: self.embedding_max_concurrent_requests,
            duration_ms,
        };
        self.record_finished_embedding_batch_stage(
            &source_progress,
            &prepared_vectors.request_source_ids,
            prepared_vectors.request_timing.as_ref(),
        );
        self.record_finished_embedding_batch_stage(
            &source_progress,
            &prepared_vectors.request_source_ids,
            prepared_vectors.postprocess_timing.as_ref(),
        );
        let mut vectors_by_source = HashMap::<SourceId, Vec<VectorDocument>>::new();
        for vector in prepared_vectors.vectors {
            vectors_by_source
                .entry(vector.source_id.clone())
                .or_default()
                .push(vector);
        }
        let mut stats_by_source = prepared_vectors.cache_stats_by_source;
        let batched_index_publish = source_count > 1;
        let mut outcomes = Vec::with_capacity(source_count);
        let mut batched_committed_sources = Vec::<BatchedCommittedSource>::new();
        let mut stopped_after_commit_error: Option<String> = None;
        let mut committed_sources_in_batch = early_completed_sources;
        for source in prepared_sources {
            let PreparedSourceIngest {
                task_id,
                source,
                evidence,
                chunks,
                links,
                evidence_spans,
                image_artifacts,
                graph_nodes,
                graph_edges,
                child_chunks,
                embedding_phase: _,
            } = source;
            let source_id = source.id.clone();
            let child_count = child_chunks.len();
            let source_vectors = vectors_by_source.remove(&source_id).unwrap_or_default();
            let source_cache_stats = stats_by_source.remove(&source_id).unwrap_or_default();
            let embedding_error = prepared_vectors.errors_by_source.get(&source_id).cloned();
            self.record_embedding_complete(
                task_id.as_ref(),
                child_count,
                source_vectors.len(),
                &source_cache_stats,
                &metrics,
            );
            let committable_source = PreparedSourceIngest {
                task_id: task_id.clone(),
                source,
                evidence,
                chunks,
                links,
                evidence_spans,
                image_artifacts,
                graph_nodes,
                graph_edges,
                child_chunks,
                embedding_phase: PhaseTiming::start(IngestTaskStage::EmbeddingQueueWait.as_str()),
            };
            let result = if let Some(error) = &stopped_after_commit_error {
                Err(format!(
                    "source ingest not attempted after earlier batch commit failure: {error}"
                ))
            } else if let Some(error) = embedding_error {
                Err(error)
            } else if source_vectors.len() != child_count {
                Err(format!(
                    "vector count mismatch for source: expected {}, got {}",
                    child_count,
                    source_vectors.len()
                ))
            } else {
                let (completed_sources, total_sources) =
                    cancellation_scope.progress(committed_sources_in_batch);
                match self.ensure_task_not_cancelled(
                    task_id.as_ref(),
                    completed_sources,
                    total_sources,
                    Some(&source_id),
                ) {
                    Ok(()) => {
                        match self.prepared_source_is_noop_ingest(
                            profile_id,
                            &committable_source.source,
                            child_count,
                            &source_cache_stats,
                        ) {
                            Ok(true) => {
                                self.record_noop_source_ingest(
                                    task_id.as_ref(),
                                    profile_id,
                                    &source_id,
                                    child_count,
                                    &source_cache_stats,
                                );
                                Ok(source_cache_stats.clone())
                            }
                            Ok(false) if batched_index_publish => {
                                let batch_generation = batched_committed_sources
                                    .first()
                                    .map(|committed| committed.index_generation);
                                match self
                                    .commit_prepared_source_without_index_publish(
                                        profile_id,
                                        batch_generation,
                                        committable_source,
                                        source_vectors,
                                        source_cache_stats.clone(),
                                    )
                                    .await
                                {
                                    Ok(committed) => {
                                        let stats = committed.cache_stats.clone();
                                        batched_committed_sources.push(committed);
                                        Ok(stats)
                                    }
                                    Err(error) => Err(error.to_string()),
                                }
                            }
                            Ok(false) => self
                                .commit_prepared_source(
                                    profile_id,
                                    committable_source,
                                    source_vectors,
                                    source_cache_stats.clone(),
                                )
                                .await
                                .map_err(|error| error.to_string()),
                            Err(error) => Err(error.to_string()),
                        }
                    }
                    Err(error) => Err(error.to_string()),
                }
            };
            match &result {
                Ok(_) => {
                    committed_sources_in_batch += 1;
                    #[cfg(test)]
                    self.notify_source_committed_for_test(&source_id);
                }
                Err(error) if prepared_vectors.errors_by_source.contains_key(&source_id) => {
                    tracing::warn!(
                        source = %source_id.0,
                        error = %error,
                        "source ingest failed because one of its embedding requests failed"
                    );
                }
                Err(error) => {
                    stopped_after_commit_error.get_or_insert_with(|| error.clone());
                }
            }
            outcomes.push(PreparedSourceOutcome {
                source_id,
                task_id,
                result,
            });
        }
        if batched_index_publish && !batched_committed_sources.is_empty() {
            if let Err(error) = self
                .publish_batched_committed_source_indexes(profile_id, &batched_committed_sources)
                .await
            {
                let error =
                    format!("batched index publication failed after source commit: {error}");
                self.mark_batched_committed_sources_failed_after_publish_error(
                    profile_id,
                    &batched_committed_sources,
                    &error,
                );
                let committed_source_ids = batched_committed_sources
                    .iter()
                    .map(|committed| committed.source_id.clone())
                    .collect::<std::collections::HashSet<_>>();
                for outcome in &mut outcomes {
                    if committed_source_ids.contains(&outcome.source_id) && outcome.result.is_ok() {
                        outcome.result = Err(error.clone());
                    }
                }
            }
        }
        for (index, outcome) in remaining_indices.into_iter().zip(outcomes) {
            outcome_slots[index] = Some(outcome);
        }
        collect_prepared_source_outcomes(outcome_slots)
    }

    async fn commit_prepared_sources_without_embeddings(
        &mut self,
        profile_id: &EmbeddingProfileId,
        prepared_sources: Vec<PreparedSourceIngest>,
        cancellation_scope: BatchCancellationScope,
    ) -> Vec<PreparedSourceOutcome> {
        let source_count = prepared_sources.len();
        let input_count = prepared_sources
            .iter()
            .map(|source| source.child_chunks.len())
            .sum::<usize>();
        let metrics = EmbeddingBatchMetrics {
            request_count: 0,
            source_count,
            input_count,
            max_vectors_per_request: 0,
            max_concurrent_requests: self.embedding_max_concurrent_requests,
            duration_ms: 0,
        };
        let mut outcomes = Vec::with_capacity(source_count);
        let mut stopped_after_commit_error: Option<String> = None;
        let mut committed_sources_in_batch = 0;

        for source in prepared_sources {
            let child_count = source.child_chunks.len();
            let source_id = source.source.id.clone();
            let task_id = source.task_id.clone();
            let cache_stats = EmbeddingCacheStats::default();
            self.record_embedding_complete(
                task_id.as_ref(),
                child_count,
                0,
                &cache_stats,
                &metrics,
            );

            let result = if let Some(error) = &stopped_after_commit_error {
                Err(format!(
                    "source ingest not attempted after earlier batch commit failure: {error}"
                ))
            } else {
                let (completed_sources, total_sources) =
                    cancellation_scope.progress(committed_sources_in_batch);
                match self.ensure_task_not_cancelled(
                    task_id.as_ref(),
                    completed_sources,
                    total_sources,
                    Some(&source_id),
                ) {
                    Ok(()) => self
                        .commit_prepared_source(profile_id, source, Vec::new(), cache_stats.clone())
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                }
            };

            match &result {
                Ok(_) => {
                    committed_sources_in_batch += 1;
                    #[cfg(test)]
                    self.notify_source_committed_for_test(&source_id);
                }
                Err(error) => {
                    stopped_after_commit_error.get_or_insert_with(|| error.clone());
                }
            }
            outcomes.push(PreparedSourceOutcome {
                source_id,
                task_id,
                result,
            });
        }

        outcomes
    }

    fn record_embedding_complete(
        &self,
        task_id: Option<&TaskId>,
        child_chunk_count: usize,
        vector_count: usize,
        cache_stats: &EmbeddingCacheStats,
        metrics: &EmbeddingBatchMetrics,
    ) {
        let mut embedding_progress =
            TaskProgressSnapshot::phase(IngestTaskStage::EmbeddingPostprocess.as_str())
                .with_counter(
                    "embedding_vectors",
                    cache_stats.embedded_chunks as u64,
                    Some(child_chunk_count as u64),
                )
                .with_counter("embedding_vector_documents", vector_count as u64, None)
                .with_counter(
                    "embedding_cache_hits",
                    cache_stats.cache_hits as u64,
                    Some(child_chunk_count as u64),
                )
                .with_counter(
                    "embedding_cache_misses",
                    cache_stats.cache_misses as u64,
                    Some(child_chunk_count as u64),
                )
                .with_counter("reused_chunks", cache_stats.reused_chunks as u64, None)
                .with_counter("changed_chunks", cache_stats.changed_chunks as u64, None)
                .with_counter(
                    "embedding_batch_inputs_total",
                    metrics.input_count as u64,
                    None,
                )
                .with_counter(
                    "batches",
                    metrics.request_count as u64,
                    Some(metrics.request_count as u64),
                )
                .with_counter("embedding_batch_sources", metrics.source_count as u64, None)
                .with_counter(
                    "max_vectors_per_request",
                    metrics.max_vectors_per_request as u64,
                    None,
                )
                .with_counter(
                    "embedding_in_flight_limit",
                    metrics.max_concurrent_requests as u64,
                    None,
                )
                .with_recent_status("embedding complete");
        if metrics.request_count > 0 {
            embedding_progress.set_endpoint(TaskEndpointSummary {
                name: "embedding".into(),
                calls: metrics.request_count as u64,
                latest_latency_ms: Some(metrics.duration_ms),
                first_token_latency_ms: None,
                p50_latency_ms: Some(metrics.duration_ms),
                p95_latency_ms: Some(metrics.duration_ms),
                latest_error: None,
            });
        }
        self.record_task_progress(task_id, embedding_progress);
    }

    fn record_embedding_throughput_wait(
        &self,
        source_progress: &HashMap<SourceId, (TaskId, PhaseTiming, usize)>,
        batches: &[Vec<EmbeddingInput>],
        request_count: usize,
        max_vectors_per_request: usize,
    ) {
        if batches.is_empty() {
            return;
        }
        let mut inputs_by_source = HashMap::<SourceId, usize>::new();
        for input in batches.iter().flat_map(|batch| batch.iter()) {
            *inputs_by_source.entry(input.source_id.clone()).or_default() += 1;
        }
        let source_count = inputs_by_source.len();
        let input_count = inputs_by_source.values().sum::<usize>();
        for (source_id, input_count_for_source) in inputs_by_source {
            let Some((task_id, phase, child_chunk_count)) = source_progress.get(&source_id) else {
                continue;
            };
            let queue_timing = phase.clone().finish(serde_json::json!({
                "operation": "embedding_batch_queue_wait",
                "source_count": source_count,
                "input_count": input_count,
                "input_count_for_source": input_count_for_source,
                "requests": request_count,
                "max_vectors_per_request": max_vectors_per_request,
                "max_concurrent_requests": self.embedding_max_concurrent_requests,
            }));
            self.record_finished_task_phase(Some(task_id), queue_timing);
            self.record_task_progress(
                Some(task_id),
                phase
                    .progress_snapshot()
                    .with_counter(
                        "embedding_vectors",
                        child_chunk_count.saturating_sub(input_count_for_source) as u64,
                        Some(*child_chunk_count as u64),
                    )
                    .with_counter(
                        "embedding_cache_misses",
                        input_count_for_source as u64,
                        None,
                    )
                    .with_counter("embedding_batch_sources", source_count as u64, None)
                    .with_counter("embedding_batch_inputs", input_count as u64, None)
                    .with_counter("batches", 0, Some(request_count as u64))
                    .with_counter(
                        "max_vectors_per_request",
                        max_vectors_per_request as u64,
                        None,
                    )
                    .with_counter(
                        "embedding_in_flight_limit",
                        self.embedding_max_concurrent_requests as u64,
                        None,
                    )
                    .with_wait_reason("embedding_throughput")
                    .with_recent_status("waiting for embedding model throughput")
                    .with_active_worker_kind("ingest"),
            );
        }
    }

    fn record_embedding_request_started(
        &self,
        source_progress: &HashMap<SourceId, (TaskId, PhaseTiming, usize)>,
        batches: &[Vec<EmbeddingInput>],
        request_count: usize,
        max_vectors_per_request: usize,
    ) {
        self.record_embedding_batch_active_stage(
            source_progress,
            batches,
            IngestTaskStage::EmbeddingRequest,
            "calling embedding endpoint",
            request_count,
            max_vectors_per_request,
        );
    }

    fn record_embedding_postprocess_started(
        &self,
        source_progress: &HashMap<SourceId, (TaskId, PhaseTiming, usize)>,
        source_ids: &[SourceId],
        request_count: usize,
        input_count: usize,
    ) {
        for source_id in source_ids {
            let Some((task_id, _phase, child_chunk_count)) = source_progress.get(source_id) else {
                continue;
            };
            self.record_task_progress(
                Some(task_id),
                TaskProgressSnapshot::phase(IngestTaskStage::EmbeddingPostprocess.as_str())
                    .with_counter("embedding_vectors", 0, Some(*child_chunk_count as u64))
                    .with_counter("embedding_batch_inputs", input_count as u64, None)
                    .with_counter("batches", request_count as u64, Some(request_count as u64))
                    .with_recent_status("processing embedding response")
                    .with_active_worker_kind("ingest"),
            );
        }
    }

    fn record_embedding_batch_active_stage(
        &self,
        source_progress: &HashMap<SourceId, (TaskId, PhaseTiming, usize)>,
        batches: &[Vec<EmbeddingInput>],
        stage: IngestTaskStage,
        status: &'static str,
        request_count: usize,
        max_vectors_per_request: usize,
    ) {
        let mut inputs_by_source = HashMap::<SourceId, usize>::new();
        for input in batches.iter().flat_map(|batch| batch.iter()) {
            *inputs_by_source.entry(input.source_id.clone()).or_default() += 1;
        }
        let source_count = inputs_by_source.len();
        let input_count = inputs_by_source.values().sum::<usize>();
        for (source_id, input_count_for_source) in inputs_by_source {
            let Some((task_id, _phase, child_chunk_count)) = source_progress.get(&source_id) else {
                continue;
            };
            self.record_task_progress(
                Some(task_id),
                TaskProgressSnapshot::phase(stage.as_str())
                    .with_counter(
                        "embedding_vectors",
                        child_chunk_count.saturating_sub(input_count_for_source) as u64,
                        Some(*child_chunk_count as u64),
                    )
                    .with_counter(
                        "embedding_batch_inputs",
                        input_count_for_source as u64,
                        None,
                    )
                    .with_counter("embedding_batch_sources", source_count as u64, None)
                    .with_counter("embedding_batch_inputs_total", input_count as u64, None)
                    .with_counter("batches", 0, Some(request_count as u64))
                    .with_counter(
                        "max_vectors_per_request",
                        max_vectors_per_request as u64,
                        None,
                    )
                    .with_counter(
                        "embedding_in_flight_limit",
                        self.embedding_max_concurrent_requests as u64,
                        None,
                    )
                    .with_recent_status(status)
                    .with_active_worker_kind("ingest"),
            );
        }
    }

    fn record_finished_embedding_batch_stage(
        &self,
        source_progress: &HashMap<SourceId, (TaskId, PhaseTiming, usize)>,
        source_ids: &[SourceId],
        timing: Option<&FinishedPhaseTiming>,
    ) {
        let Some(timing) = timing else {
            return;
        };
        for source_id in source_ids {
            let Some((task_id, _phase, _child_chunk_count)) = source_progress.get(source_id) else {
                continue;
            };
            self.record_finished_task_phase(Some(task_id), timing.clone());
        }
    }

    async fn append_generated_graph(
        &self,
        source: &Source,
        chunks: &[Chunk],
        graph_nodes: &mut Vec<GraphNode>,
        graph_edges: &mut Vec<GraphEdge>,
    ) {
        if !self.graph_extraction_config.enabled {
            return;
        }

        let Some(extractor) = &self.graph_extractor else {
            tracing::warn!(
                source = %source.id.0,
                "llm graph extraction enabled but chat model is disabled; continuing without generated graph data"
            );
            return;
        };

        match extractor
            .extract(source, chunks, &self.graph_extraction_config)
            .await
        {
            Ok(outcome) => {
                tracing::info!(
                    source = %source.id.0,
                    selected_chunks = outcome.stats.selected_chunk_count,
                    entities = outcome.stats.entity_count,
                    relationships = outcome.stats.relationship_count,
                    claims = outcome.stats.claim_count,
                    dropped = outcome.stats.dropped_item_count,
                    attempts = outcome.stats.attempt_count,
                    response_truncated = outcome.stats.response_truncated,
                    "llm graph extraction complete"
                );
                graph_nodes.extend(outcome.nodes);
                graph_edges.extend(outcome.edges);
            }
            Err(err) => {
                tracing::warn!(
                    source = %source.id.0,
                    reason = %bounded_graph_extraction_error(&err, self.graph_extraction_config.max_error_chars),
                    "llm graph extraction failed; continuing with deterministic graph data"
                );
            }
        }
    }

    pub async fn ingest_all(&mut self, force: bool) -> Result<IndexingOutcome> {
        self.ingest_all_inner(force, None).await
    }

    pub async fn ingest_all_with_task(
        &mut self,
        force: bool,
        task_id: &TaskId,
    ) -> Result<IndexingOutcome> {
        self.ingest_all_inner(force, Some(task_id)).await
    }

    async fn ingest_all_inner(
        &mut self,
        force: bool,
        task_id: Option<&TaskId>,
    ) -> Result<IndexingOutcome> {
        self.refresh_embedding_profile_capabilities().await?;
        let skipped_missing_sources = self
            .remove_missing_sources_for_all_source_ingest(task_id)
            .await?
            .len();
        if !force {
            self.check_stale()?;
        }

        let sources = self.store.list_sources()?;
        let to_ingest: Vec<SourceId> = sources
            .into_iter()
            .filter(|s| force || s.status != SourceStatus::Indexed)
            .map(|s| s.id)
            .collect();

        let total = to_ingest.len();
        let mut outcome = IndexingOutcome {
            source_count: total,
            skipped_missing_sources,
            embedding_cache: EmbeddingCacheStats::default(),
        };
        let active_profile_id = self.active_profile_id.clone();
        let mut pending = PendingPreparedSources::default();
        let mut completed = 0;
        for (i, source_id) in to_ingest.iter().enumerate() {
            self.ensure_task_not_cancelled(task_id, completed, total, Some(source_id))?;
            tracing::info!(progress = format!("{}/{}", i + 1, total), source = %source_id.0);
            self.record_task_progress(
                task_id,
                TaskProgressSnapshot::phase(IngestTaskStage::Ingest.as_str())
                    .with_counter("sources", completed as u64, Some(total as u64))
                    .with_recent_status("ingesting source"),
            );
            let prepared_source = self.prepare_source_contents(source_id, task_id).await?;
            if self.should_flush_before_pending_push(
                &pending,
                prepared_source.prepared_artifact_bytes(),
            ) {
                let source_outcomes = self
                    .flush_pending_prepared_sources(
                        &active_profile_id,
                        &mut pending,
                        BatchCancellationScope::SharedTask {
                            completed_sources_before_batch: completed,
                            total_sources: total,
                        },
                    )
                    .await;
                self.apply_ingest_all_source_outcomes(
                    task_id,
                    total,
                    &mut completed,
                    &mut outcome,
                    source_outcomes,
                )?;
            }
            pending.push(prepared_source);
            if self.should_flush_pending_sources(&pending) {
                let source_outcomes = self
                    .flush_pending_prepared_sources(
                        &active_profile_id,
                        &mut pending,
                        BatchCancellationScope::SharedTask {
                            completed_sources_before_batch: completed,
                            total_sources: total,
                        },
                    )
                    .await;
                self.apply_ingest_all_source_outcomes(
                    task_id,
                    total,
                    &mut completed,
                    &mut outcome,
                    source_outcomes,
                )?;
            }
        }
        if !pending.is_empty() {
            let source_outcomes = self
                .flush_pending_prepared_sources(
                    &active_profile_id,
                    &mut pending,
                    BatchCancellationScope::SharedTask {
                        completed_sources_before_batch: completed,
                        total_sources: total,
                    },
                )
                .await;
            self.apply_ingest_all_source_outcomes(
                task_id,
                total,
                &mut completed,
                &mut outcome,
                source_outcomes,
            )?;
        }
        if force {
            #[cfg(feature = "qdrant")]
            self.sync_qdrant_all().await;
        }

        Ok(outcome)
    }

    fn should_flush_before_pending_push(
        &self,
        pending_sources: &PendingPreparedSources,
        next_artifact_bytes: usize,
    ) -> bool {
        pending_sources.should_flush_before_push(
            next_artifact_bytes,
            self.pending_prepared_artifact_bytes_limit(),
        )
    }

    fn should_flush_pending_sources(&self, pending_sources: &PendingPreparedSources) -> bool {
        pending_sources.should_flush_after_push(
            self.pending_embedding_limit(),
            self.pending_prepared_artifact_bytes_limit(),
        )
    }

    fn pending_embedding_limit(&self) -> usize {
        self.embedding_batch_size
            .saturating_mul(self.embedding_max_concurrent_requests)
            .max(1)
    }

    fn pending_prepared_artifact_bytes_limit(&self) -> usize {
        self.image_artifact_limits.max_total_bytes_per_source.max(1)
    }

    async fn flush_pending_prepared_sources(
        &mut self,
        profile_id: &EmbeddingProfileId,
        pending_sources: &mut PendingPreparedSources,
        cancellation_scope: BatchCancellationScope,
    ) -> Vec<PreparedSourceOutcome> {
        let prepared_sources = pending_sources.take();
        self.embed_and_commit_prepared_sources(profile_id, prepared_sources, cancellation_scope)
            .await
    }

    fn apply_ingest_all_source_outcomes(
        &self,
        task_id: Option<&TaskId>,
        total: usize,
        completed: &mut usize,
        outcome: &mut IndexingOutcome,
        source_outcomes: Vec<PreparedSourceOutcome>,
    ) -> Result<()> {
        for source_outcome in source_outcomes {
            match source_outcome.result {
                Ok(cache_stats) => {
                    *completed += 1;
                    self.ensure_task_not_cancelled(
                        task_id,
                        *completed,
                        total,
                        Some(&source_outcome.source_id),
                    )?;
                    outcome.embedding_cache.add(&cache_stats);
                    self.record_task_progress(
                        task_id,
                        TaskProgressSnapshot::phase(IngestTaskStage::Ingest.as_str())
                            .with_counter("sources", *completed as u64, Some(total as u64))
                            .with_counter(
                                "embedding_cache_hits",
                                outcome.embedding_cache.cache_hits as u64,
                                None,
                            )
                            .with_counter(
                                "embedding_cache_misses",
                                outcome.embedding_cache.cache_misses as u64,
                                None,
                            )
                            .with_recent_status("finished source"),
                    );
                }
                Err(error) => bail!(error),
            }
        }
        Ok(())
    }

    fn ensure_task_not_cancelled(
        &self,
        task_id: Option<&TaskId>,
        completed_sources: usize,
        total_sources: usize,
        source_id: Option<&SourceId>,
    ) -> Result<()> {
        let Some(task_id) = task_id else {
            return Ok(());
        };
        if self.store.task_status(task_id)? != Some(TaskStatus::Cancelled) {
            return Ok(());
        }
        let payload = serde_json::json!({
            "completed_sources": completed_sources,
            "total_sources": total_sources,
            "source_id": source_id.map(|source_id| source_id.0.as_str()),
        });
        self.record_task_event(
            Some(task_id),
            "cancelled",
            "ingest cancellation observed at source boundary",
            payload.clone(),
        );
        self.record_task_phase(
            Some(task_id),
            PhaseTiming::start(IngestTaskStage::IngestCancelled.as_str()),
            payload,
        );
        bail!("ingest task cancelled");
    }

    fn record_task_phase(
        &self,
        task_id: Option<&TaskId>,
        phase: PhaseTiming,
        metadata: serde_json::Value,
    ) {
        self.record_finished_task_phase(task_id, phase.finish(metadata));
    }

    fn record_finished_task_phase(
        &self,
        task_id: Option<&TaskId>,
        finished: crate::task::FinishedPhaseTiming,
    ) {
        let Some(task_id) = task_id else {
            return;
        };
        let Ok(sqlite_write_permit) =
            self.acquire_task_metadata_write_permit(task_id, "task phase timing")
        else {
            return;
        };
        self.record_finished_task_phase_with_writer_permit(task_id, finished, &sqlite_write_permit);
    }

    fn record_finished_task_phase_with_writer_permit(
        &self,
        task_id: &TaskId,
        finished: crate::task::FinishedPhaseTiming,
        _sqlite_write_permit: &ResourcePermit,
    ) {
        if let Err(err) = self.store.insert_task_span(
            task_id,
            &finished.phase,
            &finished.started_at,
            finished.duration_ms,
            &finished.metadata,
        ) {
            tracing::warn!(
                task_id = %task_id.0,
                phase = %finished.phase,
                error = %err,
                "failed to persist task phase timing"
            );
        }
    }

    fn record_task_progress(&self, task_id: Option<&TaskId>, progress: TaskProgressSnapshot) {
        let Some(task_id) = task_id else {
            return;
        };
        let Ok(sqlite_write_permit) =
            self.acquire_task_metadata_write_permit(task_id, "task progress")
        else {
            return;
        };
        self.record_task_progress_with_writer_permit(Some(task_id), progress, &sqlite_write_permit);
    }

    fn record_task_progress_with_writer_permit(
        &self,
        task_id: Option<&TaskId>,
        progress: TaskProgressSnapshot,
        _sqlite_write_permit: &ResourcePermit,
    ) {
        let Some(task_id) = task_id else {
            return;
        };
        if let Err(err) = self.store.update_task_progress(task_id, progress) {
            tracing::warn!(
                task_id = %task_id.0,
                error = %err,
                "failed to persist task progress"
            );
        }
    }

    fn record_task_event(
        &self,
        task_id: Option<&TaskId>,
        event_type: &str,
        message: &str,
        payload: serde_json::Value,
    ) {
        let Some(task_id) = task_id else {
            return;
        };
        let Ok(sqlite_write_permit) =
            self.acquire_task_metadata_write_permit(task_id, "task event")
        else {
            return;
        };
        self.record_task_event_with_writer_permit(
            task_id,
            event_type,
            message,
            payload,
            &sqlite_write_permit,
        );
    }

    fn record_task_event_with_writer_permit(
        &self,
        task_id: &TaskId,
        event_type: &str,
        message: &str,
        payload: serde_json::Value,
        _sqlite_write_permit: &ResourcePermit,
    ) {
        if let Err(err) = self
            .store
            .insert_task_event(task_id, event_type, message, &payload)
        {
            tracing::warn!(
                task_id = %task_id.0,
                event_type,
                error = %err,
                "failed to persist task event"
            );
        }
    }

    fn acquire_task_metadata_write_permit(
        &self,
        task_id: &TaskId,
        operation: &'static str,
    ) -> Result<ResourcePermit> {
        acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write").with_context(|| {
            format!(
                "acquire sqlite writer resource for {operation}: {}",
                task_id.0
            )
        })
    }

    fn record_resource_timing(&self, task_id: Option<&TaskId>, resource: TaskResourceProgress) {
        self.record_task_event(
            task_id,
            "resource",
            "resource timing",
            serde_json::json!({ "resource": resource }),
        );
    }

    pub async fn rebuild_indexes_from_store(&mut self) -> Result<()> {
        self.refresh_embedding_profile_capabilities().await?;
        let source_ids = self
            .store
            .list_sources()?
            .into_iter()
            .map(|source| source.id)
            .collect::<Vec<_>>();
        let child_chunks = self.store.list_child_chunks()?;
        let active_profile_id = self.active_profile_id.clone();
        tracing::info!(
            count = child_chunks.len(),
            embedding_profile_id = %active_profile_id,
            "rebuilding local indexes"
        );
        let prepared = if self.embedding_enabled {
            self.prepare_full_indexes_for_chunks(&active_profile_id, &child_chunks)
                .await?
        } else {
            self.prepare_indexes_from_vectors(Vec::new())?
        };
        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        if self.embedding_enabled {
            self.store
                .replace_all_vector_documents_for_profile(&active_profile_id, &prepared.vectors)?;
            self.mark_profile_sources_embedded(&active_profile_id, &source_ids, &prepared.vectors)?;
        } else {
            self.store
                .replace_all_vector_documents_for_profile(&active_profile_id, &[])?;
        }
        self.lexical_index().rebuild_from_store(&self.store)?;
        drop(sqlite_write_permit);
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        self.publish_prepared_indexes(&active_profile_id, prepared)?;
        drop(index_publish_permit);
        #[cfg(feature = "qdrant")]
        self.sync_qdrant_all().await;

        Ok(())
    }

    pub async fn build_embedding_profile(
        &mut self,
        profile_id: &EmbeddingProfileId,
        source_id: Option<&SourceId>,
    ) -> Result<IndexingOutcome> {
        if !self.embedding_enabled {
            bail!("embedding is disabled; enable [embedding] before building vectors");
        }
        self.refresh_embedding_profile_capabilities().await?;
        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        let profile_reset = self
            .store
            .ensure_embedding_profile(profile_id, self.embedding_profile_spec.as_store_config())?;
        drop(sqlite_write_permit);
        #[cfg(not(feature = "qdrant"))]
        let _ = profile_reset;
        let target_source_ids = match source_id {
            Some(source_id) => {
                self.store
                    .get_source(source_id)?
                    .with_context(|| format!("source not found: {}", source_id.0))?;
                vec![source_id.clone()]
            }
            None => self
                .store
                .list_sources()?
                .into_iter()
                .map(|source| source.id)
                .collect::<Vec<_>>(),
        };
        let child_chunks = match source_id {
            Some(source_id) => self
                .store
                .list_child_chunks()?
                .into_iter()
                .filter(|chunk| chunk.source_id == *source_id)
                .collect::<Vec<_>>(),
            None => self.store.list_child_chunks()?,
        };
        let prepared = match source_id {
            Some(source_id) => {
                self.prepare_source_indexes_for_profile(profile_id, source_id, &child_chunks)
                    .await?
            }
            None => {
                self.prepare_full_indexes_for_chunks(profile_id, &child_chunks)
                    .await?
            }
        };
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        let staged = self.stage_prepared_index_artifacts_for_residency(&prepared)?;
        drop(index_publish_permit);
        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        let generation = match source_id {
            Some(source_id) => self.store.replace_source_vector_documents_for_profile(
                profile_id,
                source_id,
                &prepared.vectors,
            )?,
            None => {
                self.store
                    .replace_all_vector_documents_for_profile(profile_id, &prepared.vectors)?;
                self.mark_profile_sources_embedded(
                    profile_id,
                    &target_source_ids,
                    &prepared.vectors,
                )?;
                self.store.index_generation_for_profile(profile_id)?
            }
        };
        drop(sqlite_write_permit);
        let cache_stats = prepared.cache_stats.clone();
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        self.publish_committed_indexes(profile_id, generation, staged, prepared)?;
        drop(index_publish_permit);
        if *profile_id == self.active_profile_id {
            let sqlite_write_permit =
                acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
            self.clear_vector_only_stale_status(source_id)?;
            drop(sqlite_write_permit);
        }
        #[cfg(feature = "qdrant")]
        {
            if profile_reset || source_id.is_none() {
                self.sync_qdrant_profile_all(profile_id).await;
            } else if let Some(source_id) = source_id {
                self.sync_qdrant_profile_source(profile_id, source_id).await;
            }
        }
        Ok(IndexingOutcome {
            source_count: target_source_ids.len(),
            skipped_missing_sources: 0,
            embedding_cache: cache_stats,
        })
    }

    fn clear_vector_only_stale_status(&self, source_id: Option<&SourceId>) -> Result<()> {
        let sources = match source_id {
            Some(source_id) => self
                .store
                .get_source(source_id)?
                .into_iter()
                .collect::<Vec<_>>(),
            None => self.store.list_sources()?,
        };
        for source in sources {
            if source.status != SourceStatus::Stale {
                continue;
            }
            if source.parser_used.is_none() {
                continue;
            }
            if !source.path.exists() {
                continue;
            }
            if file_hash(&source.path)? == source.hash {
                self.store
                    .update_source_status(&source.id, &SourceStatus::Indexed)?;
            }
        }
        Ok(())
    }

    fn reserve_vector_memory(
        &self,
        key: String,
        owner: &'static str,
        vector_count: usize,
    ) -> Result<Option<MemoryReservationGuard>> {
        let estimated_mb = estimate_vector_memory_mb(vector_count, self.embed_client.dimension());
        if estimated_mb == 0 {
            return Ok(None);
        }
        let guard = self
            .memory_budget
            .try_reserve(key, owner, estimated_mb)
            .with_context(|| format!("reserve memory budget for {owner}"))?;
        if guard.degraded() {
            tracing::warn!(
                owner = guard.owner(),
                estimated_mb = guard.estimated_mb(),
                "memory budget pressure will reduce ingest throughput without lowering retrieval quality"
            );
        }
        Ok(Some(guard))
    }

    async fn prepare_full_indexes_for_chunks(
        &self,
        profile_id: &EmbeddingProfileId,
        child_chunks: &[Chunk],
    ) -> Result<PreparedIndexes> {
        let memory_reservation = self.reserve_vector_memory(
            format!("ingest:full_index:{}", profile_id.as_str()),
            "ingest:full_index",
            child_chunks.len(),
        )?;
        let controls = if memory_reservation
            .as_ref()
            .is_some_and(MemoryReservationGuard::degraded)
        {
            self.memory_constrained_embedding_controls()
        } else {
            self.embedding_execution_controls()
        };
        let prepared = self
            .prepare_vectors_for_chunks_with_controls(profile_id, child_chunks, controls)
            .await?;
        let hnsw = match self.vector_residency {
            VectorIndexResidency::LowMemory => HnswIndex::new(),
            VectorIndexResidency::ResidentHnsw => hnsw_from_vectors(prepared.vectors.clone())?,
        };

        Ok(PreparedIndexes {
            hnsw,
            vectors: prepared.vectors,
            cache_stats: prepared.cache_stats,
            _memory_reservation: memory_reservation,
        })
    }

    async fn prepare_vectors_for_chunks(
        &self,
        profile_id: &EmbeddingProfileId,
        child_chunks: &[Chunk],
    ) -> Result<PreparedVectors> {
        self.prepare_vectors_for_chunks_with_controls(
            profile_id,
            child_chunks,
            self.embedding_execution_controls(),
        )
        .await
    }

    async fn prepare_vectors_for_chunks_with_controls(
        &self,
        profile_id: &EmbeddingProfileId,
        child_chunks: &[Chunk],
        controls: EmbeddingExecutionControls,
    ) -> Result<PreparedVectors> {
        let prepared = self
            .prepare_vectors_for_chunks_partial(
                profile_id,
                child_chunks,
                controls,
                |_, _, _| {},
                |_, _, _| {},
                |_, _, _| {},
            )
            .await?;
        if let Some((source_id, error)) = prepared.errors_by_source.iter().next() {
            tracing::warn!(
                source = %source_id.0,
                error = %error,
                "embedding failed for source"
            );
            bail!("embedding failed for source: {error}");
        }
        if prepared.vectors.len() != child_chunks.len() {
            bail!(
                "vector count mismatch: expected {}, got {}",
                child_chunks.len(),
                prepared.vectors.len()
            );
        }
        Ok(prepared)
    }

    fn embedding_execution_controls(&self) -> EmbeddingExecutionControls {
        EmbeddingExecutionControls {
            batch_size: self.embedding_batch_size,
            max_concurrent_requests: self.embedding_max_concurrent_requests,
        }
    }

    fn memory_constrained_embedding_controls(&self) -> EmbeddingExecutionControls {
        EmbeddingExecutionControls {
            batch_size: 1,
            max_concurrent_requests: 1,
        }
    }

    async fn prepare_vectors_for_chunks_partial<F, G, H>(
        &self,
        profile_id: &EmbeddingProfileId,
        child_chunks: &[Chunk],
        controls: EmbeddingExecutionControls,
        mut record_throughput_wait: F,
        mut record_request_started: G,
        mut record_postprocess_started: H,
    ) -> Result<PreparedVectors>
    where
        F: FnMut(&[Vec<EmbeddingInput>], usize, usize),
        G: FnMut(&[Vec<EmbeddingInput>], usize, usize),
        H: FnMut(&[SourceId], usize, usize),
    {
        if !self.embedding_enabled {
            bail!("embedding is disabled; enable [embedding] before building vectors");
        }
        let mut vectors = Vec::new();
        let mut cache_stats = EmbeddingCacheStats::default();
        let mut cache_stats_by_source = HashMap::<SourceId, EmbeddingCacheStats>::new();
        let mut errors_by_source = HashMap::<SourceId, String>::new();
        let mut request_source_ids = Vec::new();
        let mut request_timing = None;
        let mut postprocess_timing = None;
        let mut request_count = 0;
        let mut max_vectors_per_request = 0;
        if !child_chunks.is_empty() {
            let profile_config_hash = self.embedding_profile_spec.config_hash();
            let expected_dimension = self.embed_client.dimension();
            let mut misses = Vec::new();
            for chunk in child_chunks {
                let input = self.embedding_input(chunk, &profile_config_hash);
                match self.store.get_embedding_cache_vector(
                    profile_id,
                    &profile_config_hash,
                    &input.hash,
                )? {
                    Some(vector) if vector.len() == expected_dimension => {
                        let sqlite_write_permit =
                            acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
                        self.store.record_embedding_cache_hit(
                            profile_id,
                            &profile_config_hash,
                            &input.hash,
                        )?;
                        drop(sqlite_write_permit);
                        cache_stats.cache_hits += 1;
                        cache_stats.reused_chunks += 1;
                        let source_stats = cache_stats_by_source
                            .entry(input.source_id.clone())
                            .or_default();
                        source_stats.cache_hits += 1;
                        source_stats.reused_chunks += 1;
                        vectors.push(VectorDocument {
                            chunk_id: input.chunk_id,
                            source_id: input.source_id,
                            vector,
                        });
                    }
                    _ => {
                        cache_stats.cache_misses += 1;
                        cache_stats.changed_chunks += 1;
                        let source_stats = cache_stats_by_source
                            .entry(input.source_id.clone())
                            .or_default();
                        source_stats.cache_misses += 1;
                        source_stats.changed_chunks += 1;
                        misses.push(input);
                    }
                }
            }

            if !misses.is_empty() {
                let batches = embedding_input_batches(misses, controls.batch_size);
                request_count = batches.len();
                max_vectors_per_request = batches.iter().map(Vec::len).max().unwrap_or(0);
                let request_input_count = batches.iter().map(Vec::len).sum::<usize>();
                request_source_ids = embedding_batch_source_ids_from_batches(&batches);
                record_throughput_wait(&batches, request_count, max_vectors_per_request);
                record_request_started(&batches, request_count, max_vectors_per_request);
                let embed_client = &self.embed_client;
                let request_phase = PhaseTiming::start(IngestTaskStage::EmbeddingRequest.as_str());
                let batch_results = stream::iter(batches.into_iter().enumerate())
                    .map(|(batch_index, batch)| async move {
                        let texts = batch
                            .iter()
                            .map(|input| input.text.clone())
                            .collect::<Vec<_>>();
                        let embeddings = match embed_client.embed(&texts).await {
                            Ok(embeddings) => embeddings,
                            Err(error) => return Err((batch_index, batch, error)),
                        };
                        if embeddings.len() != batch.len() {
                            let expected = batch.len();
                            return Err((
                                batch_index,
                                batch,
                                anyhow::anyhow!(
                                    "embedding count mismatch: expected {}, got {}",
                                    expected,
                                    embeddings.len()
                                ),
                            ));
                        }
                        Ok((batch_index, batch, embeddings))
                    })
                    .buffered(controls.max_concurrent_requests.max(1))
                    .collect::<Vec<_>>()
                    .await;
                request_timing = Some(request_phase.finish(serde_json::json!({
                    "operation": "embedding_endpoint_requests",
                    "source_count": request_source_ids.len(),
                    "input_count": request_input_count,
                    "requests": request_count,
                    "max_vectors_per_request": max_vectors_per_request,
                    "max_concurrent_requests": controls.max_concurrent_requests.max(1),
                })));

                record_postprocess_started(&request_source_ids, request_count, request_input_count);
                let postprocess_phase =
                    PhaseTiming::start(IngestTaskStage::EmbeddingPostprocess.as_str());
                let mut embedded_vectors = Vec::new();
                for result in batch_results {
                    match result {
                        Ok((_batch_index, batch, embeddings)) => {
                            embedded_vectors.reserve(batch.len());
                            for (input, embedding) in batch.into_iter().zip(embeddings) {
                                cache_stats.embedded_chunks += 1;
                                cache_stats_by_source
                                    .entry(input.source_id.clone())
                                    .or_default()
                                    .embedded_chunks += 1;
                                embedded_vectors.push(PreparedEmbeddingVector {
                                    embedding_input_hash: input.hash,
                                    document: VectorDocument {
                                        chunk_id: input.chunk_id,
                                        source_id: input.source_id,
                                        vector: embedding,
                                    },
                                });
                            }
                        }
                        Err((_batch_index, batch, error)) => {
                            let error = error.to_string();
                            for source_id in embedding_batch_source_ids(&batch) {
                                errors_by_source
                                    .entry(source_id)
                                    .or_insert_with(|| error.clone());
                            }
                        }
                    }
                }
                let sqlite_write_permit =
                    acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
                let cache_entries = embedded_vectors
                    .iter()
                    .map(|entry| SourceEmbeddingCacheVector {
                        source_id: &entry.document.source_id,
                        embedding_input_hash: &entry.embedding_input_hash,
                        vector: &entry.document.vector,
                    })
                    .collect::<Vec<_>>();
                let cache_entry_count = cache_entries.len();
                self.store.upsert_embedding_cache_vectors_for_live_sources(
                    profile_id,
                    &profile_config_hash,
                    &cache_entries,
                )?;
                drop(sqlite_write_permit);
                drop(cache_entries);
                vectors.extend(embedded_vectors.into_iter().map(|entry| entry.document));
                postprocess_timing = Some(postprocess_phase.finish(serde_json::json!({
                    "operation": "embedding_response_postprocess",
                    "source_count": request_source_ids.len(),
                    "input_count": request_input_count,
                    "cache_entries": cache_entry_count,
                    "embedded_chunks": cache_stats.embedded_chunks,
                    "error_source_count": errors_by_source.len(),
                })));
            }
        }

        Ok(PreparedVectors {
            vectors,
            cache_stats,
            cache_stats_by_source,
            errors_by_source,
            request_source_ids,
            request_timing,
            postprocess_timing,
            request_count,
            max_vectors_per_request,
        })
    }

    async fn prepare_source_indexes_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
        source_child_chunks: &[Chunk],
    ) -> Result<PreparedIndexes> {
        let source_prepared = self
            .prepare_vectors_for_chunks(profile_id, source_child_chunks)
            .await?;
        self.prepare_source_indexes_from_vectors(
            profile_id,
            source_id,
            source_prepared.vectors,
            source_prepared.cache_stats,
        )
    }

    fn prepare_source_indexes_from_vectors(
        &self,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
        source_vectors: Vec<VectorDocument>,
        cache_stats: EmbeddingCacheStats,
    ) -> Result<PreparedIndexes> {
        let vector_count = match self.vector_residency {
            VectorIndexResidency::LowMemory => source_vectors.len(),
            VectorIndexResidency::ResidentHnsw => self
                .store
                .count_vector_documents_for_profile(profile_id, None)?
                .saturating_sub(
                    self.store
                        .count_vector_documents_for_profile(profile_id, Some(source_id))?,
                )
                .saturating_add(source_vectors.len()),
        };
        let memory_reservation = self.reserve_vector_memory(
            format!(
                "ingest:source_index:{}:{}",
                profile_id.as_str(),
                source_id.0
            ),
            "ingest:source_index",
            vector_count,
        )?;
        let hnsw = match self.vector_residency {
            VectorIndexResidency::LowMemory => HnswIndex::new(),
            VectorIndexResidency::ResidentHnsw => {
                let mut all_vectors = self
                    .store
                    .list_vector_documents_for_profile(profile_id)?
                    .into_iter()
                    .filter(|document| document.source_id != *source_id)
                    .collect::<Vec<_>>();
                all_vectors.extend(source_vectors.clone());
                hnsw_from_vectors(all_vectors)?
            }
        };
        Ok(PreparedIndexes {
            hnsw,
            vectors: source_vectors,
            cache_stats,
            _memory_reservation: memory_reservation,
        })
    }

    fn prepare_indexes_from_vectors(
        &self,
        vectors: Vec<VectorDocument>,
    ) -> Result<PreparedIndexes> {
        let hnsw = match self.vector_residency {
            VectorIndexResidency::LowMemory => HnswIndex::new(),
            VectorIndexResidency::ResidentHnsw => hnsw_from_vectors(vectors.clone())?,
        };
        Ok(PreparedIndexes {
            hnsw,
            vectors,
            cache_stats: EmbeddingCacheStats::default(),
            _memory_reservation: None,
        })
    }

    fn mark_profile_sources_embedded(
        &self,
        profile_id: &EmbeddingProfileId,
        source_ids: &[SourceId],
        vectors: &[VectorDocument],
    ) -> Result<()> {
        let mut counts: HashMap<SourceId, usize> = HashMap::new();
        for vector in vectors {
            *counts.entry(vector.source_id.clone()).or_default() += 1;
        }
        for source_id in source_ids {
            let count = counts.get(source_id).copied().unwrap_or(0);
            self.store.set_source_embedding_status(
                profile_id,
                source_id,
                SourceEmbeddingStatus::Embedded,
                count,
                None,
            )?;
        }
        Ok(())
    }

    fn publish_prepared_indexes(
        &mut self,
        profile_id: &EmbeddingProfileId,
        prepared: PreparedIndexes,
    ) -> Result<()> {
        let generation = self.store.index_generation_for_profile(profile_id)?;
        let staged = self.stage_prepared_index_artifacts_for_residency(&prepared)?;
        self.publish_committed_indexes(profile_id, generation, staged, prepared)
    }

    fn stage_prepared_index_artifacts_for_residency(
        &self,
        prepared: &PreparedIndexes,
    ) -> Result<Option<PathBuf>> {
        match self.vector_residency {
            VectorIndexResidency::LowMemory => Ok(None),
            VectorIndexResidency::ResidentHnsw => {
                stage_prepared_index_artifacts(&self.data_dir, prepared).map(Some)
            }
        }
    }

    fn publish_committed_indexes(
        &mut self,
        profile_id: &EmbeddingProfileId,
        generation: u64,
        staged: Option<PathBuf>,
        prepared: PreparedIndexes,
    ) -> Result<()> {
        let publish_result = match staged.as_deref() {
            Some(staged) => {
                publish_staged_index_artifacts(&self.data_dir, profile_id, generation, staged)
            }
            None => write_index_manifest(&self.data_dir, profile_id, generation),
        };
        match publish_result {
            Ok(()) => {}
            Err(err) => {
                self.invalidate_live_indexes()?;
                return Err(err);
            }
        };
        if self.vector_residency == VectorIndexResidency::LowMemory {
            self.hnsw.clear();
            self.loaded_profile_id = profile_id.clone();
        } else if self.loaded_profile_id == *profile_id || self.active_profile_id == *profile_id {
            self.hnsw = prepared.hnsw;
            self.loaded_profile_id = profile_id.clone();
        }
        if let Err(error) = apply_index_gc(&self.data_dir, &self.store, self.index_gc_policy) {
            tracing::warn!(
                error = %error,
                embedding_profile_id = %profile_id,
                generation,
                "index generation garbage collection failed after publish"
            );
        }
        Ok(())
    }

    fn invalidate_live_indexes(&mut self) -> Result<()> {
        self.hnsw = HnswIndex::new();
        Ok(())
    }

    fn embedding_text(&self, chunk: &Chunk) -> String {
        self.embed_client
            .prepare_document(&chunk_search_text(chunk), &chunk.heading_path.join(" > "))
    }

    fn assign_embedding_input_hashes(&self, chunks: &mut [Chunk], profile_config_hash: &str) {
        for chunk in chunks {
            if chunk.chunk_type != ChunkType::Child {
                continue;
            }
            let input = self.embedding_input(chunk, profile_config_hash);
            chunk.embedding_input_hash = Some(input.hash);
        }
    }

    fn embedding_input(&self, chunk: &Chunk, profile_config_hash: &str) -> EmbeddingInput {
        let text = self.embedding_text(chunk);
        let hash = embedding_input_hash(profile_config_hash, &text);
        EmbeddingInput {
            chunk_id: chunk.id.clone(),
            source_id: chunk.source_id.clone(),
            hash,
            text,
        }
    }

    async fn caption_prepared_image_artifacts(
        &self,
        source_id: &SourceId,
        prepared: &PreparedImageArtifacts,
    ) -> Result<()> {
        for (artifact, file) in prepared.artifacts.iter().zip(&prepared.files) {
            self.caption_prepared_image(source_id, artifact, file)
                .await?;
        }
        Ok(())
    }

    async fn ocr_scanned_pdf_pages(
        &self,
        source_id: &SourceId,
        pdf_path: &Path,
        scan: Option<&crate::types::PdfScanSummary>,
        start_position: u32,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<EvidenceUnit>> {
        let Some(scan) = scan else {
            return Ok(Vec::new());
        };
        if !scan.ocr_recommended {
            return Ok(Vec::new());
        }
        let pages = ocr_required_pages(scan);
        let Some(provider) = &self.ocr_provider else {
            self.record_task_event(
                task_id,
                "warning",
                "OCR disabled for image-only PDF pages",
                serde_json::json!({
                    "source_id": source_id.0,
                    "image_only_page_count": pages.len(),
                    "pages": pages.iter().map(|page| page.page).collect::<Vec<_>>(),
                }),
            );
            return Ok(Vec::new());
        };

        let profile = provider.profile();
        let mut evidence = Vec::new();
        for page in pages {
            let request = OcrPageRequest {
                source_id: source_id.clone(),
                pdf_path: pdf_path.to_path_buf(),
                page: page.page,
                page_label: page.page_label.clone(),
            };
            let output = provider
                .recognize_page(&request)
                .with_context(|| format!("OCR page {}", page.page))?;
            let mut page_evidence = ocr_evidence_from_output(
                source_id,
                &page,
                output,
                &profile,
                start_position + evidence.len() as u32,
            );
            evidence.append(&mut page_evidence);
        }
        self.record_task_event(
            task_id,
            "phase",
            "OCR completed for image-only PDF pages",
            serde_json::json!({
                "source_id": source_id.0,
                "ocr_evidence_count": evidence.len(),
                "ocr_profile_hash": profile.profile_hash(),
            }),
        );
        Ok(evidence)
    }

    async fn caption_prepared_image(
        &self,
        source_id: &SourceId,
        artifact: &ImageArtifact,
        file: &PreparedImageFile,
    ) -> Result<()> {
        let Some(model) = &self.vision_model else {
            if self
                .store
                .get_successful_image_caption(
                    &artifact.content_hash,
                    &self.vision_caption_model,
                    &self.vision_caption_prompt_hash,
                )?
                .is_some()
            {
                tracing::debug!(
                    image_id = %artifact.image_id.0,
                    image_hash = %artifact.content_hash,
                    "reusing successful image caption cache"
                );
                return Ok(());
            }
            let attempt =
                CaptionAttempt::skipped("vision caption provider is disabled or not configured");
            let sqlite_write_permit =
                acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
            self.store.upsert_image_caption_attempt_for_live_source(
                source_id,
                &artifact.content_hash,
                &self.vision_caption_model,
                VISION_CAPTION_PROMPT_VERSION,
                &self.vision_caption_prompt_hash,
                &attempt,
            )?;
            drop(sqlite_write_permit);
            return Ok(());
        };

        if self
            .store
            .get_successful_image_caption(
                &artifact.content_hash,
                &self.vision_caption_model,
                &self.vision_caption_prompt_hash,
            )?
            .is_some()
        {
            let sqlite_write_permit =
                acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
            self.store.record_image_caption_cache_hit(
                &artifact.content_hash,
                &self.vision_caption_model,
                &self.vision_caption_prompt_hash,
            )?;
            drop(sqlite_write_permit);
            return Ok(());
        }

        let attempt = request_image_caption(model.as_ref(), &file.bytes, &artifact.mime_type).await;

        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        self.store.upsert_image_caption_attempt_for_live_source(
            source_id,
            &artifact.content_hash,
            &self.vision_caption_model,
            VISION_CAPTION_PROMPT_VERSION,
            &self.vision_caption_prompt_hash,
            &attempt,
        )?;
        drop(sqlite_write_permit);

        if attempt.status != ImageCaptionStatus::Success {
            tracing::warn!(
                image_id = %artifact.image_id.0,
                image_hash = %artifact.content_hash,
                status = ?attempt.status,
                error = ?attempt.error_message,
                "image caption unavailable; continuing ingest"
            );
        }
        Ok(())
    }
}

fn configured_vision_model(config: &Config) -> (Option<Box<dyn VisionModel>>, String) {
    let model_name = if config.vision.model.trim().is_empty() {
        "vision-disabled".to_string()
    } else {
        config.vision.model.clone()
    };
    if !config.vision.enabled {
        return (None, model_name);
    }
    if config.vision.provider != "openai_compatible" {
        tracing::warn!(
            provider = %config.vision.provider,
            "unsupported vision provider; image captioning disabled"
        );
        return (None, model_name);
    }
    if config.vision.base_url.trim().is_empty() || config.vision.model.trim().is_empty() {
        tracing::warn!("vision config is incomplete; image captioning disabled");
        return (None, model_name);
    }
    (
        Some(Box::new(OpenAiCompatibleVisionModel::from_config(
            &config.vision,
        ))),
        model_name,
    )
}

fn load_vector_index_for_residency(
    residency: VectorIndexResidency,
    data_dir: &Path,
    store: &Store,
    profile_id: &EmbeddingProfileId,
) -> Result<HnswIndex> {
    match residency {
        VectorIndexResidency::LowMemory => Ok(HnswIndex::new()),
        VectorIndexResidency::ResidentHnsw => {
            load_published_vector_index(data_dir, store, profile_id)
        }
    }
}

fn load_published_vector_index(
    data_dir: &Path,
    store: &Store,
    profile_id: &EmbeddingProfileId,
) -> Result<HnswIndex> {
    let store_generation = store.index_generation_for_profile(profile_id)?;
    let manifest_generation =
        read_index_manifest(data_dir, profile_id)?.map(|manifest| manifest.generation);
    if manifest_generation == Some(store_generation) {
        let generation_dir = index_generation_dir(data_dir, profile_id, store_generation);
        let hnsw_path = generation_dir.join("vectors.hnsw");
        if hnsw_path.exists() {
            let mut hnsw = HnswIndex::new();
            match hnsw.load(&hnsw_path) {
                Ok(()) => {
                    let sqlite_points = store.list_vector_documents_for_profile(profile_id)?;
                    let point_sets_match = {
                        let published_by_chunk = hnsw
                            .points()
                            .iter()
                            .map(|point| (&point.chunk_id, point))
                            .collect::<HashMap<_, _>>();
                        published_by_chunk.len() == hnsw.len()
                            && hnsw.len() == sqlite_points.len()
                            && sqlite_points.iter().all(|point| {
                                published_by_chunk.get(&point.chunk_id).copied() == Some(point)
                            })
                    };
                    if point_sets_match {
                        return Ok(hnsw);
                    }
                    tracing::warn!(
                        path = %hnsw_path.display(),
                        published_point_count = hnsw.len(),
                        sqlite_point_count = sqlite_points.len(),
                        "published vector index point set differs from SQLite; rebuilding from SQLite"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        path = %hnsw_path.display(),
                        "published vector index unavailable; rebuilding from SQLite"
                    );
                }
            }
        }
    }

    let mut hnsw = HnswIndex::new();
    hnsw.rebuild_from_store_for_profile(store, profile_id)?;
    Ok(hnsw)
}

fn stage_prepared_index_artifacts(data_dir: &Path, prepared: &PreparedIndexes) -> Result<PathBuf> {
    let staging_dir = unique_staging_dir(data_dir);
    if staging_dir.exists() {
        remove_dir_if_exists(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)
        .with_context(|| format!("create index staging dir: {}", staging_dir.display()))?;

    prepared.hnsw.save(&staging_dir.join("vectors.hnsw"))?;

    Ok(staging_dir)
}

fn staged_index_artifact_stats(
    staging_dir: Option<&Path>,
    generation: u64,
) -> StagedIndexArtifactStats {
    let hnsw_bytes = staging_dir
        .and_then(|staging_dir| fs::metadata(staging_dir.join("vectors.hnsw")).ok())
        .map_or(0, |metadata| metadata.len());
    let manifest_bytes = match index_manifest_json_bytes(generation) {
        Ok(bytes) => usize_to_u64(bytes.len()),
        Err(_) => 0,
    };
    StagedIndexArtifactStats {
        file_count: if hnsw_bytes == 0 { 0 } else { 1 },
        hnsw_bytes,
        total_bytes: hnsw_bytes,
        manifest_bytes,
    }
}

fn publish_staged_index_artifacts(
    data_dir: &Path,
    profile_id: &EmbeddingProfileId,
    generation: u64,
    staging_dir: &Path,
) -> Result<()> {
    let generation_dir = index_generation_dir(data_dir, profile_id, generation);
    if generation_dir.exists() {
        remove_dir_if_exists(&generation_dir)?;
    }
    fs::create_dir_all(profile_index_root_dir(data_dir, profile_id)).with_context(|| {
        format!(
            "create profile index root: {}",
            profile_index_root_dir(data_dir, profile_id).display()
        )
    })?;
    fs::rename(staging_dir, &generation_dir).with_context(|| {
        format!(
            "publish staged index generation: {} -> {}",
            staging_dir.display(),
            generation_dir.display()
        )
    })?;

    write_index_manifest(data_dir, profile_id, generation)?;
    Ok(())
}

fn read_index_manifest(
    data_dir: &Path,
    profile_id: &EmbeddingProfileId,
) -> Result<Option<IndexManifest>> {
    let path = index_manifest_path(data_dir, profile_id);
    if !path.exists() {
        if *profile_id == EmbeddingProfileId::default_profile() {
            return read_legacy_index_manifest(data_dir);
        }
        return Ok(None);
    }
    let data =
        fs::read(&path).with_context(|| format!("read index manifest: {}", path.display()))?;
    serde_json::from_slice(&data)
        .with_context(|| format!("parse index manifest: {}", path.display()))
        .map(Some)
}

fn read_legacy_index_manifest(data_dir: &Path) -> Result<Option<IndexManifest>> {
    let path = legacy_index_manifest_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let data =
        fs::read(&path).with_context(|| format!("read index manifest: {}", path.display()))?;
    serde_json::from_slice(&data)
        .with_context(|| format!("parse index manifest: {}", path.display()))
        .map(Some)
}

fn write_index_manifest(
    data_dir: &Path,
    profile_id: &EmbeddingProfileId,
    generation: u64,
) -> Result<()> {
    let path = index_manifest_path(data_dir, profile_id);
    let tmp_path = path.with_extension("json.tmp");
    let data = index_manifest_json_bytes(generation)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create index manifest dir: {}", parent.display()))?;
    }
    fs::write(&tmp_path, data)
        .with_context(|| format!("write index manifest temp: {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &path).with_context(|| {
        format!(
            "publish index manifest: {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn index_manifest_json_bytes(generation: u64) -> Result<Vec<u8>> {
    serde_json::to_vec(&IndexManifest { generation }).map_err(Into::into)
}

fn hnsw_from_vectors(vectors: Vec<VectorDocument>) -> Result<HnswIndex> {
    let mut hnsw = HnswIndex::new();
    hnsw.replace_all(vectors);
    hnsw.build()?;
    Ok(hnsw)
}

fn remove_staged_index_artifacts(staged: &Option<PathBuf>) {
    if let Some(staged) = staged {
        let _ = remove_dir_if_exists(staged);
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove dir: {}", path.display())),
    }
}

fn index_manifest_path(data_dir: &Path, profile_id: &EmbeddingProfileId) -> PathBuf {
    profile_index_root_dir(data_dir, profile_id).join("index-manifest.json")
}

fn legacy_index_manifest_path(data_dir: &Path) -> PathBuf {
    data_dir.join("index-manifest.json")
}

fn index_root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes")
}

fn profile_index_root_dir(data_dir: &Path, profile_id: &EmbeddingProfileId) -> PathBuf {
    index_root_dir(data_dir)
        .join("profiles")
        .join(profile_id.as_str())
}

fn index_generation_dir(
    data_dir: &Path,
    profile_id: &EmbeddingProfileId,
    generation: u64,
) -> PathBuf {
    profile_index_root_dir(data_dir, profile_id).join(format!("gen-{generation}"))
}

fn unique_staging_dir(data_dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    index_root_dir(data_dir).join(format!("staging-{}-{nanos}", std::process::id()))
}

fn chunk_search_text(chunk: &Chunk) -> String {
    chunk
        .context_text
        .as_ref()
        .map(|ctx| format!("{ctx} {}", chunk.text))
        .unwrap_or_else(|| chunk.text.clone())
}

fn embedding_input_hash(profile_config_hash: &str, prepared_text: &str) -> String {
    hex_sha256(format!("{CHUNKER_VERSION}\0{profile_config_hash}\0{prepared_text}").as_bytes())
}

fn embedding_request_count(input_count: usize, batch_size: usize) -> u64 {
    if input_count == 0 {
        return 0;
    }
    input_count.div_ceil(batch_size.max(1)) as u64
}

fn estimate_vector_memory_mb(vector_count: usize, embedding_dimensions: usize) -> usize {
    if vector_count == 0 || embedding_dimensions == 0 {
        return 0;
    }
    let bytes = (vector_count as u128)
        .saturating_mul(embedding_dimensions as u128)
        .saturating_mul(std::mem::size_of::<f32>() as u128)
        .saturating_mul(2);
    let mib = 1024_u128 * 1024_u128;
    let mb = bytes.saturating_add(mib - 1).saturating_div(mib);
    usize::try_from(mb).unwrap_or(usize::MAX).max(1)
}

fn embedding_input_batches(
    inputs: Vec<EmbeddingInput>,
    batch_size: usize,
) -> Vec<Vec<EmbeddingInput>> {
    let batch_size = batch_size.max(1);
    let mut batches = Vec::with_capacity(inputs.len().div_ceil(batch_size));
    let mut current = Vec::with_capacity(batch_size);
    for input in inputs {
        current.push(input);
        if current.len() == batch_size {
            batches.push(current);
            current = Vec::with_capacity(batch_size);
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn embedding_batch_source_ids(inputs: &[EmbeddingInput]) -> Vec<SourceId> {
    let mut source_ids = Vec::new();
    for input in inputs {
        if !source_ids.contains(&input.source_id) {
            source_ids.push(input.source_id.clone());
        }
    }
    source_ids
}

fn embedding_batch_source_ids_from_batches(batches: &[Vec<EmbeddingInput>]) -> Vec<SourceId> {
    let mut source_ids = Vec::new();
    for input in batches.iter().flat_map(|batch| batch.iter()) {
        if !source_ids.contains(&input.source_id) {
            source_ids.push(input.source_id.clone());
        }
    }
    source_ids
}

fn public_source_ingest_outcome(outcome: PreparedSourceOutcome) -> SourceIngestOutcome {
    SourceIngestOutcome {
        source_id: outcome.source_id,
        task_id: outcome.task_id.unwrap_or_default(),
        result: outcome.result,
    }
}

async fn push_reported_prepared_source_outcomes<F, Fut>(
    outcomes: &mut Vec<SourceIngestOutcome>,
    prepared_outcomes: Vec<PreparedSourceOutcome>,
    report_outcome: &mut F,
) where
    F: FnMut(SourceIngestOutcome) -> Fut,
    Fut: Future<Output = ()>,
{
    for outcome in prepared_outcomes
        .into_iter()
        .map(public_source_ingest_outcome)
    {
        push_reported_source_outcome(outcomes, outcome, report_outcome).await;
    }
}

async fn push_reported_source_outcome<F, Fut>(
    outcomes: &mut Vec<SourceIngestOutcome>,
    outcome: SourceIngestOutcome,
    report_outcome: &mut F,
) where
    F: FnMut(SourceIngestOutcome) -> Fut,
    Fut: Future<Output = ()>,
{
    report_outcome(outcome.clone()).await;
    outcomes.push(outcome);
}

fn build_evidence_graph(
    source: &Source,
    evidence: &[EvidenceUnit],
    chunks: &[Chunk],
    links: &[(ChunkId, EvidenceId)],
    image_artifacts: &[ImageArtifact],
    image_text_proximities: &[ImageTextProximity],
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let mut graph = GraphBuildState::new(source);
    let evidence_by_id: HashMap<EvidenceId, &EvidenceUnit> = evidence
        .iter()
        .map(|unit| (unit.id.clone(), unit))
        .collect();

    for unit in evidence {
        graph.add_evidence_node(unit);
    }

    for (ordinal, chunk) in chunks.iter().enumerate() {
        graph.add_chunk_node(chunk, ordinal as u32, &evidence_by_id);
    }

    for artifact in image_artifacts {
        graph.add_image_artifact_node(artifact);
    }

    for (ordinal, (chunk_id, evidence_id)) in links.iter().enumerate() {
        graph.add_chunk_evidence_edge(chunk_id, evidence_id, ordinal as u32);
    }

    for chunk in chunks {
        graph.add_parent_chunk_edge(chunk);
    }

    for unit in evidence {
        graph.add_evidence_derivation_edge(unit);
    }

    for artifact in image_artifacts {
        graph.add_image_artifact_derivation_edge(artifact);
    }

    graph.add_evidence_adjacency_edges(evidence);
    graph.add_chunk_adjacency_edges(chunks);
    graph.add_page_adjacency_edges();
    graph.add_section_adjacency_edges();
    graph.add_image_near_text_edges(image_text_proximities, image_artifacts, evidence);
    graph.add_same_source_edges();

    (graph.nodes, graph.edges)
}

struct GraphBuildState {
    source_id: SourceId,
    source_node_id: GraphNodeId,
    nodes: Vec<GraphNode>,
    node_ids: HashSet<String>,
    edges: Vec<GraphEdge>,
    edge_ids: HashSet<String>,
    page_nodes: HashMap<u32, GraphNodeId>,
    section_nodes: HashMap<Vec<String>, GraphNodeId>,
    evidence_nodes: HashMap<EvidenceId, GraphNodeId>,
    chunk_nodes: HashMap<ChunkId, GraphNodeId>,
    image_nodes: HashMap<ImageId, GraphNodeId>,
}

impl GraphBuildState {
    fn new(source: &Source) -> Self {
        let source_id = source.id.clone();
        let source_node_id = GraphNodeId::new(&source_id, GraphNodeKind::Source, &source_id.0);
        let mut graph = Self {
            source_id: source_id.clone(),
            source_node_id: source_node_id.clone(),
            nodes: Vec::new(),
            node_ids: HashSet::new(),
            edges: Vec::new(),
            edge_ids: HashSet::new(),
            page_nodes: HashMap::new(),
            section_nodes: HashMap::new(),
            evidence_nodes: HashMap::new(),
            chunk_nodes: HashMap::new(),
            image_nodes: HashMap::new(),
        };
        graph.push_node(GraphNode {
            id: source_node_id,
            source_id,
            kind: GraphNodeKind::Source,
            external_id: source.id.0.clone(),
            label: source
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned),
            locator: None,
            ordinal: None,
            metadata: Some(serde_json::json!({
                "path": source.path.to_string_lossy(),
                "hash": &source.hash,
                "parser_used": &source.parser_used,
            })),
        });
        graph
    }

    fn add_evidence_node(&mut self, unit: &EvidenceUnit) {
        let node_id = self.push_node(GraphNode {
            id: GraphNodeId::new(&self.source_id, GraphNodeKind::EvidenceUnit, &unit.id.0),
            source_id: self.source_id.clone(),
            kind: GraphNodeKind::EvidenceUnit,
            external_id: unit.id.0.clone(),
            label: Some(format!(
                "{} evidence {}",
                evidence_kind_label(unit.kind),
                unit.position
            )),
            locator: Some(unit.locator.clone()),
            ordinal: Some(unit.position),
            metadata: Some(serde_json::json!({
                "kind": evidence_kind_label(unit.kind),
                "text_hash": &unit.text_hash,
                "heading_path": &unit.heading_path,
            })),
        });
        self.evidence_nodes.insert(unit.id.clone(), node_id.clone());
        let mut has_parent = false;

        if let Some(page) = locator_page(&unit.locator) {
            let page_node_id = self.ensure_page_node(page);
            self.push_parent_child_edges(&page_node_id, &node_id, Some(unit.position));
            self.push_edge(
                EdgeType::SamePage,
                &page_node_id,
                &node_id,
                Some(unit.position),
            );
            has_parent = true;
        }

        if let Some(section_node_id) =
            self.ensure_section_nodes(&unit.heading_path, Some(unit.position))
        {
            self.push_parent_child_edges(&section_node_id, &node_id, Some(unit.position));
            has_parent = true;
        }

        if !has_parent {
            let source_node_id = self.source_node_id.clone();
            self.push_parent_child_edges(&source_node_id, &node_id, Some(unit.position));
        }
    }

    fn add_chunk_node(
        &mut self,
        chunk: &Chunk,
        ordinal: u32,
        evidence_by_id: &HashMap<EvidenceId, &EvidenceUnit>,
    ) {
        let node_id = self.push_node(GraphNode {
            id: GraphNodeId::new(&self.source_id, GraphNodeKind::Chunk, &chunk.id.0),
            source_id: self.source_id.clone(),
            kind: GraphNodeKind::Chunk,
            external_id: chunk.id.0.clone(),
            label: Some(format!("{} chunk", chunk_type_label(&chunk.chunk_type))),
            locator: None,
            ordinal: Some(ordinal),
            metadata: Some(serde_json::json!({
                "chunk_type": chunk_type_label(&chunk.chunk_type),
                "token_count": chunk.token_count,
                "heading_path": &chunk.heading_path,
            })),
        });
        self.chunk_nodes.insert(chunk.id.clone(), node_id.clone());
        let mut has_parent = false;

        let mut pages = Vec::new();
        for evidence_id in &chunk.evidence_unit_ids {
            if let Some(page) = evidence_by_id
                .get(evidence_id)
                .and_then(|unit| locator_page(&unit.locator))
            {
                if !pages.contains(&page) {
                    pages.push(page);
                }
            }
        }
        pages.sort_unstable();
        for page in pages {
            let page_node_id = self.ensure_page_node(page);
            self.push_parent_child_edges(&page_node_id, &node_id, Some(ordinal));
            self.push_edge(EdgeType::SamePage, &page_node_id, &node_id, Some(ordinal));
            has_parent = true;
        }

        if let Some(section_node_id) = self.ensure_section_nodes(&chunk.heading_path, Some(ordinal))
        {
            self.push_parent_child_edges(&section_node_id, &node_id, Some(ordinal));
            self.push_edge(
                EdgeType::SectionContains,
                &section_node_id,
                &node_id,
                Some(ordinal),
            );
            has_parent = true;
        }

        if !has_parent {
            let source_node_id = self.source_node_id.clone();
            self.push_parent_child_edges(&source_node_id, &node_id, Some(ordinal));
        }
    }

    fn add_image_artifact_node(&mut self, artifact: &ImageArtifact) {
        let node_id = self.push_node(GraphNode {
            id: GraphNodeId::new(
                &self.source_id,
                GraphNodeKind::ImageArtifact,
                &artifact.image_id.0,
            ),
            source_id: self.source_id.clone(),
            kind: GraphNodeKind::ImageArtifact,
            external_id: artifact.image_id.0.clone(),
            label: Some(format!("PDF image {}", artifact.image_index)),
            locator: Some(SourceLocator::PdfImage {
                page: artifact.page,
                image_index: artifact.image_index,
                bbox: artifact.bbox.clone(),
            }),
            ordinal: Some(artifact.image_index),
            metadata: Some(serde_json::json!({
                "content_hash": &artifact.content_hash,
                "mime_type": &artifact.mime_type,
                "width": artifact.width,
                "height": artifact.height,
                "page": artifact.page,
                "image_index": artifact.image_index,
                "relative_path": artifact.relative_path.to_string_lossy(),
            })),
        });
        self.image_nodes
            .insert(artifact.image_id.clone(), node_id.clone());

        let page_node_id = self.ensure_page_node(artifact.page);
        self.push_parent_child_edges(&page_node_id, &node_id, Some(artifact.image_index));
        self.push_edge(
            EdgeType::PageContainsImage,
            &page_node_id,
            &node_id,
            Some(artifact.image_index),
        );
        self.push_edge(
            EdgeType::SamePage,
            &page_node_id,
            &node_id,
            Some(artifact.image_index),
        );
    }

    fn add_chunk_evidence_edge(
        &mut self,
        chunk_id: &ChunkId,
        evidence_id: &EvidenceId,
        ordinal: u32,
    ) {
        let Some(chunk_node_id) = self.chunk_nodes.get(chunk_id).cloned() else {
            return;
        };
        let Some(evidence_node_id) = self.evidence_nodes.get(evidence_id).cloned() else {
            return;
        };
        self.push_parent_child_edges(&chunk_node_id, &evidence_node_id, Some(ordinal));
    }

    fn add_parent_chunk_edge(&mut self, chunk: &Chunk) {
        let Some(parent_id) = &chunk.parent_chunk_id else {
            return;
        };
        let Some(parent_node_id) = self.chunk_nodes.get(parent_id).cloned() else {
            return;
        };
        let Some(child_node_id) = self.chunk_nodes.get(&chunk.id).cloned() else {
            return;
        };
        self.push_parent_child_edges(&parent_node_id, &child_node_id, None);
    }

    fn add_evidence_derivation_edge(&mut self, unit: &EvidenceUnit) {
        let Some(derived_from) = &unit.derived_from else {
            return;
        };
        let Some(from_node_id) = self.evidence_nodes.get(&unit.id).cloned() else {
            return;
        };
        let Some(to_node_id) = self.evidence_nodes.get(derived_from).cloned() else {
            return;
        };
        self.push_edge(EdgeType::DerivedFrom, &from_node_id, &to_node_id, None);
    }

    fn add_image_artifact_derivation_edge(&mut self, artifact: &ImageArtifact) {
        let Some(evidence_node_id) = self.evidence_nodes.get(&artifact.evidence_id).cloned() else {
            return;
        };
        let Some(image_node_id) = self.image_nodes.get(&artifact.image_id).cloned() else {
            return;
        };
        self.push_edge(
            EdgeType::DerivedFrom,
            &evidence_node_id,
            &image_node_id,
            None,
        );
    }

    fn ensure_page_node(&mut self, page: u32) -> GraphNodeId {
        if let Some(node_id) = self.page_nodes.get(&page) {
            return node_id.clone();
        }

        let external_id = format!("page:{page}");
        let node_id = self.push_node(GraphNode {
            id: GraphNodeId::new(&self.source_id, GraphNodeKind::Page, &external_id),
            source_id: self.source_id.clone(),
            kind: GraphNodeKind::Page,
            external_id,
            label: Some(format!("Page {page}")),
            locator: None,
            ordinal: Some(page),
            metadata: Some(serde_json::json!({ "page": page })),
        });
        self.page_nodes.insert(page, node_id.clone());
        let source_node_id = self.source_node_id.clone();
        self.push_parent_child_edges(&source_node_id, &node_id, Some(page));
        node_id
    }

    fn ensure_section_nodes(
        &mut self,
        heading_path: &[String],
        ordinal: Option<u32>,
    ) -> Option<GraphNodeId> {
        let mut parent_node_id = None;
        let mut current_path = Vec::new();

        for heading in heading_path {
            current_path.push(heading.clone());
            let node_id = if let Some(node_id) = self.section_nodes.get(&current_path) {
                node_id.clone()
            } else {
                let external_id = format!("section:{}", current_path.join(" > "));
                let node_id = self.push_node(GraphNode {
                    id: GraphNodeId::new(&self.source_id, GraphNodeKind::Section, &external_id),
                    source_id: self.source_id.clone(),
                    kind: GraphNodeKind::Section,
                    external_id,
                    label: Some(heading.clone()),
                    locator: None,
                    ordinal,
                    metadata: Some(serde_json::json!({ "heading_path": &current_path })),
                });
                self.section_nodes
                    .insert(current_path.clone(), node_id.clone());
                if let Some(parent) = &parent_node_id {
                    self.push_parent_child_edges(parent, &node_id, ordinal);
                } else {
                    let source_node_id = self.source_node_id.clone();
                    self.push_parent_child_edges(&source_node_id, &node_id, ordinal);
                }
                node_id
            };
            parent_node_id = Some(node_id);
        }

        parent_node_id
    }

    fn add_evidence_adjacency_edges(&mut self, evidence: &[EvidenceUnit]) {
        let mut ordered = evidence
            .iter()
            .filter_map(|unit| {
                self.evidence_nodes
                    .get(&unit.id)
                    .cloned()
                    .map(|node_id| (unit.position, node_id))
            })
            .collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| graph_node_id_key(&left.1).cmp(graph_node_id_key(&right.1)))
        });
        let node_ids = ordered
            .into_iter()
            .map(|(_, node_id)| node_id)
            .collect::<Vec<_>>();
        self.push_sequence_edges(&node_ids);
    }

    fn add_chunk_adjacency_edges(&mut self, chunks: &[Chunk]) {
        let mut parent_nodes = Vec::new();
        let mut child_nodes = Vec::new();

        for (ordinal, chunk) in chunks.iter().enumerate() {
            let Some(node_id) = self.chunk_nodes.get(&chunk.id).cloned() else {
                continue;
            };
            match chunk.chunk_type {
                ChunkType::Parent => parent_nodes.push((ordinal as u32, node_id)),
                ChunkType::Child => child_nodes.push((ordinal as u32, node_id)),
            }
        }

        parent_nodes.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| graph_node_id_key(&left.1).cmp(graph_node_id_key(&right.1)))
        });
        child_nodes.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| graph_node_id_key(&left.1).cmp(graph_node_id_key(&right.1)))
        });
        self.push_sequence_edges(
            &parent_nodes
                .into_iter()
                .map(|(_, node_id)| node_id)
                .collect::<Vec<_>>(),
        );
        self.push_sequence_edges(
            &child_nodes
                .into_iter()
                .map(|(_, node_id)| node_id)
                .collect::<Vec<_>>(),
        );
    }

    fn add_page_adjacency_edges(&mut self) {
        let mut pages = self
            .page_nodes
            .iter()
            .map(|(page, node_id)| (*page, node_id.clone()))
            .collect::<Vec<_>>();
        pages.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| graph_node_id_key(&left.1).cmp(graph_node_id_key(&right.1)))
        });
        let node_ids = pages
            .into_iter()
            .map(|(_, node_id)| node_id)
            .collect::<Vec<_>>();
        self.push_sequence_edges(&node_ids);
    }

    fn add_section_adjacency_edges(&mut self) {
        let mut sections = self
            .nodes
            .iter()
            .filter(|node| node.kind == GraphNodeKind::Section)
            .map(|node| {
                (
                    node.ordinal.unwrap_or(u32::MAX),
                    node.external_id.clone(),
                    node.id.clone(),
                )
            })
            .collect::<Vec<_>>();
        sections.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| graph_node_id_key(&left.2).cmp(graph_node_id_key(&right.2)))
        });
        let node_ids = sections
            .into_iter()
            .map(|(_, _, node_id)| node_id)
            .collect::<Vec<_>>();
        self.push_sequence_edges(&node_ids);
    }

    fn add_image_near_text_edges(
        &mut self,
        proximities: &[ImageTextProximity],
        image_artifacts: &[ImageArtifact],
        evidence: &[EvidenceUnit],
    ) {
        let image_pages = image_artifacts
            .iter()
            .map(|artifact| (artifact.image_id.clone(), artifact.page))
            .collect::<HashMap<_, _>>();

        for proximity in proximities {
            let Some(image_node_id) = self.image_nodes.get(&proximity.image_id).cloned() else {
                continue;
            };
            let Some(page) = image_pages.get(&proximity.image_id).copied() else {
                continue;
            };
            let mut linked_text_nodes = HashSet::new();

            for nearby in [
                exact_nearby_text(&proximity.nearby_text_before),
                exact_nearby_text(&proximity.nearby_text_after),
            ]
            .into_iter()
            .flatten()
            {
                for unit in evidence {
                    if unit.kind != EvidenceKind::Text
                        || locator_page(&unit.locator) != Some(page)
                        || !evidence_matches_nearby_text(unit, nearby)
                    {
                        continue;
                    }
                    let Some(text_node_id) = self.evidence_nodes.get(&unit.id).cloned() else {
                        continue;
                    };
                    if linked_text_nodes.insert(text_node_id.0.clone()) {
                        self.push_edge(
                            EdgeType::ImageNearText,
                            &image_node_id,
                            &text_node_id,
                            Some(unit.position),
                        );
                    }
                }
            }
        }
    }

    fn add_same_source_edges(&mut self) {
        let source_node_id = self.source_node_id.clone();
        let node_refs = self
            .nodes
            .iter()
            .filter(|node| node.id != source_node_id)
            .map(|node| (node.ordinal, node.id.clone()))
            .collect::<Vec<_>>();

        for (ordinal, node_id) in node_refs {
            self.push_edge(EdgeType::SameSource, &source_node_id, &node_id, ordinal);
        }
    }

    fn push_parent_child_edges(
        &mut self,
        parent_node_id: &GraphNodeId,
        child_node_id: &GraphNodeId,
        ordinal: Option<u32>,
    ) {
        self.push_edge(EdgeType::Child, parent_node_id, child_node_id, ordinal);
        self.push_edge(EdgeType::Parent, child_node_id, parent_node_id, ordinal);
    }

    fn push_sequence_edges(&mut self, node_ids: &[GraphNodeId]) {
        for (ordinal, pair) in node_ids.windows(2).enumerate() {
            let previous = &pair[0];
            let next = &pair[1];
            self.push_edge(EdgeType::Next, previous, next, Some(ordinal as u32));
            self.push_edge(EdgeType::Previous, next, previous, Some(ordinal as u32));
        }
    }

    fn push_node(&mut self, node: GraphNode) -> GraphNodeId {
        let node_id = node.id.clone();
        if self.node_ids.insert(node_id.0.clone()) {
            self.nodes.push(node);
        }
        node_id
    }

    fn push_edge(
        &mut self,
        edge_type: EdgeType,
        from_node_id: &GraphNodeId,
        to_node_id: &GraphNodeId,
        ordinal: Option<u32>,
    ) {
        let edge = GraphEdge {
            id: GraphEdgeId::new(
                &self.source_id,
                edge_type,
                from_node_id,
                to_node_id,
                ordinal,
            ),
            source_id: self.source_id.clone(),
            edge_type,
            from_node_id: from_node_id.clone(),
            to_node_id: to_node_id.clone(),
            ordinal,
            weight: None,
            metadata: None,
        };
        if self.edge_ids.insert(edge.id.0.clone()) {
            self.edges.push(edge);
        }
    }
}

fn locator_page(locator: &SourceLocator) -> Option<u32> {
    match locator {
        SourceLocator::Pdf { page, .. }
        | SourceLocator::PdfOcr { page, .. }
        | SourceLocator::PdfImage { page, .. } => Some(*page),
        SourceLocator::Document { .. }
        | SourceLocator::Markdown { .. }
        | SourceLocator::Canonical { .. } => None,
    }
}

fn graph_node_id_key(node_id: &GraphNodeId) -> &str {
    &node_id.0
}

fn exact_nearby_text(text: &Option<String>) -> Option<&str> {
    let text = text.as_deref()?.trim();
    if text.is_empty() || text.starts_with("same-page excerpt fallback:") {
        None
    } else {
        Some(text)
    }
}

fn evidence_matches_nearby_text(unit: &EvidenceUnit, nearby_text: &str) -> bool {
    let evidence_text = unit.text.trim();
    evidence_text == nearby_text || (nearby_text.len() >= 24 && evidence_text.contains(nearby_text))
}

fn evidence_kind_label(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Text => "text",
        EvidenceKind::Ocr => "ocr",
        EvidenceKind::Image => "image",
        EvidenceKind::Generated => "generated",
    }
}

fn chunk_type_label(chunk_type: &ChunkType) -> &'static str {
    match chunk_type {
        ChunkType::Child => "child",
        ChunkType::Parent => "parent",
    }
}

fn bounded_graph_extraction_error(error: &anyhow::Error, max_chars: usize) -> String {
    error.to_string().chars().take(max_chars.min(512)).collect()
}

fn extract_image_artifacts_for_ingest(
    parser: &dyn Parser,
    path: &Path,
    limits: ImageArtifactLimits,
) -> Result<Vec<ParsedImageArtifact>> {
    match parser.extract_image_artifacts_with_limits(path, limits) {
        Ok(artifacts) => Ok(artifacts),
        Err(err) => {
            if let Some(limit) = err
                .downcast_ref::<ImageArtifactLimitError>()
                .filter(|limit| limit.is_unsupported_extraction())
            {
                warn_unsupported_image_extraction(limit);
                Ok(Vec::new())
            } else {
                Err(err)
            }
        }
    }
}

fn warn_unsupported_image_extraction(limit: &ImageArtifactLimitError) {
    if let ImageArtifactLimitError::UnsupportedImageExtraction {
        stage,
        backend,
        reason,
        page,
        image_index,
    } = limit
    {
        tracing::warn!(
            stage = %stage,
            backend = *backend,
            reason = *reason,
            page = *page,
            image_index = *image_index,
            "skipping unsupported PDF image artifact during ingest"
        );
    }
}

fn prepare_image_artifacts(
    data_dir: &Path,
    source_id: &SourceId,
    start_position: u32,
    parsed: Vec<ParsedImageArtifact>,
    limits: ImageArtifactLimits,
) -> Result<PreparedImageArtifacts> {
    let mut evidence = Vec::new();
    let mut artifacts = Vec::new();
    let mut files = Vec::new();
    let mut text_proximities = Vec::new();
    let mut budget = ImageArtifactBudget::new(limits, ImageArtifactLimitStage::Prepare);

    for parsed_artifact in parsed {
        budget.reserve_image_slot(parsed_artifact.page, parsed_artifact.image_index)?;
        budget.validate_dimensions(
            parsed_artifact.page,
            parsed_artifact.image_index,
            parsed_artifact.width,
            parsed_artifact.height,
        )?;
        budget.accept_image_bytes(
            parsed_artifact.page,
            parsed_artifact.image_index,
            parsed_artifact.bytes.len(),
        )?;
        let content_hash = parsed_artifact.content_hash();
        let id_hash = if parsed_artifact.bbox.is_some() {
            content_hash.clone()
        } else {
            // Without a bbox, repeated placements of the same image on one page
            // would otherwise collide. The stored content_hash remains the real
            // image hash; the ID input only disambiguates the placement fallback.
            format!(
                "{}:image-index:{}",
                content_hash, parsed_artifact.image_index
            )
        };
        let image_id = ImageId::for_pdf_image(
            source_id,
            parsed_artifact.page,
            parsed_artifact.bbox.as_ref(),
            &id_hash,
        );
        let text_proximity = ImageTextProximity {
            image_id: image_id.clone(),
            nearby_text_before: parsed_artifact.nearby_text_before.clone(),
            nearby_text_after: parsed_artifact.nearby_text_after.clone(),
        };
        let evidence_id = EvidenceId(image_id.0.clone());
        let relative_path =
            image_artifact_relative_path(source_id, &image_id, &parsed_artifact.extension)?;
        let absolute_path = checked_image_artifact_absolute_path(data_dir, &relative_path)?;
        let locator = SourceLocator::PdfImage {
            page: parsed_artifact.page,
            image_index: parsed_artifact.image_index,
            bbox: parsed_artifact.bbox.clone(),
        };
        let text = image_evidence_text(&parsed_artifact, &relative_path, &locator);

        evidence.push(EvidenceUnit {
            id: evidence_id.clone(),
            source_id: source_id.clone(),
            kind: EvidenceKind::Image,
            derived_from: None,
            locator,
            text_hash: hex_sha256(text.as_bytes()),
            text,
            heading_path: Vec::new(),
            language: None,
            position: start_position + evidence.len() as u32,
        });
        artifacts.push(ImageArtifact {
            image_id,
            source_id: source_id.clone(),
            evidence_id,
            relative_path: relative_path.clone(),
            content_hash: content_hash.clone(),
            mime_type: parsed_artifact.mime_type,
            width: parsed_artifact.width,
            height: parsed_artifact.height,
            page: parsed_artifact.page,
            image_index: parsed_artifact.image_index,
            bbox: parsed_artifact.bbox,
        });
        files.push(PreparedImageFile {
            absolute_path,
            bytes: parsed_artifact.bytes,
            content_hash,
            page: parsed_artifact.page,
            image_index: parsed_artifact.image_index,
        });
        text_proximities.push(text_proximity);
    }

    Ok(PreparedImageArtifacts {
        evidence,
        artifacts,
        files,
        text_proximities,
    })
}

fn image_evidence_text(
    artifact: &ParsedImageArtifact,
    relative_path: &Path,
    locator: &SourceLocator,
) -> String {
    let mut text = format!(
        "Image evidence at {locator}. Artifact path: {}. Mime type: {}. Dimensions: {}x{} px.",
        relative_path.display(),
        artifact.mime_type,
        artifact.width,
        artifact.height
    );
    if let Some(before) = &artifact.nearby_text_before {
        if before.starts_with("same-page excerpt fallback:") {
            text.push_str(" Parser limitation: exact nearby text before/after was unavailable; ");
            text.push_str(before);
            text.push('.');
        } else {
            text.push_str(" Nearby text before image: ");
            text.push_str(before);
            text.push('.');
        }
    }
    if let Some(after) = &artifact.nearby_text_after {
        text.push_str(" Nearby text after image: ");
        text.push_str(after);
        text.push('.');
    }
    text
}

fn write_image_artifact_files(
    files: &[PreparedImageFile],
    limits: ImageArtifactLimits,
) -> Result<Vec<WrittenImageFile>> {
    validate_image_artifact_write_budget(files, limits)?;
    let mut written = Vec::new();
    for file in files {
        let preexisting = if file.absolute_path.exists() {
            file_hash(&file.absolute_path)? == file.content_hash
        } else {
            false
        };
        if !preexisting {
            if let Some(parent) = file.absolute_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create image artifact dir: {}", parent.display()))?;
            }
            let tmp_path = image_artifact_tmp_path(&file.absolute_path);
            fs::write(&tmp_path, &file.bytes)
                .with_context(|| format!("write image artifact temp: {}", tmp_path.display()))?;
            fs::rename(&tmp_path, &file.absolute_path).with_context(|| {
                format!(
                    "publish image artifact: {} -> {}",
                    tmp_path.display(),
                    file.absolute_path.display()
                )
            })?;
        }
        written.push(WrittenImageFile {
            absolute_path: file.absolute_path.clone(),
            preexisting,
        });
    }
    Ok(written)
}

fn validate_image_artifact_write_budget(
    files: &[PreparedImageFile],
    limits: ImageArtifactLimits,
) -> Result<()> {
    let mut budget = ImageArtifactBudget::new(limits, ImageArtifactLimitStage::Write);
    for (idx, file) in files.iter().enumerate() {
        let image_index = if file.image_index == 0 {
            idx as u32 + 1
        } else {
            file.image_index
        };
        budget.reserve_image_slot(file.page, image_index)?;
        budget.accept_image_bytes(file.page, image_index, file.bytes.len())?;
    }
    Ok(())
}

fn cleanup_written_image_files(files: &[WrittenImageFile]) {
    for file in files {
        if !file.preexisting {
            let _ = fs::remove_file(&file.absolute_path);
        }
    }
}

fn cleanup_stale_source_image_artifacts(
    data_dir: &Path,
    source_id: &SourceId,
    artifacts: &[ImageArtifact],
) -> Result<()> {
    let source_dir = source_image_artifact_dir(data_dir, source_id)?;
    if !source_dir.exists() {
        return Ok(());
    }
    let keep: HashSet<PathBuf> = artifacts
        .iter()
        .map(|artifact| checked_image_artifact_absolute_path(data_dir, &artifact.relative_path))
        .collect::<Result<_>>()?;
    for entry in fs::read_dir(&source_dir)
        .with_context(|| format!("read image artifact dir: {}", source_dir.display()))?
    {
        let path = entry?.path();
        if path.is_file() && !keep.contains(&path) {
            fs::remove_file(&path)
                .with_context(|| format!("remove stale image artifact: {}", path.display()))?;
        }
    }
    if artifacts.is_empty() || fs::read_dir(&source_dir)?.next().is_none() {
        remove_dir_if_exists(&source_dir)?;
    }
    Ok(())
}

fn remove_source_image_artifacts(data_dir: &Path, source_id: &SourceId) -> Result<()> {
    let source_dir = source_image_artifact_dir(data_dir, source_id)?;
    remove_dir_if_exists(&source_dir)
}

fn image_artifact_relative_path(
    source_id: &SourceId,
    image_id: &ImageId,
    extension: &str,
) -> Result<PathBuf> {
    let relative_path = PathBuf::from(IMAGE_ARTIFACTS_DIR)
        .join(source_image_artifact_component(source_id))
        .join(format!(
            "{}.{}",
            sanitize_path_component(&image_id.0, "image"),
            sanitize_extension(extension)
        ));
    ensure_relative_image_artifact_path(&relative_path)?;
    Ok(relative_path)
}

fn source_image_artifact_dir(data_dir: &Path, source_id: &SourceId) -> Result<PathBuf> {
    let root = image_artifacts_root_dir(data_dir);
    let source_dir = root.join(source_image_artifact_component(source_id));
    ensure_source_image_artifact_dir(data_dir, &root, &source_dir, source_id)?;
    Ok(source_dir)
}

fn image_artifacts_root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(IMAGE_ARTIFACTS_DIR)
}

fn source_image_artifact_component(source_id: &SourceId) -> String {
    sanitize_path_component(&source_id.0, "source")
}

fn checked_image_artifact_absolute_path(data_dir: &Path, relative_path: &Path) -> Result<PathBuf> {
    ensure_relative_image_artifact_path(relative_path)?;
    let root = image_artifacts_root_dir(data_dir);
    let absolute_path = data_dir.join(relative_path);
    let parent = absolute_path.parent().with_context(|| {
        format!(
            "image artifact path has no parent: {}",
            absolute_path.display()
        )
    })?;
    if absolute_path == data_dir
        || absolute_path == root
        || !absolute_path.starts_with(&root)
        || parent == root
        || !parent.starts_with(&root)
    {
        bail!(
            "unsafe image artifact path outside artifact root: {}",
            absolute_path.display()
        );
    }
    Ok(absolute_path)
}

fn ensure_source_image_artifact_dir(
    data_dir: &Path,
    root: &Path,
    source_dir: &Path,
    source_id: &SourceId,
) -> Result<()> {
    if source_dir == data_dir
        || source_dir == root
        || !source_dir.starts_with(root)
        || source_dir.parent() != Some(root)
    {
        bail!(
            "unsafe image artifact source directory for source {}: {}",
            source_id.0,
            source_dir.display()
        );
    }
    Ok(())
}

fn ensure_relative_image_artifact_path(relative_path: &Path) -> Result<()> {
    let components: Vec<Component<'_>> = relative_path.components().collect();
    match components.as_slice() {
        [Component::Normal(root), Component::Normal(source), Component::Normal(file)]
            if root.to_str() == Some(IMAGE_ARTIFACTS_DIR)
                && safe_component_text(source).is_some()
                && safe_component_text(file).is_some() =>
        {
            Ok(())
        }
        _ => bail!(
            "unsafe image artifact relative path: {}",
            relative_path.display()
        ),
    }
}

fn image_artifact_tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    path.with_file_name(format!("{}.tmp-{}", name, std::process::id()))
}

fn sanitize_path_component(value: &str, fallback_prefix: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return hashed_component(fallback_prefix, value);
    }
    if sanitized == value {
        sanitized.to_string()
    } else {
        format!("{}-{}", sanitized, component_hash(value))
    }
}

fn safe_component_text(component: &std::ffi::OsStr) -> Option<&str> {
    component
        .to_str()
        .filter(|value| !value.is_empty() && *value != "." && *value != "..")
}

fn hashed_component(prefix: &str, value: &str) -> String {
    format!("{}-{}", prefix, component_hash(value))
}

fn component_hash(value: &str) -> String {
    let digest = hex_sha256(value.as_bytes());
    digest[..COMPONENT_HASH_LEN].to_string()
}

fn sanitize_extension(value: &str) -> String {
    let sanitized = value
        .trim_matches('.')
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "bin".to_string()
    } else {
        sanitized.to_string()
    }
}

fn source_pdf_page_count(path: &Path) -> Result<Option<usize>> {
    let is_pdf = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    if !is_pdf {
        return Ok(None);
    }

    #[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
    {
        Ok(Some(
            lopdf::Document::load(path)
                .context("failed to open PDF for page-count scan")?
                .get_pages()
                .len(),
        ))
    }

    #[cfg(not(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber")))]
    {
        Ok(None)
    }
}

fn file_hash(path: &Path) -> Result<String> {
    let data = std::fs::read(path).with_context(|| format!("read file: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn source_path_is_missing(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) => {
            Err(error).with_context(|| format!("check source path metadata: {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deletion::{DeletionOutcome, DeletionProduct};
    use anyhow::bail;
    use async_trait::async_trait;
    use futures::StreamExt;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use crate::chunker::chunk_evidence;
    #[cfg(feature = "qdrant")]
    use crate::config::QdrantConfig;
    use crate::config::{Config, GraphConfig, RetrievalConfig};
    use crate::image_limits::ImageArtifactLimitError;
    #[cfg(feature = "qdrant")]
    use crate::index::qdrant::QdrantClient;
    use crate::index::sqlite_fts::FtsMaintenanceStatus;
    use crate::ocr::{
        source_ingest_diagnostics, OcrLine, OcrPageOutput, OcrPageRequest, OcrProvider,
    };
    use crate::provider::{
        ChatMessageContent, ChatModel, ChatRequest, ChatResponse, ChatStream, ImageDescribeRequest,
        ImageDescription, ProviderError, ProviderResult, TokenUsage, VisionModel,
    };
    use crate::retrieve::RetrievalPipeline;
    use crate::task::TaskKind;
    use crate::types::{
        BBox, ChunkId, EdgeType, EvidenceId, EvidenceKind, EvidenceUnit, GraphEdge, GraphNode,
        GraphNodeId, GraphNodeKind, OcrProfile, OcrSourceStatus, RetrievalOrigin, SourceLocator,
    };
    use crate::vision_caption::{vision_caption_prompt_hash, ImageCaptionStatus};

    mod ingest_pdf_tests;
    #[path = "ingest_profile_tests.rs"]
    mod ingest_profile_tests;

    struct FailingEmbeddingClient;

    #[async_trait]
    impl EmbeddingClient for FailingEmbeddingClient {
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            bail!("embedding unavailable")
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    struct SelectiveFailingEmbeddingClient;

    #[async_trait]
    impl EmbeddingClient for SelectiveFailingEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            if texts.iter().any(|text| text.contains("FAIL_EMBED")) {
                bail!("selective embedding unavailable");
            }
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    struct StaticEmbeddingClient;

    #[async_trait]
    impl EmbeddingClient for StaticEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    struct SentinelEmbeddingClient;

    #[async_trait]
    impl EmbeddingClient for SentinelEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|_| unit_l2_test_vector(vec![98765.0, 43210.0]))
                .collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    #[derive(Clone)]
    struct RecordingEmbeddingClient {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl RecordingEmbeddingClient {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .expect("recording embedding calls lock should not be poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl EmbeddingClient for RecordingEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls
                .lock()
                .expect("recording embedding calls lock should not be poisoned")
                .push(texts.to_vec());
            Ok(texts
                .iter()
                .map(|text| {
                    let sum = text.bytes().fold(0_u32, |acc, byte| acc + u32::from(byte));
                    unit_l2_test_vector(vec![sum as f32, 1.0])
                })
                .collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    fn unit_l2_test_vector(mut vector: Vec<f32>) -> Vec<f32> {
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector
    }

    #[derive(Clone)]
    struct DelayedRecordingEmbeddingClient {
        calls: Arc<Mutex<Vec<usize>>>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    impl DelayedRecordingEmbeddingClient {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                in_flight: Arc::new(AtomicUsize::new(0)),
                max_in_flight: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn call_sizes(&self) -> Vec<usize> {
            self.calls
                .lock()
                .expect("delayed embedding calls lock should not be poisoned")
                .clone()
        }

        fn max_in_flight(&self) -> usize {
            self.max_in_flight.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EmbeddingClient for DelayedRecordingEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls
                .lock()
                .expect("delayed embedding calls lock should not be poisoned")
                .push(texts.len());
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(25)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    struct CaptionKeywordEmbeddingClient;

    #[async_trait]
    impl EmbeddingClient for CaptionKeywordEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| caption_keyword_vector(text))
                .collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    fn caption_keyword_vector(text: &str) -> Vec<f32> {
        if text.to_ascii_lowercase().contains("captionneedle") {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        }
    }

    #[derive(Clone)]
    struct MockVisionModel {
        responses: Arc<Mutex<VecDeque<String>>>,
        calls: Arc<AtomicUsize>,
    }

    impl MockVisionModel {
        fn new<S>(responses: impl IntoIterator<Item = S>) -> Self
        where
            S: Into<String>,
        {
            Self {
                responses: Arc::new(Mutex::new(
                    responses
                        .into_iter()
                        .map(Into::into)
                        .collect::<VecDeque<_>>(),
                )),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl VisionModel for MockVisionModel {
        async fn describe_image(
            &self,
            _req: ImageDescribeRequest,
        ) -> ProviderResult<ImageDescription> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let text = self
                .responses
                .lock()
                .expect("mock response lock should not be poisoned")
                .pop_front()
                .expect("mock vision response should be available");
            Ok(ImageDescription { text })
        }
    }

    #[derive(Clone)]
    struct MockOcrProvider {
        profile: OcrProfile,
        calls: Arc<Mutex<Vec<OcrPageRequest>>>,
    }

    impl MockOcrProvider {
        fn new(language: &str, profile: &str) -> Self {
            Self {
                profile: OcrProfile {
                    provider: "test".into(),
                    engine: "mock-ocr".into(),
                    engine_version: Some("1.0".into()),
                    language: language.into(),
                    profile: profile.into(),
                },
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn call_count(&self) -> usize {
            self.calls
                .lock()
                .expect("mock OCR calls lock should not be poisoned")
                .len()
        }
    }

    impl OcrProvider for MockOcrProvider {
        fn profile(&self) -> OcrProfile {
            self.profile.clone()
        }

        fn recognize_page(&self, request: &OcrPageRequest) -> Result<OcrPageOutput> {
            self.calls
                .lock()
                .expect("mock OCR calls lock should not be poisoned")
                .push(request.clone());
            Ok(OcrPageOutput {
                lines: vec![OcrLine {
                    line_index: Some(1),
                    text: "ocrneedle scanned invoice total".into(),
                    bbox: Some(BBox {
                        x0: 10.0,
                        y0: 20.0,
                        x1: 120.0,
                        y1: 36.0,
                    }),
                    confidence: Some(0.97),
                    words: Vec::new(),
                }],
            })
        }
    }

    #[derive(Clone)]
    struct MockGraphChatModel {
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    impl MockGraphChatModel {
        fn succeeds() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                fail: false,
            }
        }

        fn fails() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                fail: true,
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ChatModel for MockGraphChatModel {
        async fn chat(&self, req: ChatRequest) -> ProviderResult<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(ProviderError::MalformedResponse {
                    operation: "test graph extraction",
                    message: "provider unavailable".to_string(),
                });
            }
            let chunk_id = first_prompt_chunk_id(&req).unwrap_or_else(|| "missing-chunk".into());
            Ok(ChatResponse {
                content: generated_graph_response(&chunk_id),
                finish_reason: None,
                usage: Some(TokenUsage {
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    total_tokens: Some(2),
                }),
            })
        }

        async fn stream_chat(&self, _req: ChatRequest) -> ProviderResult<ChatStream> {
            Ok(futures::stream::empty().boxed())
        }
    }

    fn first_prompt_chunk_id(req: &ChatRequest) -> Option<String> {
        req.messages.iter().find_map(|message| {
            let ChatMessageContent::Text(text) = &message.content else {
                return None;
            };
            let (_, rest) = text.split_once("<chunk id=\"")?;
            let (chunk_id, _) = rest.split_once('"')?;
            Some(chunk_id.to_string())
        })
    }

    fn generated_graph_response(chunk_id: &str) -> String {
        serde_json::json!({
            "entities": [
                {
                    "name": "Feature A",
                    "type": "feature",
                    "description": "An extracted feature",
                    "source_spans": [format!("{chunk_id}:1-1")]
                },
                {
                    "name": "Component B",
                    "type": "component",
                    "description": "An extracted component",
                    "source_spans": [format!("{chunk_id}:1-1")]
                }
            ],
            "relationships": [
                {
                    "source": "Feature A",
                    "target": "Component B",
                    "type": "supports",
                    "description": "Feature A supports Component B.",
                    "confidence": 0.75,
                    "source_spans": [format!("{chunk_id}:1-1")]
                }
            ],
            "claims": [
                {
                    "claim": "Feature A supports Component B.",
                    "subject": "Feature A",
                    "predicate": "supports",
                    "object": "Component B",
                    "source_spans": [format!("{chunk_id}:1-1")]
                }
            ]
        })
        .to_string()
    }

    fn test_source(id: &str, path: PathBuf) -> Source {
        Source {
            id: SourceId(id.to_string()),
            path,
            hash: "hash".into(),
            status: SourceStatus::Indexed,
            parser_used: Some("plaintext".into()),
            last_ingested_at: None,
        }
    }

    fn prepared_source_with_image_bytes(id: &str, bytes: usize) -> PreparedSourceIngest {
        let source_id = SourceId(id.to_string());
        let image_id = ImageId(format!("{id}-image"));
        let evidence_id = EvidenceId(format!("{id}-image-evidence"));
        let files = if bytes == 0 {
            Vec::new()
        } else {
            vec![PreparedImageFile {
                absolute_path: PathBuf::from(format!("/tmp/{id}.png")),
                bytes: vec![0; bytes],
                content_hash: format!("{id}-content-hash"),
                page: 1,
                image_index: 1,
            }]
        };
        let artifacts = files
            .iter()
            .map(|file| ImageArtifact {
                image_id: image_id.clone(),
                source_id: source_id.clone(),
                evidence_id: evidence_id.clone(),
                relative_path: PathBuf::from(format!("{id}.png")),
                content_hash: file.content_hash.clone(),
                mime_type: "image/png".into(),
                width: 1,
                height: 1,
                page: file.page,
                image_index: file.image_index,
                bbox: None,
            })
            .collect();
        PreparedSourceIngest {
            task_id: None,
            source: test_source(id, PathBuf::from(format!("/tmp/{id}.txt"))),
            evidence: Vec::new(),
            chunks: Vec::new(),
            links: Vec::new(),
            evidence_spans: Vec::new(),
            image_artifacts: PreparedImageArtifacts {
                evidence: Vec::new(),
                artifacts,
                files,
                text_proximities: Vec::new(),
            },
            graph_nodes: Vec::new(),
            graph_edges: Vec::new(),
            child_chunks: Vec::new(),
            embedding_phase: PhaseTiming::start(IngestTaskStage::EmbeddingQueueWait.as_str()),
        }
    }

    fn test_evidence(source_id: &SourceId, id: &str, text: &str) -> EvidenceUnit {
        EvidenceUnit {
            id: EvidenceId(id.to_string()),
            source_id: source_id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: source_id.0.clone(),
                line_start: 1,
                line_end: None,
            },
            text: text.to_string(),
            text_hash: format!("hash-{id}"),
            heading_path: Vec::new(),
            language: None,
            position: 0,
        }
    }

    fn test_child(source_id: &SourceId, id: &str, evidence_id: &EvidenceId, text: &str) -> Chunk {
        Chunk {
            id: ChunkId(id.to_string()),
            source_id: source_id.clone(),
            chunk_hash: format!("hash-{id}"),
            embedding_input_hash: None,
            text: text.to_string(),
            context_text: None,
            token_count: 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: vec![evidence_id.clone()],
        }
    }

    fn insert_source_with_child(store: &Store, source: &Source, chunk_id: &str) -> Result<Chunk> {
        insert_source_with_child_text(store, source, chunk_id, "old text")
    }

    fn insert_source_with_child_text(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        text: &str,
    ) -> Result<Chunk> {
        ensure_default_test_embedding_profile(store);
        store.add_source(source)?;
        insert_child_text(store, &source.id, chunk_id, text)
    }

    fn ensure_default_test_embedding_profile(store: &Store) {
        let embedding_profile_spec = test_embedding_profile_spec(2);
        store
            .ensure_embedding_profile(
                &EmbeddingProfileId::default_profile(),
                embedding_profile_spec.as_store_config(),
            )
            .unwrap();
    }

    fn insert_child_text(
        store: &Store,
        source_id: &SourceId,
        chunk_id: &str,
        text: &str,
    ) -> Result<Chunk> {
        let evidence = test_evidence(source_id, &format!("evidence-{chunk_id}"), text);
        let chunk = test_child(source_id, chunk_id, &evidence.id, text);
        store.bulk_insert_evidence(&[evidence])?;
        store.bulk_insert_chunks(std::slice::from_ref(&chunk))?;
        store.link_chunk_evidence(&[(chunk.id.clone(), chunk.evidence_unit_ids[0].clone())])?;
        Ok(chunk)
    }

    fn hnsw_with_chunks(chunks: &[Chunk]) -> HnswIndex {
        let mut hnsw = HnswIndex::new();
        for (idx, chunk) in chunks.iter().enumerate() {
            // Unit-L2 so HNSW cosine fail-closed accepts the fixture.
            hnsw.add(&chunk.id, unit_l2_test_vector(vec![idx as f32, 1.0]));
        }
        hnsw.build().unwrap();
        hnsw
    }

    fn store_vectors_for_chunks(store: &Store, chunks: &[Chunk]) {
        let vectors = chunks
            .iter()
            .enumerate()
            .map(|(idx, chunk)| VectorDocument {
                chunk_id: chunk.id.clone(),
                source_id: chunk.source_id.clone(),
                vector: unit_l2_test_vector(vec![idx as f32, 1.0]),
            })
            .collect::<Vec<_>>();
        store.replace_all_vector_documents(&vectors).unwrap();
    }

    fn sqlite_writer_resource_for_test() -> Arc<ObservableResource> {
        global_resource_registry().resource(
            "sqlite_writer",
            "sqlite_write",
            ResourceLimitConfig {
                capacity: 1,
                queue_capacity: 1,
                queue_timeout: Duration::from_secs(5),
            },
        )
    }

    fn named_resource_for_test(name: &'static str, kind: &'static str) -> Arc<ObservableResource> {
        global_resource_registry().resource(
            name,
            kind,
            ResourceLimitConfig {
                capacity: 1,
                queue_capacity: 1,
                queue_timeout: Duration::from_secs(5),
            },
        )
    }

    fn release_after_short_wait(permit: ResourcePermit) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            drop(permit);
        })
    }

    fn assert_sqlite_writer_waits_for_test<F>(resource: &Arc<ObservableResource>, write: F)
    where
        F: FnOnce(),
    {
        let before = resource.snapshot().queue_wait_ms_total;
        let held = resource.acquire_blocking().expect("held writer permit");
        let release = release_after_short_wait(held);

        write();

        release.join().expect("writer release thread joins");
        let after = resource.snapshot();
        assert!(
            after.queue_wait_ms_total > before,
            "expected write to wait through sqlite_writer queue; before={before}, after={after:?}"
        );
    }

    fn test_config() -> Config {
        Config {
            store: Default::default(),
            parser: Default::default(),
            embedding: Default::default(),
            retrieval: Default::default(),
            vector_index: Default::default(),
            graph: Default::default(),
            rerank: Default::default(),
            context: Default::default(),
            vision: Default::default(),
            ocr: Default::default(),
            chat: Default::default(),
            verifier: Default::default(),
            qdrant: Default::default(),
            index_gc: Default::default(),
            cli: Default::default(),
            daemon: Default::default(),
            collection_watcher: Default::default(),
        }
    }

    #[test]
    fn fts_startup_maintenance_pipeline_rebuilds_then_skips() {
        let tempdir = tempfile::tempdir().unwrap();
        let db_path = tempdir.path().join("verbatim.db");
        {
            let store = Store::new(&db_path).unwrap();
            let source = test_source("src-fts-startup", tempdir.path().join("source.txt"));
            insert_source_with_child_text(&store, &source, "chunk-fts-startup", "alpha startup")
                .unwrap();
        }
        let mut config = test_config();
        config.embedding.provider = "test".to_string();
        config.embedding.base_url = String::new();
        config.embedding.model = "test-embedding".to_string();
        config.embedding.dimension = 2;
        config.embedding.query_instruction = String::new();

        let first = IngestPipeline::new(&config, tempdir.path()).unwrap();
        assert_eq!(
            first.fts_startup_maintenance().status,
            FtsMaintenanceStatus::Rebuilt
        );
        assert_eq!(
            first.lexical_index().search("alpha", 5).unwrap()[0].0 .0,
            "chunk-fts-startup"
        );
        drop(first);

        let second = IngestPipeline::new(&config, tempdir.path()).unwrap();
        assert_eq!(
            second.fts_startup_maintenance().status,
            FtsMaintenanceStatus::Skipped
        );
        assert_eq!(
            second.lexical_index().search("alpha", 5).unwrap()[0].0 .0,
            "chunk-fts-startup"
        );
    }

    fn embedding_context_config(context_window_tokens: usize) -> Config {
        let mut config = test_config();
        config.embedding.enabled = true;
        config.embedding.base_url = "https://embeddings.example.test/v1".into();
        config.embedding.model = "same-configured-model".into();
        config.embedding.context_window_tokens = Some(context_window_tokens);
        config.embedding.served_model = Some("same-configured-model".into());
        config.embedding.dtype = Some("float16".into());
        config.embedding.quantization = Some("fp16".into());
        config.embedding.weight_identity = Some("sha256:weights-a".into());
        config
    }

    fn seed_indexed_source_for_profile(
        store: &Store,
        profile_id: &EmbeddingProfileId,
        source_path: &Path,
    ) -> SourceId {
        let mut source = test_source("src-context", source_path.to_path_buf());
        source.hash = file_hash(source_path).unwrap();
        store.add_source(&source).unwrap();
        let chunk =
            insert_child_text(store, &source.id, "chunk-context", "alpha context text").unwrap();
        store
            .replace_source_vector_documents_for_profile(
                profile_id,
                &source.id,
                &[VectorDocument {
                    chunk_id: chunk.id,
                    source_id: source.id.clone(),
                    vector: vec![1.0, 0.0],
                }],
            )
            .unwrap();
        source.id
    }

    #[test]
    fn add_source_returns_existing_id_for_duplicate_path_without_resetting_status() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let source_path = tempdir.path().join("duplicate.txt");
        fs::write(&source_path, "same source text").unwrap();

        let first_id = pipeline.add_source(&source_path).unwrap();
        pipeline
            .store()
            .update_source_status(&first_id, &SourceStatus::Indexed)
            .unwrap();

        let second_id = pipeline.add_source(&source_path).unwrap();
        let sources = pipeline.store().list_sources().unwrap();
        let stored_source = pipeline.store().get_source(&first_id).unwrap().unwrap();

        assert_eq!(second_id, first_id);
        assert_eq!(sources.len(), 1);
        assert_eq!(stored_source.status, SourceStatus::Indexed);
    }

    #[test]
    fn vector_memory_estimate_uses_two_f32_copies_and_rounds_up() {
        assert_eq!(estimate_vector_memory_mb(0, 4096), 0);
        assert_eq!(estimate_vector_memory_mb(1, 2), 1);
        assert_eq!(estimate_vector_memory_mb(128, 1024), 1);
        assert_eq!(estimate_vector_memory_mb(129, 1024), 2);
    }

    #[tokio::test]
    async fn prepare_full_indexes_marks_reservation_degraded_under_slow_warn_pressure() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let memory_budget = MemoryBudget::new(
            Some(1),
            crate::config::MemoryBudgetEnforcement::SlowWarn,
            Duration::from_millis(500),
            25,
        );
        let pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_memory_budget(memory_budget.clone());
        let source_id = SourceId("src-memory-slow-warn".into());
        let evidence_id = EvidenceId("evidence-memory-slow-warn".into());
        let chunks = vec![test_child(
            &source_id,
            "chunk-memory-slow-warn",
            &evidence_id,
            "memory pressure text",
        )];

        let prepared = pipeline
            .prepare_full_indexes_for_chunks(&EmbeddingProfileId::default_profile(), &chunks)
            .await
            .unwrap();

        assert!(prepared
            ._memory_reservation
            .as_ref()
            .is_some_and(MemoryReservationGuard::degraded));
        assert_eq!(memory_budget.reserved_mb(), 1);
        drop(prepared);
        assert_eq!(memory_budget.reserved_mb(), 0);
    }

    #[tokio::test]
    async fn prepare_full_indexes_fails_clearly_when_memory_budget_is_exceeded() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let memory_budget = MemoryBudget::new(
            Some(1),
            crate::config::MemoryBudgetEnforcement::Fail,
            Duration::from_millis(500),
            25,
        );
        let pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_memory_budget(memory_budget);
        let source_id = SourceId("src-memory-fail".into());
        let evidence_id = EvidenceId("evidence-memory-fail".into());
        let chunks = vec![test_child(
            &source_id,
            "chunk-memory-fail",
            &evidence_id,
            "memory fail text",
        )];

        let error = match pipeline
            .prepare_full_indexes_for_chunks(&EmbeddingProfileId::default_profile(), &chunks)
            .await
        {
            Ok(_) => panic!("expected memory budget failure"),
            Err(error) => error,
        };

        assert!(
            format!("{error:#}").contains("memory budget exceeded"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn ingest_source_records_commit_io_telemetry_spans() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let source_path = tempdir.path().join("io-telemetry.md");
        fs::write(
            &source_path,
            "# I/O telemetry\n\nThis source creates chunks and vectors for ingest telemetry.",
        )
        .unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        let task_id = TaskId("task-io-telemetry".into());
        pipeline
            .store()
            .create_task(
                &task_id,
                TaskKind::Ingest,
                &serde_json::json!({ "source_id": source_id.0 }),
            )
            .unwrap();
        pipeline.store().start_task(&task_id).unwrap();

        pipeline
            .ingest_source_with_task(&source_id, &task_id)
            .await
            .unwrap();

        let spans = pipeline.store().list_task_spans(&task_id).unwrap();
        let db_span = spans
            .iter()
            .find(|span| span.phase == IngestTaskStage::SqliteWrite.as_str())
            .expect("ingest should record sqlite write span");
        assert_eq!(db_span.metadata["operation"], "replace_source_contents");
        assert_eq!(db_span.metadata["io"]["scope"], "source_ingest_commit");
        assert_eq!(db_span.metadata["io"]["storage"], "sqlite");
        assert_eq!(db_span.metadata["io"]["logical_rows"]["sources"], 1);
        assert!(
            db_span.metadata["io"]["logical_rows"]["chunks"]
                .as_u64()
                .unwrap_or_default()
                > 0
        );
        assert!(
            db_span.metadata["io"]["estimated_logical_write_rows"]
                .as_u64()
                .unwrap_or_default()
                >= db_span.metadata["io"]["logical_rows"]["chunks"]
                    .as_u64()
                    .unwrap_or_default()
        );

        let bm25_span = spans
            .iter()
            .find(|span| span.phase == IngestTaskStage::Bm25Index.as_str())
            .expect("ingest should record bm25 index span");
        assert_eq!(
            bm25_span.metadata["operation"],
            "sqlite_fts_triggered_chunk_index"
        );
        assert_eq!(bm25_span.metadata["backend"], "sqlite_fts5");
        assert_eq!(
            bm25_span.metadata["triggered_by"],
            serde_json::json!(["delete_source_cascade", "insert_child_chunks"])
        );
        assert!(
            bm25_span.metadata["indexed_child_chunks"]
                .as_u64()
                .unwrap_or_default()
                > 0
        );

        let publish_span = spans
            .iter()
            .find(|span| {
                span.phase == IngestTaskStage::VectorIndex.as_str()
                    && span.metadata["io"]["scope"] == "source_ingest_index_publish"
            })
            .expect("ingest should record vector index publishing span");
        assert_eq!(
            publish_span.metadata["io"]["scope"],
            "source_ingest_index_publish"
        );
        assert_eq!(publish_span.metadata["io"]["storage"], "filesystem");
        assert!(
            publish_span.metadata["io"]["staged_artifact_bytes"]
                .as_u64()
                .unwrap_or_default()
                > 0
        );
        assert!(
            publish_span.metadata["io"]["manifest_bytes"]
                .as_u64()
                .unwrap_or_default()
                > 0
        );
    }

    #[tokio::test]
    async fn source_batch_ingest_publishes_vector_index_once_for_multiple_sources() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let first_path = tempdir.path().join("batch-first.md");
        let second_path = tempdir.path().join("batch-second.md");
        fs::write(&first_path, "# First\n\nAlpha source batch vector body.").unwrap();
        fs::write(&second_path, "# Second\n\nBeta source batch vector body.").unwrap();
        let first_id = pipeline.add_source(&first_path).unwrap();
        let second_id = pipeline.add_source(&second_path).unwrap();
        let first_task = TaskId("task-batch-first".into());
        let second_task = TaskId("task-batch-second".into());
        for (task_id, source_id) in [(&first_task, &first_id), (&second_task, &second_id)] {
            pipeline
                .store()
                .create_task(
                    task_id,
                    TaskKind::Ingest,
                    &serde_json::json!({ "source_id": source_id.0 }),
                )
                .unwrap();
            pipeline.store().start_task(task_id).unwrap();
        }
        let generation_before = pipeline
            .store()
            .index_generation_for_profile(&EmbeddingProfileId::default_profile())
            .unwrap();

        let outcomes = pipeline
            .ingest_sources_with_tasks(&[
                (first_id.clone(), first_task.clone()),
                (second_id.clone(), second_task.clone()),
            ])
            .await;

        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|outcome| outcome.result.is_ok()));
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&first_id),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&second_id),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            pipeline
                .store()
                .index_generation_for_profile(&EmbeddingProfileId::default_profile())
                .unwrap(),
            generation_before + 1,
            "one source batch should publish one vector-index generation, not one per source"
        );
    }

    #[tokio::test]
    async fn source_batch_publish_cleanup_failure_keeps_sources_embedded() {
        // Regression: a post-publish stale-image-cleanup failure in the batched
        // path must NOT be treated as an index publication failure. Once the
        // HNSW generation has been published, committed sources must remain
        // Embedded/fresh and outcomes must stay Ok.
        let source_tempdir = tempfile::tempdir().unwrap();
        // Use a real directory as data_dir so staging + publish succeed, then
        // poison the per-source image artifact path so cleanup fails.
        let data_dir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            data_dir.path().to_path_buf(),
        );
        let first_path = source_tempdir.path().join("batch-cleanup-first.md");
        let second_path = source_tempdir.path().join("batch-cleanup-second.md");
        fs::write(&first_path, "# First\n\nAlpha batch cleanup vector body.").unwrap();
        fs::write(&second_path, "# Second\n\nBeta batch cleanup vector body.").unwrap();
        let first_id = pipeline.add_source(&first_path).unwrap();
        let second_id = pipeline.add_source(&second_path).unwrap();
        // Poison image artifact cleanup for both sources by turning their
        // artifact directory paths into regular files.
        write_blocking_image_artifact_path(data_dir.path(), &first_id);
        write_blocking_image_artifact_path(data_dir.path(), &second_id);
        let first_task = TaskId("task-batch-cleanup-first".into());
        let second_task = TaskId("task-batch-cleanup-second".into());
        for (task_id, source_id) in [(&first_task, &first_id), (&second_task, &second_id)] {
            pipeline
                .store()
                .create_task(
                    task_id,
                    TaskKind::Ingest,
                    &serde_json::json!({ "source_id": source_id.0 }),
                )
                .unwrap();
            pipeline.store().start_task(task_id).unwrap();
        }
        let profile = EmbeddingProfileId::default_profile();
        let generation_before = pipeline
            .store()
            .index_generation_for_profile(&profile)
            .unwrap();

        let outcomes = pipeline
            .ingest_sources_with_tasks(&[
                (first_id.clone(), first_task.clone()),
                (second_id.clone(), second_task.clone()),
            ])
            .await;

        assert_eq!(outcomes.len(), 2);
        assert!(
            outcomes.iter().all(|outcome| outcome.result.is_ok()),
            "post-publish cleanup failure must not fail committed batch sources"
        );
        // The index generation was bumped exactly once for the batch.
        assert_eq!(
            pipeline
                .store()
                .index_generation_for_profile(&profile)
                .unwrap(),
            generation_before + 1,
            "batched index publish must complete and bump the generation once"
        );
        for source_id in [&first_id, &second_id] {
            assert_eq!(
                pipeline
                    .store()
                    .count_vector_documents_for_profile(&profile, Some(source_id))
                    .unwrap(),
                1
            );
            assert!(
                !pipeline
                    .store()
                    .source_vectors_stale_for_profile(&profile, source_id)
                    .unwrap(),
                "committed source must stay fresh after post-publish cleanup failure"
            );
        }
    }

    #[tokio::test]
    async fn ingest_task_stage_telemetry_records_bounded_private_safe_stage_names() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            SentinelEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_embedding_controls(2, 1);
        let path = tempdir
            .path()
            .join("PRIVATE_PATH_MARKER-stage-telemetry.md");
        let sensitive_text = "SENSITIVE_SOURCE_TEXT_SHOULD_NOT_APPEAR";
        fs::write(
            &path,
            format!("# Heading\n\n{sensitive_text} retrieval telemetry body\n"),
        )
        .unwrap();
        let source = test_source("src-stage-telemetry", path);
        let source_id = source.id.clone();
        pipeline.store().add_source(&source).unwrap();
        let task_id = TaskId("task-stage-telemetry".into());
        pipeline
            .store()
            .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
            .unwrap();
        pipeline.store().start_task(&task_id).unwrap();

        pipeline
            .ingest_source_with_task(&source_id, &task_id)
            .await
            .unwrap();

        let task = pipeline.store().get_task(&task_id).unwrap().unwrap();
        let events = pipeline
            .store()
            .list_task_events(&task_id, None, 100)
            .unwrap();
        let spans = pipeline.store().list_task_spans(&task_id).unwrap();
        let span_phases = spans
            .iter()
            .map(|span| span.phase.as_str())
            .collect::<Vec<_>>();

        for expected in [
            IngestTaskStage::Parse,
            IngestTaskStage::Chunk,
            IngestTaskStage::EmbeddingQueueWait,
            IngestTaskStage::EmbeddingRequest,
            IngestTaskStage::EmbeddingPostprocess,
            IngestTaskStage::SqliteWrite,
            IngestTaskStage::Bm25Index,
            IngestTaskStage::VectorIndex,
        ] {
            assert!(
                span_phases.contains(&expected.as_str()),
                "missing stage {} in {span_phases:?}",
                expected.as_str()
            );
        }
        assert!(span_phases
            .iter()
            .all(|phase| crate::task::INGEST_TASK_STAGE_NAMES.contains(phase)));
        let chunk_metadata = &spans
            .iter()
            .find(|span| span.phase == IngestTaskStage::Chunk.as_str())
            .expect("chunk phase telemetry should exist")
            .metadata;
        assert_eq!(chunk_metadata["canonical_evidence_count"], 0);
        assert!(chunk_metadata["noncanonical_evidence_count"]
            .as_u64()
            .is_some_and(|count| count > 0));
        assert_eq!(
            chunk_metadata["chunking_strategies"],
            serde_json::json!(["chunk_evidence"])
        );
        assert_eq!(
            chunk_metadata["canonical_chunker_version"],
            CANONICAL_CHUNKER_VERSION
        );
        assert_eq!(chunk_metadata["canonical_target_tokens"], 300);
        assert_eq!(chunk_metadata["canonical_overlap_units"], 2);
        assert_eq!(chunk_metadata["canonical_max_units_per_child"], 20);

        let progress_phases = events
            .iter()
            .filter(|event| event.event_type == "progress")
            .filter_map(|event| event.payload["phase"]["name"].as_str())
            .collect::<Vec<_>>();
        assert!(progress_phases.contains(&IngestTaskStage::Parse.as_str()));
        assert!(progress_phases.contains(&IngestTaskStage::Chunk.as_str()));

        let encoded = serde_json::to_string(&(task, events, spans)).unwrap();
        assert!(!encoded.contains(sensitive_text));
        assert!(!encoded.contains("PRIVATE_PATH_MARKER"));
        assert!(!encoded.contains("98765"));
        assert!(!encoded.contains("43210"));
    }

    #[tokio::test]
    async fn build_embedding_profile_uses_existing_chunks_without_reparsing() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", PathBuf::from("/tmp/first.txt"));
        let second = test_source("src-2", PathBuf::from("/tmp/second.txt"));
        insert_source_with_child_text(&store, &first, "chunk-1", "alpha text").unwrap();
        insert_source_with_child_text(&store, &second, "chunk-2", "beta text").unwrap();
        let alt_profile = EmbeddingProfileId::new("alt").unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let source_count = pipeline
            .build_embedding_profile(&alt_profile, Some(&first.id))
            .await
            .unwrap();

        assert_eq!(source_count.source_count, 1);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&alt_profile, Some(&first.id))
                .unwrap(),
            1
        );
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&alt_profile, Some(&second.id))
                .unwrap(),
            0
        );

        let all_count = pipeline
            .build_embedding_profile(&alt_profile, None)
            .await
            .unwrap();

        assert_eq!(all_count.source_count, 2);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&alt_profile, None)
                .unwrap(),
            2
        );
        assert_eq!(
            pipeline
                .store()
                .get_source(&first.id)
                .unwrap()
                .unwrap()
                .status,
            SourceStatus::Indexed
        );
    }

    #[tokio::test]
    async fn build_embedding_profile_counts_sources_not_child_chunks() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", PathBuf::from("/tmp/first.txt"));
        insert_source_with_child_text(&store, &source, "chunk-1", "alpha text").unwrap();
        insert_child_text(&store, &source.id, "chunk-2", "beta text").unwrap();
        let alt_profile = EmbeddingProfileId::new("alt").unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let source_count = pipeline
            .build_embedding_profile(&alt_profile, Some(&source.id))
            .await
            .unwrap();

        assert_eq!(source_count.source_count, 1);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&alt_profile, Some(&source.id))
                .unwrap(),
            2
        );

        let all_count = pipeline
            .build_embedding_profile(&alt_profile, None)
            .await
            .unwrap();

        assert_eq!(all_count.source_count, 1);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&alt_profile, None)
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn build_embedding_profile_records_zero_vector_status_for_target_source() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let source_path = tempdir.path().join("empty.txt");
        fs::write(&source_path, "").unwrap();
        let mut source = test_source("src-empty", source_path.clone());
        source.hash = file_hash(&source_path).unwrap();
        source.status = SourceStatus::Stale;
        store.add_source(&source).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let source_count = pipeline
            .build_embedding_profile(&EmbeddingProfileId::default_profile(), Some(&source.id))
            .await
            .unwrap();

        assert_eq!(source_count.source_count, 1);
        assert_eq!(
            pipeline
                .store()
                .get_source(&source.id)
                .unwrap()
                .unwrap()
                .status,
            SourceStatus::Indexed
        );
        assert!(pipeline.check_stale().unwrap().is_empty());
    }

    #[tokio::test]
    async fn build_embedding_profile_records_zero_vector_status_for_all_sources() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let source_path = tempdir.path().join("empty.txt");
        fs::write(&source_path, "").unwrap();
        let mut source = test_source("src-empty", source_path.clone());
        source.hash = file_hash(&source_path).unwrap();
        source.status = SourceStatus::Stale;
        store.add_source(&source).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let source_count = pipeline
            .build_embedding_profile(&EmbeddingProfileId::default_profile(), None)
            .await
            .unwrap();

        assert_eq!(source_count.source_count, 1);
        assert_eq!(
            pipeline
                .store()
                .get_source(&source.id)
                .unwrap()
                .unwrap()
                .status,
            SourceStatus::Indexed
        );
        assert!(pipeline.check_stale().unwrap().is_empty());
    }

    #[tokio::test]
    async fn source_specific_vector_only_rebuild_keeps_unparsed_source_stale() {
        assert_unparsed_source_remains_stale_after_vector_only_rebuild(true).await;
    }

    #[tokio::test]
    async fn all_source_vector_only_rebuild_keeps_unparsed_source_stale() {
        assert_unparsed_source_remains_stale_after_vector_only_rebuild(false).await;
    }

    #[test]
    fn embedding_disabled_check_stale_reports_pending_source_with_unchanged_hash() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_embedding_enabled(false);
        let source_path = tempdir.path().join("pending.txt");
        fs::write(&source_path, "alpha pending source needs lexical indexing").unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();

        let stale = pipeline.check_stale().unwrap();

        assert_eq!(stale, vec![source_id.clone()]);
        assert_eq!(
            pipeline
                .store()
                .get_source(&source_id)
                .unwrap()
                .unwrap()
                .status,
            SourceStatus::Stale
        );
    }

    async fn assert_unparsed_source_remains_stale_after_vector_only_rebuild(source_specific: bool) {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let source_path = tempdir.path().join("unparsed.txt");
        fs::write(&source_path, "alpha text that still needs parse ingest").unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();

        assert_eq!(pipeline.check_stale().unwrap(), vec![source_id.clone()]);

        let source_filter = source_specific.then_some(source_id.clone());
        let rebuilt = pipeline
            .build_embedding_profile(
                &EmbeddingProfileId::default_profile(),
                source_filter.as_ref(),
            )
            .await
            .unwrap();

        assert_eq!(rebuilt.source_count, 1);
        let source = pipeline.store().get_source(&source_id).unwrap().unwrap();
        assert_eq!(source.status, SourceStatus::Stale);
        assert!(source.parser_used.is_none());
        assert_eq!(pipeline.check_stale().unwrap(), vec![source_id]);
    }

    #[tokio::test]
    async fn check_stale_reports_missing_active_profile_vectors() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let source_path = tempdir.path().join("first.txt");
        fs::write(&source_path, "alpha text").unwrap();
        let mut source = test_source("src-1", source_path.clone());
        source.hash = file_hash(&source_path).unwrap();
        insert_source_with_child_text(&store, &source, "chunk-1", "alpha text").unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let stale = pipeline.check_stale().unwrap();

        assert_eq!(stale, vec![source.id.clone()]);
        assert_eq!(
            pipeline
                .store()
                .get_source(&source.id)
                .unwrap()
                .unwrap()
                .status,
            SourceStatus::Stale
        );

        pipeline
            .build_embedding_profile(&EmbeddingProfileId::default_profile(), Some(&source.id))
            .await
            .unwrap();

        assert!(pipeline.check_stale().unwrap().is_empty());
        assert_eq!(
            pipeline
                .store()
                .get_source(&source.id)
                .unwrap()
                .unwrap()
                .status,
            SourceStatus::Indexed
        );
    }

    #[test]
    fn context_window_shrink_marks_existing_profile_vectors_stale() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_path = tempdir.path().join("context.txt");
        fs::write(&source_path, "alpha context text").unwrap();
        let high_config = embedding_context_config(8192);
        let low_config = embedding_context_config(128);
        let source_id = {
            let pipeline = IngestPipeline::new(&high_config, tempdir.path()).unwrap();
            let source_id = seed_indexed_source_for_profile(
                pipeline.store(),
                pipeline.active_embedding_profile_id(),
                &source_path,
            );
            assert!(pipeline.check_stale().unwrap().is_empty());
            source_id
        };

        let pipeline = IngestPipeline::new(&low_config, tempdir.path()).unwrap();
        let status = pipeline.index_status().unwrap();

        assert_eq!(status.stale_source_ids, vec![source_id.0]);
        assert_eq!(status.chunking.embedding_input_budget_tokens, Some(96));
        assert!(status
            .messages
            .iter()
            .any(|message| message.contains("Context shrink requires reingest/reindex")));
        assert!(pipeline
            .store()
            .list_vector_documents_for_profile(pipeline.active_embedding_profile_id())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn context_window_growth_keeps_existing_vectors_as_quality_reindex_opportunity() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_path = tempdir.path().join("context.txt");
        fs::write(&source_path, "alpha context text").unwrap();
        let low_config = embedding_context_config(128);
        let high_config = embedding_context_config(8192);
        {
            let pipeline = IngestPipeline::new(&low_config, tempdir.path()).unwrap();
            seed_indexed_source_for_profile(
                pipeline.store(),
                pipeline.active_embedding_profile_id(),
                &source_path,
            );
            assert!(pipeline.check_stale().unwrap().is_empty());
        }

        let pipeline = IngestPipeline::new(&high_config, tempdir.path()).unwrap();
        let status = pipeline.index_status().unwrap();

        assert_eq!(status.stale_source_count, 0);
        assert!(status.stale_source_ids.is_empty());
        assert_eq!(status.chunking.embedding_input_budget_tokens, Some(6144));
        assert!(status.messages.iter().any(|message| {
            message.contains("context growth is a quality reindex opportunity")
        }));
        assert_eq!(
            pipeline
                .store()
                .list_vector_documents_for_profile(pipeline.active_embedding_profile_id())
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn restart_preserves_endpoint_discovered_profile_until_refresh_confirms_same_capability()
    {
        let body = r#"{"data":[{"id":"same-configured-model","served_model":"served-a","max_model_len":8192,"dtype":"float16","quantization":"fp16","revision":"Sha256:ABCDef"}]}"#;
        let (base_url, handle) = spawn_embedding_models_server(vec![body]);
        let tempdir = tempfile::tempdir().unwrap();
        let source_path = tempdir.path().join("context.txt");
        fs::write(&source_path, "alpha context text").unwrap();
        let mut config = test_config();
        config.embedding.enabled = true;
        config.embedding.base_url = base_url;
        config.embedding.model = "same-configured-model".into();
        config.embedding.context_window_tokens = None;
        config.embedding.served_model = None;
        config.embedding.dtype = None;
        config.embedding.quantization = None;
        config.embedding.weight_identity = None;
        config.embedding.capability_cache_ttl_seconds = 60;

        let source_id = {
            let mut pipeline = IngestPipeline::new(&config, tempdir.path()).unwrap();
            assert!(pipeline
                .refresh_embedding_profile_capabilities()
                .await
                .unwrap());
            let status = pipeline.index_status().unwrap();
            assert_eq!(status.capability.served_model.as_deref(), Some("served-a"));
            assert_eq!(status.capability.max_context_tokens, Some(8192));
            let source_id = seed_indexed_source_for_profile(
                pipeline.store(),
                pipeline.active_embedding_profile_id(),
                &source_path,
            );
            assert!(pipeline.check_stale().unwrap().is_empty());
            source_id
        };

        let mut pipeline = IngestPipeline::new(&config, tempdir.path()).unwrap();
        assert_eq!(
            pipeline
                .store()
                .list_vector_documents_for_profile(pipeline.active_embedding_profile_id())
                .unwrap()
                .len(),
            1
        );
        assert!(pipeline.check_stale().unwrap().is_empty());

        assert!(!pipeline
            .refresh_embedding_profile_capabilities()
            .await
            .unwrap());
        let status = pipeline.index_status().unwrap();

        assert_eq!(status.stale_source_ids, Vec::<String>::new());
        assert_eq!(status.capability.served_model.as_deref(), Some("served-a"));
        assert_eq!(status.capability.max_context_tokens, Some(8192));
        assert_eq!(status.chunking.embedding_input_budget_tokens, Some(6144));
        assert_eq!(
            pipeline
                .store()
                .list_vector_documents_for_profile(pipeline.active_embedding_profile_id())
                .unwrap()
                .len(),
            1
        );
        assert!(!pipeline
            .store()
            .find_stale_sources_for_profile(
                &HashMap::from([(source_id.clone(), file_hash(&source_path).unwrap())]),
                pipeline.active_embedding_profile_id(),
            )
            .unwrap()
            .contains(&source_id));

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].line, "GET /v1/models HTTP/1.1");
        assert!(requests[0].body.is_empty());
    }

    #[test]
    fn pipeline_loads_only_active_profile_index_until_profile_switch() {
        let tempdir = tempfile::tempdir().unwrap();
        let db_path = tempdir.path().join("verbatim.db");
        let mut config = test_config();
        config.vector_index.residency = VectorIndexResidency::ResidentHnsw;
        let alt_profile = EmbeddingProfileId::new("alt").unwrap();
        {
            let store = Store::new(&db_path).unwrap();
            let first = test_source("src-1", PathBuf::from("/tmp/first.txt"));
            let second = test_source("src-2", PathBuf::from("/tmp/second.txt"));
            let embedding_profile_spec = EmbeddingProfileSpec::from_config(&config.embedding);
            store
                .ensure_embedding_profile(
                    &EmbeddingProfileId::default_profile(),
                    embedding_profile_spec.as_store_config(),
                )
                .unwrap();
            store
                .ensure_embedding_profile(&alt_profile, embedding_profile_spec.as_store_config())
                .unwrap();
            store.add_source(&first).unwrap();
            store.add_source(&second).unwrap();
            let first_chunk =
                insert_child_text(&store, &first.id, "chunk-1", "alpha text").unwrap();
            let second_chunk =
                insert_child_text(&store, &second.id, "chunk-2", "beta text").unwrap();
            store
                .replace_all_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    &[VectorDocument {
                        chunk_id: first_chunk.id.clone(),
                        source_id: first.id.clone(),
                        vector: vec![1.0, 0.0],
                    }],
                )
                .unwrap();
            store
                .replace_all_vector_documents_for_profile(
                    &alt_profile,
                    &[
                        VectorDocument {
                            chunk_id: first_chunk.id,
                            source_id: first.id,
                            vector: vec![0.0, 1.0],
                        },
                        VectorDocument {
                            chunk_id: second_chunk.id,
                            source_id: second.id,
                            // Unit-L2 so HNSW Euclidean ranking stays cosine-equivalent.
                            vector: unit_l2_test_vector(vec![0.5, 0.5]),
                        },
                    ],
                )
                .unwrap();
        }

        let mut pipeline = IngestPipeline::new(&config, tempdir.path()).unwrap();

        assert_eq!(pipeline.active_embedding_profile_id().as_str(), "default");
        assert_eq!(pipeline.hnsw().len(), 1);

        pipeline.select_embedding_profile(&alt_profile).unwrap();

        assert_eq!(pipeline.hnsw().len(), 2);
    }

    #[test]
    fn select_embedding_profile_waits_for_sqlite_writer_resource() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let profile_id = EmbeddingProfileId::new("writer-gated-select").unwrap();
        let writer = sqlite_writer_resource_for_test();

        assert_sqlite_writer_waits_for_test(&writer, || {
            pipeline.select_embedding_profile(&profile_id).unwrap();
        });

        assert_eq!(pipeline.loaded_profile_id, profile_id);
        assert!(pipeline
            .store()
            .load_embedding_profile_config(&profile_id)
            .unwrap()
            .is_some());
    }

    #[derive(Debug)]
    struct TestHttpRequest {
        line: String,
        body: String,
    }

    #[cfg(feature = "qdrant")]
    const QDRANT_COLLECTION_INFO: &str = r#"{"status":"ok","result":{"config":{"params":{"vectors":{"size":2,"distance":"Cosine"}}},"payload_schema":{"profile_id":{"data_type":"keyword"},"source_id":{"data_type":"keyword"}}}}"#;

    #[cfg(feature = "qdrant")]
    fn qdrant_test_config(url: String) -> QdrantConfig {
        QdrantConfig {
            enabled: true,
            url,
            collection: "verbatim".into(),
            prefer_for_search: false,
            timeout_seconds: 2,
        }
    }

    fn spawn_embedding_models_server(
        bodies: Vec<&'static str>,
    ) -> (String, thread::JoinHandle<Vec<TestHttpRequest>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind embedding test server");
        let addr = listener.local_addr().expect("embedding test server addr");
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for body in bodies {
                let (mut stream, _) = listener.accept().expect("accept embedding request");
                requests.push(read_http_request(&mut stream));
                write_http_response(&mut stream, 200, body);
            }
            requests
        });
        (format!("http://{addr}/v1"), handle)
    }

    #[cfg(feature = "qdrant")]
    fn spawn_qdrant_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, thread::JoinHandle<Vec<TestHttpRequest>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind qdrant test server");
        let addr = listener.local_addr().expect("qdrant test server addr");
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept qdrant request");
                requests.push(read_http_request(&mut stream));
                write_http_response(&mut stream, response.0, response.1);
            }
            requests
        });
        (format!("http://{addr}"), handle)
    }

    fn read_http_request(stream: &mut TcpStream) -> TestHttpRequest {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).expect("read http request");
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
            if http_request_complete(&buffer) {
                break;
            }
        }
        let text = String::from_utf8(buffer).expect("request utf8");
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        TestHttpRequest {
            line: head.lines().next().unwrap_or_default().to_string(),
            body: body.to_string(),
        }
    }

    fn http_request_complete(buffer: &[u8]) -> bool {
        let text = String::from_utf8_lossy(buffer);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let content_len = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        body.len() >= content_len
    }

    fn write_http_response(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = if status == 200 { "OK" } else { "ERR" };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write http response");
    }

    fn sorted_graph_node_ids(nodes: &[GraphNode]) -> Vec<String> {
        let mut ids = nodes
            .iter()
            .map(|node| node.id.0.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn sorted_graph_edge_ids(edges: &[GraphEdge]) -> Vec<String> {
        let mut ids = edges
            .iter()
            .map(|edge| edge.id.0.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn write_pdf_with_image(path: &Path) {
        let image_bytes = vec![255u8; 8 * 8 * 3];
        let content = b"q\n36 0 0 36 72 72 cm\n/Im1 Do\nQ\n";
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8",
                &image_bytes,
            ),
            stream_object(b"<<", content),
        ];
        fs::write(path, pdf_bytes(objects)).expect("fixture PDF should save");
    }

    fn write_pdf_with_text(path: &Path, text: &str) {
        let content = format!("BT\n/F1 12 Tf\n72 120 Td\n({text}) Tj\nET\n");
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            stream_object(b"<<", content.as_bytes()),
        ];
        fs::write(path, pdf_bytes(objects)).expect("fixture PDF should save");
    }

    fn write_pdf_with_text_and_image_filter(path: &Path, filter: Option<&str>, image_bytes: &[u8]) {
        let mut image_prefix =
            b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8"
                .to_vec();
        if let Some(filter) = filter {
            image_prefix.extend(format!(" /Filter /{filter}").as_bytes());
        }
        let content = b"BT\n/F1 12 Tf\n72 120 Td\n(Unsupported image filter text evidence) Tj\nET\nq\n36 0 0 36 72 72 cm\n/Im1 Do\nQ\n";
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 5 0 R >> >> /Contents 6 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            stream_object(&image_prefix, image_bytes),
            stream_object(b"<<", content),
        ];
        fs::write(path, pdf_bytes(objects)).expect("fixture PDF should save");
    }

    fn write_blocking_image_artifact_path(data_dir: &Path, source_id: &SourceId) {
        let path = source_image_artifact_dir(data_dir, source_id).unwrap();
        fs::create_dir_all(path.parent().expect("artifact dir has parent")).unwrap();
        fs::write(path, b"not a directory").unwrap();
    }

    fn test_parsed_image_artifact(extension: &str) -> ParsedImageArtifact {
        test_parsed_image_artifact_with(extension, 1, 1, vec![1, 2, 3, 4], 2, 2)
    }

    fn test_parsed_image_artifact_with(
        extension: &str,
        page: u32,
        image_index: u32,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> ParsedImageArtifact {
        ParsedImageArtifact {
            page,
            image_index,
            bbox: None,
            bytes,
            mime_type: format!("image/{extension}"),
            extension: extension.to_string(),
            width,
            height,
            nearby_text_before: None,
            nearby_text_after: None,
        }
    }

    fn valid_caption_json(short_caption: &str) -> String {
        format!(
            r#"{{
  "type": "diagram",
  "short_caption": "{short_caption}",
  "detailed_description": "A diagram shows an input flowing into an index.",
  "visible_text": ["Input", "Index"],
  "key_entities": ["Input", "Index"],
  "relationships": [{{"from": "Input", "to": "Index", "label": "feeds"}}],
  "answerable_questions": ["What feeds the index?"],
  "uncertainties": ["The small footer text is not legible."]
}}"#
        )
    }

    #[tokio::test]
    async fn born_digital_pdf_ingest_persists_versioned_selector_without_ocr() {
        let tempdir = tempfile::tempdir().unwrap();
        let db_path = tempdir.path().join("verbatim.db");
        let store = Store::new(&db_path).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let path = tempdir.path().join("born-digital.pdf");
        write_pdf_with_text(&path, "Born digital alpha evidence");
        let source_id = pipeline.add_source(&path).unwrap();

        pipeline.ingest_source(&source_id).await.unwrap();
        drop(pipeline);

        let reopened = Store::new(&db_path).unwrap();
        let evidence = reopened.list_evidence_by_source(&source_id).unwrap();
        assert!(evidence.iter().any(|unit| {
            unit.kind == EvidenceKind::Text
                && unit.text == "Born digital alpha evidence"
                && matches!(
                    &unit.locator,
                    SourceLocator::Pdf {
                        page: 1,
                        selector: Some(selector),
                        ..
                    } if selector.version == crate::pdf_selector::PDF_SELECTOR_VERSION
                )
        }));
        assert!(evidence.iter().all(|unit| unit.kind != EvidenceKind::Ocr));
        let artifacts = reopened.list_image_artifacts_by_source(&source_id).unwrap();
        let diagnostics = source_ingest_diagnostics(&path, &evidence, &artifacts, None);
        assert_eq!(diagnostics.ocr.status, OcrSourceStatus::NotRequired);
        assert_eq!(diagnostics.pdf.as_ref().unwrap().image_only_page_count, 0);
    }

    #[tokio::test]
    async fn scanned_image_only_pdf_ingest_rejects_without_ocr() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let path = tempdir.path().join("scanned.pdf");
        write_pdf_with_image(&path);
        let source_id = pipeline.add_source(&path).unwrap();
        let original = pipeline.store().get_source(&source_id).unwrap().unwrap();

        let error = pipeline
            .ingest_source(&source_id)
            .await
            .expect_err("image-only PDFs must fail closed");

        assert_eq!(
            error.downcast_ref::<IngestDiagnosticCode>(),
            Some(&IngestDiagnosticCode::PdfNoUsableTextLayer)
        );
        assert_eq!(error.to_string(), "pdf_no_usable_text_layer");
        assert!(pipeline
            .store()
            .list_evidence_by_source(&source_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            pipeline
                .store()
                .get_source(&source_id)
                .unwrap()
                .unwrap()
                .hash,
            original.hash
        );
    }

    #[tokio::test]
    async fn scanned_image_only_pdf_rejects_before_configured_ocr() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let ocr = MockOcrProvider::new("eng", "default");
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_ocr_provider(ocr.clone());
        let path = tempdir.path().join("ocr-enabled.pdf");
        write_pdf_with_image(&path);
        let source_id = pipeline.add_source(&path).unwrap();

        let error = pipeline
            .ingest_source(&source_id)
            .await
            .expect_err("configured OCR must not rescue image-only PDFs");

        assert_eq!(
            error.downcast_ref::<IngestDiagnosticCode>(),
            Some(&IngestDiagnosticCode::PdfNoUsableTextLayer)
        );
        assert_eq!(error.to_string(), "pdf_no_usable_text_layer");
        assert_eq!(ocr.call_count(), 0);
        assert!(pipeline
            .store()
            .list_evidence_by_source(&source_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn configured_vision_model_requires_explicit_enable() {
        let partial: Config = toml::from_str(
            r#"
[vision]
model = "local-vision"
"#,
        )
        .unwrap();

        let (model, model_name) = configured_vision_model(&partial);
        assert!(model.is_none());
        assert_eq!(model_name, "local-vision");

        let enabled: Config = toml::from_str(
            r#"
[vision]
enabled = true
base_url = "http://127.0.0.1:8000/v1"
model = "local-vision"
"#,
        )
        .unwrap();

        let (model, model_name) = configured_vision_model(&enabled);
        assert!(model.is_some());
        assert_eq!(model_name, "local-vision");
    }

    #[tokio::test]
    async fn mvp_regression_ingest_retrieval_graph_and_source_lifecycle() {
        let tempdir = tempfile::tempdir().unwrap();
        let left_dir = tempdir.path().join("left");
        let right_dir = tempdir.path().join("right");
        fs::create_dir_all(&left_dir).unwrap();
        fs::create_dir_all(&right_dir).unwrap();

        let markdown_path = left_dir.join("notes.md");
        fs::write(
            &markdown_path,
            "# MVP Heading\n\nSee [linked graph](https://example.test/graph) for markdownneedle retrieval.\n\nGraphneighbor sibling evidence stays in the same section.\n",
        )
        .unwrap();
        let same_stem_path = right_dir.join("notes.md");
        fs::write(&same_stem_path, "# Other\n\nsame stem stays distinct.\n").unwrap();
        let plaintext_path = tempdir.path().join("plain.txt");
        fs::write(
            &plaintext_path,
            "plainneedle first line\ncontinues on line two\n\noldplainneedle removal target\n",
        )
        .unwrap();
        let pdf_path = tempdir.path().join("diagram.pdf");
        write_pdf_with_text_and_image_filter(&pdf_path, None, &[7u8; 8 * 8 * 3]);

        let vision = MockVisionModel::new(vec![valid_caption_json(
            "A captionneedle indexing flow diagram.",
        )]);
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            CaptionKeywordEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        let markdown_id = pipeline.add_source(&markdown_path).unwrap();
        let same_stem_id = pipeline.add_source(&same_stem_path).unwrap();
        let plaintext_id = pipeline.add_source(&plaintext_path).unwrap();
        let pdf_id = pipeline.add_source(&pdf_path).unwrap();
        assert_ne!(markdown_id, same_stem_id);
        assert!(markdown_id.0.starts_with("notes-"));
        assert!(same_stem_id.0.starts_with("notes-"));

        for source_id in [&markdown_id, &same_stem_id, &plaintext_id, &pdf_id] {
            pipeline.ingest_source(source_id).await.unwrap();
        }

        let markdown_evidence = pipeline
            .store()
            .list_evidence_by_source(&markdown_id)
            .unwrap();
        assert!(markdown_evidence.iter().any(|unit| {
            unit.heading_path == vec!["MVP Heading"]
                && unit.text.contains("linked graph")
                && unit.text.contains("markdownneedle")
        }));

        let plaintext_evidence = pipeline
            .store()
            .list_evidence_by_source(&plaintext_id)
            .unwrap();
        assert!(plaintext_evidence.iter().any(|unit| {
            unit.text == "plainneedle first line continues on line two"
                && matches!(
                    unit.locator,
                    SourceLocator::Document {
                        line_start: 1,
                        line_end: Some(2),
                        ..
                    }
                )
        }));

        let pdf_evidence = pipeline.store().list_evidence_by_source(&pdf_id).unwrap();
        assert!(pdf_evidence.iter().any(|unit| {
            unit.kind == EvidenceKind::Text
                && unit.text.contains("Unsupported image filter text evidence")
                && matches!(unit.locator, SourceLocator::Pdf { page: 1, .. })
        }));
        assert!(pdf_evidence
            .iter()
            .all(|unit| unit.kind != EvidenceKind::Generated));
        assert!(pdf_evidence
            .iter()
            .any(|unit| unit.kind == EvidenceKind::Image));

        assert!(!pipeline
            .lexical_index()
            .search("markdownneedle", 5)
            .unwrap()
            .is_empty());
        assert!(!pipeline
            .lexical_index()
            .search("plainneedle", 5)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .lexical_index()
            .search("captionneedle", 5)
            .unwrap()
            .is_empty());

        {
            let lexical_index = pipeline.lexical_index();
            let embed_client = CaptionKeywordEmbeddingClient;
            let retrieval_config = RetrievalConfig {
                dense_top_k: 0,
                bm25_top_k: 3,
                rrf_k: 60,
                ..RetrievalConfig::default()
            };
            let graph_config = GraphConfig {
                enabled: true,
                max_hops: 1,
                max_expanded_chunks: 5,
                max_neighbors_per_seed: 5,
                edge_types: vec![EdgeType::SectionContains],
                extraction: Default::default(),
                global_search: Default::default(),
            };
            let retrieval = RetrievalPipeline::new_with_graph(
                pipeline.hnsw(),
                &lexical_index,
                pipeline.store(),
                &embed_client,
                &retrieval_config,
                &graph_config,
            );

            let (graph_results, debug) = retrieval
                .search_filtered_with_debug("markdownneedle", Some(&markdown_id))
                .await
                .unwrap();
            let expanded_sibling = graph_results
                .iter()
                .find(|result| {
                    result.provenance.origin == RetrievalOrigin::GraphExpansion
                        && result
                            .evidence_units
                            .iter()
                            .any(|unit| unit.text.contains("Graphneighbor sibling evidence"))
                })
                .expect("section seed should graph-expand to sibling evidence");
            assert!(expanded_sibling
                .provenance
                .graph_path
                .iter()
                .any(|step| step.edge_type == EdgeType::SectionContains));
            assert!(!debug.graph_expanded_hits.is_empty());
        }

        fs::write(&plaintext_path, "freshplainneedle replacement paragraph\n").unwrap();
        pipeline.ingest_source(&plaintext_id).await.unwrap();
        assert!(pipeline
            .lexical_index()
            .search("oldplainneedle", 5)
            .unwrap()
            .is_empty());
        assert!(!pipeline
            .lexical_index()
            .search("freshplainneedle", 5)
            .unwrap()
            .is_empty());

        pipeline.remove_source(&plaintext_id).await.unwrap();
        assert!(pipeline
            .store()
            .get_source(&plaintext_id)
            .unwrap()
            .is_none());
        assert!(pipeline
            .lexical_index()
            .search("freshplainneedle", 5)
            .unwrap()
            .is_empty());
    }

    fn image_limit_error(err: &anyhow::Error) -> &ImageArtifactLimitError {
        err.downcast_ref::<ImageArtifactLimitError>()
            .expect("error should preserve structured image limit type")
    }

    fn stream_object(prefix: &[u8], data: &[u8]) -> Vec<u8> {
        let mut object = prefix.to_vec();
        object.extend(format!(" /Length {} >>\nstream\n", data.len()).as_bytes());
        object.extend(data);
        object.extend(b"\nendstream");
        object
    }

    fn pdf_bytes(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (idx, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n", idx + 1).as_bytes());
            pdf.extend(object);
            pdf.extend(b"\nendobj\n");
        }
        let xref_offset = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn unsafe_source_ids_map_to_safe_artifact_directories() {
        let tempdir = tempfile::tempdir().unwrap();
        let data_dir = tempdir.path();
        let root = image_artifacts_root_dir(data_dir);

        for raw in ["..", ".", "---"] {
            let source_id = SourceId(raw.to_string());
            let source_dir = source_image_artifact_dir(data_dir, &source_id).unwrap();

            assert!(source_dir.starts_with(&root));
            assert_ne!(source_dir, data_dir);
            assert_ne!(source_dir, root);
            assert_eq!(source_dir.parent(), Some(root.as_path()));
            assert!(source_dir
                .file_name()
                .and_then(|name| name.to_str())
                .expect("safe component should be UTF-8")
                .starts_with("source-"));
        }
    }

    #[test]
    fn unsafe_source_cleanup_preserves_artifact_root_and_siblings() {
        let tempdir = tempfile::tempdir().unwrap();
        let data_dir = tempdir.path();
        let root = image_artifacts_root_dir(data_dir);
        let sibling_file = root.join("safe-sibling").join("keep.bin");
        let data_file = data_dir.join("verbatim.db");
        fs::create_dir_all(sibling_file.parent().unwrap()).unwrap();
        fs::write(&sibling_file, b"keep").unwrap();
        fs::write(&data_file, b"db").unwrap();

        for raw in ["..", ".", "---"] {
            let source_id = SourceId(raw.to_string());
            let source_dir = source_image_artifact_dir(data_dir, &source_id).unwrap();
            fs::create_dir_all(&source_dir).unwrap();
            fs::write(source_dir.join("owned.bin"), b"owned").unwrap();

            remove_source_image_artifacts(data_dir, &source_id).unwrap();

            assert!(!source_dir.exists());
            assert!(data_dir.exists());
            assert!(root.exists());
            assert!(sibling_file.exists());
            assert!(data_file.exists());

            fs::create_dir_all(&source_dir).unwrap();
            fs::write(source_dir.join("stale.bin"), b"stale").unwrap();

            cleanup_stale_source_image_artifacts(data_dir, &source_id, &[]).unwrap();

            assert!(!source_dir.exists());
            assert!(data_dir.exists());
            assert!(root.exists());
            assert!(sibling_file.exists());
            assert!(data_file.exists());
        }
    }

    #[test]
    fn unsafe_source_image_artifact_writes_stay_in_source_subtree() {
        let tempdir = tempfile::tempdir().unwrap();
        let data_dir = tempdir.path();
        let root = image_artifacts_root_dir(data_dir);

        for raw in ["..", ".", "---"] {
            let source_id = SourceId(raw.to_string());
            let prepared = prepare_image_artifacts(
                data_dir,
                &source_id,
                0,
                vec![test_parsed_image_artifact("png")],
                ImageArtifactLimits::default(),
            )
            .unwrap();

            assert_eq!(prepared.files.len(), 1);
            assert_eq!(prepared.artifacts.len(), 1);
            let source_dir = source_image_artifact_dir(data_dir, &source_id).unwrap();
            let artifact_path = &prepared.files[0].absolute_path;
            assert!(artifact_path.starts_with(&source_dir));
            assert_eq!(artifact_path.parent(), Some(source_dir.as_path()));
            assert_ne!(artifact_path, data_dir);
            assert_ne!(artifact_path, &root);
            assert!(prepared.artifacts[0]
                .relative_path
                .starts_with(Path::new(IMAGE_ARTIFACTS_DIR)));

            write_image_artifact_files(&prepared.files, ImageArtifactLimits::default()).unwrap();

            assert!(artifact_path.exists());
            assert!(source_dir.exists());
        }
    }

    #[test]
    fn prepare_image_artifacts_rejects_too_many_images() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_id = SourceId("src-1".to_string());
        let limits = ImageArtifactLimits {
            max_images_per_source: 1,
            ..ImageArtifactLimits::default()
        };

        let err = prepare_image_artifacts(
            tempdir.path(),
            &source_id,
            0,
            vec![
                test_parsed_image_artifact_with("png", 1, 1, vec![1], 1, 1),
                test_parsed_image_artifact_with("png", 1, 2, vec![2], 1, 1),
            ],
            limits,
        )
        .unwrap_err();

        assert!(matches!(
            image_limit_error(&err),
            ImageArtifactLimitError::TooManyImages {
                stage: ImageArtifactLimitStage::Prepare,
                limit: 1,
                attempted: 2,
                page: 1,
                image_index: 2
            }
        ));
    }

    #[test]
    fn prepare_image_artifacts_rejects_per_image_byte_limit() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_id = SourceId("src-1".to_string());
        let limits = ImageArtifactLimits {
            max_bytes_per_image: 3,
            ..ImageArtifactLimits::default()
        };

        let err = prepare_image_artifacts(
            tempdir.path(),
            &source_id,
            0,
            vec![test_parsed_image_artifact_with(
                "png",
                1,
                1,
                vec![1, 2, 3, 4],
                2,
                2,
            )],
            limits,
        )
        .unwrap_err();

        assert!(matches!(
            image_limit_error(&err),
            ImageArtifactLimitError::ImageBytesExceeded {
                stage: ImageArtifactLimitStage::Prepare,
                limit: 3,
                actual: 4,
                page: 1,
                image_index: 1
            }
        ));
    }

    #[test]
    fn prepare_image_artifacts_rejects_total_artifact_byte_limit() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_id = SourceId("src-1".to_string());
        let limits = ImageArtifactLimits {
            max_total_bytes_per_source: 3,
            ..ImageArtifactLimits::default()
        };

        let err = prepare_image_artifacts(
            tempdir.path(),
            &source_id,
            0,
            vec![
                test_parsed_image_artifact_with("png", 1, 1, vec![1, 2], 1, 1),
                test_parsed_image_artifact_with("png", 1, 2, vec![3, 4], 1, 1),
            ],
            limits,
        )
        .unwrap_err();

        assert!(matches!(
            image_limit_error(&err),
            ImageArtifactLimitError::TotalBytesExceeded {
                stage: ImageArtifactLimitStage::Prepare,
                limit: 3,
                attempted_total: 4,
                page: 1,
                image_index: 2
            }
        ));
    }

    #[test]
    fn prepare_image_artifacts_rejects_pixel_limit() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_id = SourceId("src-1".to_string());
        let limits = ImageArtifactLimits {
            max_image_pixels: 3,
            ..ImageArtifactLimits::default()
        };

        let err = prepare_image_artifacts(
            tempdir.path(),
            &source_id,
            0,
            vec![test_parsed_image_artifact_with("png", 1, 1, vec![1], 2, 2)],
            limits,
        )
        .unwrap_err();

        assert!(matches!(
            image_limit_error(&err),
            ImageArtifactLimitError::ImageDimensionsExceeded {
                stage: ImageArtifactLimitStage::Prepare,
                max_pixels: 3,
                width: 2,
                height: 2,
                pixels: 4,
                page: 1,
                image_index: 1,
                ..
            }
        ));
    }

    #[test]
    fn write_image_artifact_files_rejects_total_budget_before_writing() {
        let tempdir = tempfile::tempdir().unwrap();
        let first_path = tempdir.path().join("first.png");
        let second_path = tempdir.path().join("second.png");
        let files = vec![
            PreparedImageFile {
                absolute_path: first_path.clone(),
                bytes: vec![1, 2],
                content_hash: "first".into(),
                page: 1,
                image_index: 1,
            },
            PreparedImageFile {
                absolute_path: second_path.clone(),
                bytes: vec![3, 4],
                content_hash: "second".into(),
                page: 1,
                image_index: 2,
            },
        ];
        let limits = ImageArtifactLimits {
            max_total_bytes_per_source: 3,
            ..ImageArtifactLimits::default()
        };

        let err = write_image_artifact_files(&files, limits).unwrap_err();

        assert!(matches!(
            image_limit_error(&err),
            ImageArtifactLimitError::TotalBytesExceeded {
                stage: ImageArtifactLimitStage::Write,
                limit: 3,
                attempted_total: 4,
                page: 1,
                image_index: 2
            }
        ));
        assert!(!first_path.exists());
        assert!(!second_path.exists());
    }

    #[test]
    fn graph_links_image_to_parser_provided_nearby_text() {
        let tempdir = tempfile::tempdir().unwrap();
        let source = test_source("src-near-image", tempdir.path().join("near.pdf"));
        let text = "Nearby graph text before image.";
        let text_evidence = EvidenceUnit {
            id: EvidenceId("text-1".into()),
            source_id: source.id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::legacy_pdf(1, 0, None),
            text: text.into(),
            text_hash: "hash-text-1".into(),
            heading_path: Vec::new(),
            language: None,
            position: 0,
        };
        let mut parsed_image = test_parsed_image_artifact("png");
        parsed_image.nearby_text_before = Some(text.into());
        let prepared = prepare_image_artifacts(
            tempdir.path(),
            &source.id,
            1,
            vec![parsed_image],
            ImageArtifactLimits::default(),
        )
        .unwrap();
        let mut evidence = vec![text_evidence.clone()];
        evidence.extend(prepared.evidence.clone());
        let chunk_output = chunk_evidence(&source.id, &evidence, &ChunkerConfig::default());

        let (_nodes, edges) = build_evidence_graph(
            &source,
            &evidence,
            &chunk_output.chunks,
            &chunk_output.links,
            &prepared.artifacts,
            &prepared.text_proximities,
        );

        let image_node_id = GraphNodeId::new(
            &source.id,
            GraphNodeKind::ImageArtifact,
            &prepared.artifacts[0].image_id.0,
        );
        let text_node_id =
            GraphNodeId::new(&source.id, GraphNodeKind::EvidenceUnit, &text_evidence.id.0);
        assert!(edges.iter().any(|edge| {
            edge.edge_type == EdgeType::ImageNearText
                && edge.from_node_id == image_node_id
                && edge.to_node_id == text_node_id
        }));
    }

    #[tokio::test]
    async fn remove_source_cleans_lexical_and_dense_state() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk =
            insert_source_with_child_text(&store, &first, "chunk-1", "deletedterm").unwrap();
        let second_chunk =
            insert_source_with_child_text(&store, &second, "chunk-2", "retainedterm").unwrap();
        let chunks = [first_chunk, second_chunk];
        store_vectors_for_chunks(&store, &chunks);
        let hnsw = hnsw_with_chunks(&chunks);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        pipeline.remove_source(&first.id).await.unwrap();

        assert!(pipeline.store().get_source(&first.id).unwrap().is_none());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert!(pipeline
            .lexical_index()
            .search("deletedterm", 5)
            .unwrap()
            .is_empty());
        let retained = pipeline.lexical_index().search("retainedterm", 5).unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].0 .0, "chunk-2");
        assert_eq!(pipeline.store().list_vector_documents().unwrap().len(), 1);
        assert_eq!(pipeline.hnsw().len(), 1);
    }

    #[tokio::test]
    async fn ingest_all_batches_small_sources_across_embedding_requests() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let embed_client = DelayedRecordingEmbeddingClient::new();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            embed_client.clone(),
            tempdir.path().to_path_buf(),
        )
        .with_embedding_controls(2, 2);
        let task_id = TaskId("task-cross-source-batch".into());
        pipeline
            .store()
            .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
            .unwrap();

        for index in 0..4 {
            let path = tempdir.path().join(format!("small-{index}.txt"));
            fs::write(&path, format!("small source {index} retrieval text\n")).unwrap();
            let source = test_source(&format!("src-{index}"), path);
            pipeline.store().add_source(&source).unwrap();
        }

        let outcome = pipeline.ingest_all_with_task(true, &task_id).await.unwrap();

        assert_eq!(outcome.source_count, 4);
        assert_eq!(outcome.embedding_cache.embedded_chunks, 4);
        let mut call_sizes = embed_client.call_sizes();
        call_sizes.sort_unstable();
        assert_eq!(call_sizes, vec![2, 2]);
        assert_eq!(embed_client.max_in_flight(), 2);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&EmbeddingProfileId::default_profile(), None)
                .unwrap(),
            4
        );
        let events = pipeline
            .store()
            .list_task_events(&task_id, None, 100)
            .unwrap();
        assert!(events.iter().any(|event| {
            event.event_type == "progress"
                && event
                    .payload
                    .get("wait_reason")
                    .and_then(serde_json::Value::as_str)
                    == Some("embedding_throughput")
                && event
                    .payload
                    .get("recent_status")
                    .and_then(serde_json::Value::as_str)
                    == Some("waiting for embedding model throughput")
                && event.payload["counters"]
                    .as_array()
                    .is_some_and(|counters| {
                        counters.iter().any(|counter| {
                            counter["name"] == "embedding_batch_sources"
                                && counter["completed"] == 4
                        }) && counters.iter().any(|counter| {
                            counter["name"] == "max_vectors_per_request"
                                && counter["completed"] == 2
                        })
                    })
        }));
    }

    #[tokio::test]
    async fn ingest_all_indexes_json_source_as_searchable_text() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let source_path = tempdir.path().join("article.json");
        fs::write(
            &source_path,
            serde_json::json!({
                "title": "Cognitive arbitrage",
                "body": {
                    "summary": "JSON collection members should be searchable",
                    "tags": ["retrieval", "durable collections"]
                }
            })
            .to_string(),
        )
        .unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();

        let outcome = pipeline.ingest_all(false).await.unwrap();

        assert_eq!(outcome.source_count, 1);
        let source = pipeline.store().get_source(&source_id).unwrap().unwrap();
        assert_eq!(source.status, SourceStatus::Indexed);
        assert_eq!(source.parser_used.as_deref(), Some("json"));
        let evidence = pipeline
            .store()
            .list_evidence_by_source(&source_id)
            .unwrap();
        assert_eq!(evidence.len(), 4);
        assert!(evidence
            .iter()
            .any(|unit| unit.text.contains("Cognitive arbitrage")));
        assert!(!pipeline
            .lexical_index()
            .search("durable collections", 5)
            .unwrap()
            .is_empty());
        assert!(pipeline.check_stale().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ingest_all_prunes_missing_registered_source_without_failure() {
        let tempdir = tempfile::tempdir().unwrap();
        let missing_path = tempdir.path().join("removed.md");
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        pipeline
            .store()
            .add_source(&Source {
                id: SourceId("removed-md".into()),
                path: missing_path,
                hash: "old-hash".into(),
                status: SourceStatus::Stale,
                parser_used: Some("markdown".into()),
                last_ingested_at: None,
            })
            .unwrap();

        let outcome = pipeline.ingest_all(false).await.unwrap();

        assert_eq!(outcome.source_count, 0);
        assert_eq!(outcome.skipped_missing_sources, 1);
        assert!(pipeline
            .store()
            .get_source(&SourceId("removed-md".into()))
            .unwrap()
            .is_none());
        assert!(pipeline.check_stale().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_source_cleanup_preserves_source_on_metadata_error() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_path = tempdir.path().join("inaccessible.md");
        fs::write(&source_path, "source exists but metadata failed").unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let source_id = SourceId("inaccessible-md".into());
        pipeline
            .store()
            .add_source(&Source {
                id: source_id.clone(),
                path: source_path.clone(),
                hash: "old-hash".into(),
                status: SourceStatus::Stale,
                parser_used: Some("markdown".into()),
                last_ingested_at: None,
            })
            .unwrap();

        let error = pipeline
            .remove_missing_sources_for_all_source_ingest_with(None, |_| {
                Err(std::io::Error::new(ErrorKind::PermissionDenied, "metadata denied").into())
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("metadata denied"));
        assert!(pipeline.store().get_source(&source_id).unwrap().is_some());
    }

    #[tokio::test]
    async fn source_task_batch_progress_marks_commit_phases_after_embedding() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let task_id = TaskId("task-source-batch-commit-progress".into());
        let source_id = SourceId("src-source-batch-commit-progress".into());
        pipeline
            .store()
            .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
            .unwrap();
        pipeline.store().start_task(&task_id).unwrap();
        let path = tempdir.path().join("source-batch-commit-progress.txt");
        fs::write(&path, "source batch commit progress retrieval text\n").unwrap();
        let source = test_source(&source_id.0, path);
        pipeline.store().add_source(&source).unwrap();

        let outcomes = pipeline
            .ingest_sources_with_tasks(&[(source_id.clone(), task_id.clone())])
            .await;

        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].result.is_ok());
        let events = pipeline
            .store()
            .list_task_events(&task_id, None, 100)
            .unwrap();
        let phases = events
            .iter()
            .filter(|event| event.event_type == "progress")
            .filter_map(|event| event.payload["phase"]["name"].as_str())
            .collect::<Vec<_>>();

        assert!(
            phases.contains(&IngestTaskStage::SqliteWrite.as_str()),
            "progress phases should include sqlite commit, got {phases:?}"
        );
        assert!(
            phases.contains(&IngestTaskStage::VectorIndex.as_str()),
            "progress phases should include vector index publishing, got {phases:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event_type == "progress"
                    && event.payload["phase"]["name"] == IngestTaskStage::VectorIndex.as_str()
                    && event.payload["recent_status"] == "staging index artifacts"
                    && event.payload["resources"]
                        .as_array()
                        .is_some_and(|resources| {
                            resources.iter().any(|resource| {
                                resource["name"] == "index_publish"
                                    && resource["kind"] == "index_publish"
                                    && resource["state"] == "waiting"
                            })
                        })
            }),
            "progress events should report pending index_publish resource before acquiring the permit"
        );
        let completed_resources = events
            .iter()
            .filter(|event| event.event_type == "resource")
            .filter_map(|event| event.payload.get("resource"))
            .filter(|resource| resource["state"] == "completed")
            .collect::<Vec<_>>();
        assert!(
            completed_resources.len() >= 2,
            "expected multiple completed resource timing events, got {completed_resources:?}"
        );
        assert!(
            completed_resources.iter().any(|resource| {
                resource["name"] == "sqlite_writer"
                    && resource["kind"] == "sqlite_write"
                    && resource["queue_wait_ms"].is_u64()
                    && resource["service_ms"].is_u64()
            }),
            "completed resource timings should include sqlite_writer service time, got {completed_resources:?}"
        );
        assert!(
            completed_resources.iter().any(|resource| {
                resource["name"] == "index_publish"
                    && resource["kind"] == "index_publish"
                    && resource["queue_wait_ms"].is_u64()
                    && resource["service_ms"].is_u64()
            }),
            "completed resource timings should include index_publish service time, got {completed_resources:?}"
        );
    }

    #[test]
    fn task_metadata_writes_wait_for_sqlite_writer_resource() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let task_id = TaskId("task-resource-gated-metadata".into());
        pipeline
            .store()
            .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
            .unwrap();
        let resource = sqlite_writer_resource_for_test();

        assert_sqlite_writer_waits_for_test(&resource, || {
            pipeline.record_task_progress(
                Some(&task_id),
                TaskProgressSnapshot::phase("resource-gated-progress")
                    .with_recent_status("writer gated progress"),
            );
        });
        assert_sqlite_writer_waits_for_test(&resource, || {
            pipeline.record_task_event(
                Some(&task_id),
                "resource_test",
                "writer gated event",
                serde_json::json!({"source": "test"}),
            );
        });
        assert_sqlite_writer_waits_for_test(&resource, || {
            pipeline.record_task_phase(
                Some(&task_id),
                PhaseTiming::start("resource-gated-span"),
                serde_json::json!({"source": "test"}),
            );
        });

        let task = pipeline.store().get_task(&task_id).unwrap().unwrap();
        assert_eq!(
            task.progress.unwrap().recent_status.as_deref(),
            Some("writer gated progress")
        );
        let events = pipeline
            .store()
            .list_task_events(&task_id, None, 20)
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == "resource_test"));
        let spans = pipeline.store().list_task_spans(&task_id).unwrap();
        assert!(spans.iter().any(|span| span.phase == "resource-gated-span"));
    }

    #[test]
    fn telemetry_waits_for_sqlite_writer_before_non_sqlite_resource_acquire() {
        for (resource_name, resource_kind) in
            [("cpu_worker", "cpu"), ("index_publish", "index_publish")]
        {
            let writer = sqlite_writer_resource_for_test();
            let resource = named_resource_for_test(resource_name, resource_kind);
            let held_writer = writer.acquire_blocking().expect("held writer permit");
            let completed = Arc::new(AtomicUsize::new(0));
            let completed_in_thread = Arc::clone(&completed);

            let telemetry_then_resource = thread::spawn(move || {
                let _writer = acquire_ingest_resource_blocking("sqlite_writer", "sqlite_write")
                    .expect("telemetry waits for sqlite writer");
                let _resource = acquire_ingest_resource_blocking(resource_name, resource_kind)
                    .expect("resource acquired only after telemetry write admission");
                completed_in_thread.store(1, Ordering::Release);
            });

            thread::sleep(Duration::from_millis(25));
            let snapshot = resource.snapshot();
            assert_eq!(
                snapshot.active, 0,
                "{resource_name} should not be held while telemetry waits for sqlite_writer"
            );
            assert_eq!(
                snapshot.queued, 0,
                "{resource_name} should not queue while telemetry waits for sqlite_writer"
            );
            assert_eq!(completed.load(Ordering::Acquire), 0);

            drop(held_writer);
            telemetry_then_resource
                .join()
                .expect("telemetry/resource thread joins");
            assert_eq!(completed.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    fn profile_delete_waits_for_sqlite_writer_without_holding_index_publish() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let profile_id = EmbeddingProfileId::new("delete-me").unwrap();
        pipeline
            .store()
            .ensure_embedding_profile(
                &profile_id,
                test_embedding_profile_spec(2).as_store_config(),
            )
            .unwrap();
        let writer = sqlite_writer_resource_for_test();
        let index_publish = named_resource_for_test("index_publish", "index_publish");
        let before = writer.snapshot().queue_wait_ms_total;
        let held_writer = writer.acquire_blocking().expect("held writer permit");
        let delete_completed = Arc::new(AtomicUsize::new(0));
        let delete_completed_in_thread = Arc::clone(&delete_completed);
        let profile_id_for_thread = profile_id.clone();
        let delete_thread = thread::spawn(move || {
            let result =
                pipeline.delete_embedding_profile_index_data(&profile_id_for_thread, false);
            delete_completed_in_thread.store(1, Ordering::Release);
            result
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if writer.snapshot().queued == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "profile delete did not queue on sqlite_writer"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let publish_snapshot = index_publish.snapshot();
        assert_eq!(publish_snapshot.active, 0);
        assert_eq!(publish_snapshot.queued, 0);
        assert_eq!(delete_completed.load(Ordering::Acquire), 0);

        drop(held_writer);
        let (plan, report) = delete_thread
            .join()
            .expect("profile delete thread joins")
            .expect("profile delete succeeds");
        assert_eq!(plan.profile_id, profile_id.as_str());
        assert_eq!(report.removed_artifacts.len(), 0);
        assert_eq!(delete_completed.load(Ordering::Acquire), 1);
        assert!(
            writer.snapshot().queue_wait_ms_total > before,
            "profile delete should wait through sqlite_writer queue"
        );
    }

    #[tokio::test]
    async fn source_task_batch_reports_each_outcome_before_later_sources_complete() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let committed_sources = Arc::new(AtomicUsize::new(0));
        let observer_committed_sources = Arc::clone(&committed_sources);
        let reported_committed_counts = Arc::new(Mutex::new(Vec::new()));
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_embedding_controls(1, 1)
        .with_source_commit_observer(move |_store, _source_id| {
            observer_committed_sources.fetch_add(1, Ordering::SeqCst);
        });
        let mut source_tasks = Vec::new();
        for index in 0..2 {
            let task_id = TaskId(format!("task-source-batch-report-{index}"));
            let source_id = SourceId(format!("src-source-batch-report-{index}"));
            pipeline
                .store()
                .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
                .unwrap();
            pipeline.store().start_task(&task_id).unwrap();
            let path = tempdir
                .path()
                .join(format!("source-batch-report-{index}.txt"));
            fs::write(
                &path,
                format!("source batch report {index} retrieval text\n"),
            )
            .unwrap();
            let source = test_source(&source_id.0, path);
            pipeline.store().add_source(&source).unwrap();
            source_tasks.push((source_id, task_id));
        }

        let reported_counts = Arc::clone(&reported_committed_counts);
        let committed_sources_for_reporter = Arc::clone(&committed_sources);
        let outcomes = pipeline
            .ingest_sources_with_tasks_reporting(&source_tasks, move |outcome| {
                let reported_counts = Arc::clone(&reported_counts);
                let committed_sources = Arc::clone(&committed_sources_for_reporter);
                async move {
                    reported_counts
                        .lock()
                        .expect("reported counts lock should not be poisoned")
                        .push((outcome.task_id, committed_sources.load(Ordering::SeqCst)));
                }
            })
            .await;

        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|outcome| outcome.result.is_ok()));
        let reported_counts = reported_committed_counts
            .lock()
            .expect("reported counts lock should not be poisoned")
            .clone();
        assert_eq!(reported_counts.len(), 2);
        assert_eq!(reported_counts[0].0, source_tasks[0].1);
        assert_eq!(reported_counts[0].1, 1);
        assert_eq!(reported_counts[1].0, source_tasks[1].1);
        assert_eq!(reported_counts[1].1, 2);
    }

    #[test]
    fn pending_prepared_sources_flush_before_retaining_multiple_image_heavy_sources() {
        let mut pending = PendingPreparedSources::default();
        pending.push(prepared_source_with_image_bytes("src-image-1", 6));
        let next = prepared_source_with_image_bytes("src-image-2", 6);

        assert_eq!(pending.len(), 1);
        assert_eq!(pending.artifact_bytes(), 6);
        assert!(pending.should_flush_before_push(next.prepared_artifact_bytes(), 10));

        let flushed = pending.take();
        assert_eq!(flushed.len(), 1);
        assert!(pending.is_empty());
        pending.push(next);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.artifact_bytes(), 6);
    }

    #[tokio::test]
    async fn ingest_all_batch_cancellation_after_first_commit_skips_later_sources() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-cancel-after-first-source".into());
        let committed_sources = Arc::new(AtomicUsize::new(0));
        let observer_task_id = task_id.clone();
        let observer_committed_sources = Arc::clone(&committed_sources);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_source_commit_observer(move |store, _source_id| {
            if observer_committed_sources.fetch_add(1, Ordering::SeqCst) == 0 {
                store.cancel_task(&observer_task_id).unwrap();
            }
        });
        pipeline
            .store()
            .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
            .unwrap();
        for index in 0..2 {
            let path = tempdir.path().join(format!("cancel-{index}.txt"));
            fs::write(&path, format!("cancel source {index} retrieval text\n")).unwrap();
            let source = test_source(&format!("src-cancel-{index}"), path);
            pipeline.store().add_source(&source).unwrap();
        }

        let err = pipeline
            .ingest_all_with_task(true, &task_id)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("ingest task cancelled"));
        assert_eq!(committed_sources.load(Ordering::SeqCst), 1);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&EmbeddingProfileId::default_profile(), None)
                .unwrap(),
            1
        );
        assert_eq!(
            pipeline.store().task_status(&task_id).unwrap(),
            Some(TaskStatus::Cancelled)
        );
    }

    #[tokio::test]
    async fn source_task_batch_cancellation_after_first_commit_skips_later_sources() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first_task = TaskId("task-source-batch-first".into());
        let second_task = TaskId("task-source-batch-second".into());
        let second_task_for_observer = second_task.clone();
        let committed_sources = Arc::new(AtomicUsize::new(0));
        let observer_committed_sources = Arc::clone(&committed_sources);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_source_commit_observer(move |store, _source_id| {
            if observer_committed_sources.fetch_add(1, Ordering::SeqCst) == 0 {
                store.cancel_task(&second_task_for_observer).unwrap();
            }
        });
        let mut source_tasks = Vec::new();
        for (index, task_id) in [&first_task, &second_task].into_iter().enumerate() {
            pipeline
                .store()
                .create_task(task_id, TaskKind::Ingest, &serde_json::json!({}))
                .unwrap();
            pipeline.store().start_task(task_id).unwrap();
            let source_id = SourceId(format!("src-source-batch-{index}"));
            let path = tempdir.path().join(format!("source-batch-{index}.txt"));
            fs::write(&path, format!("source batch {index} retrieval text\n")).unwrap();
            let source = test_source(&source_id.0, path);
            pipeline.store().add_source(&source).unwrap();
            source_tasks.push((source_id, task_id.clone()));
        }

        let outcomes = pipeline.ingest_sources_with_tasks(&source_tasks).await;

        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].result.is_ok());
        assert!(outcomes[1]
            .result
            .as_ref()
            .unwrap_err()
            .contains("ingest task cancelled"));
        assert_eq!(committed_sources.load(Ordering::SeqCst), 1);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&EmbeddingProfileId::default_profile(), None)
                .unwrap(),
            1
        );
        assert_eq!(
            pipeline.store().task_status(&second_task).unwrap(),
            Some(TaskStatus::Cancelled)
        );
    }

    #[tokio::test]
    async fn source_task_batch_embedding_failure_only_fails_dependent_source() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            SelectiveFailingEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_embedding_controls(1, 2);
        let ok_task = TaskId("task-source-batch-ok".into());
        let fail_task = TaskId("task-source-batch-fail".into());
        let ok_source_id = SourceId("src-source-batch-ok".into());
        let fail_source_id = SourceId("src-source-batch-fail".into());
        for (source_id, task_id, text) in [
            (
                &ok_source_id,
                &ok_task,
                "successful source retrieval text\n",
            ),
            (
                &fail_source_id,
                &fail_task,
                "FAIL_EMBED source retrieval text\n",
            ),
        ] {
            pipeline
                .store()
                .create_task(task_id, TaskKind::Ingest, &serde_json::json!({}))
                .unwrap();
            pipeline.store().start_task(task_id).unwrap();
            let path = tempdir.path().join(format!("{}.txt", source_id.0));
            fs::write(&path, text).unwrap();
            let source = test_source(&source_id.0, path);
            pipeline.store().add_source(&source).unwrap();
        }

        let outcomes = pipeline
            .ingest_sources_with_tasks(&[
                (ok_source_id.clone(), ok_task.clone()),
                (fail_source_id.clone(), fail_task.clone()),
            ])
            .await;

        let ok_outcome = outcomes
            .iter()
            .find(|outcome| outcome.source_id == ok_source_id)
            .unwrap();
        let fail_outcome = outcomes
            .iter()
            .find(|outcome| outcome.source_id == fail_source_id)
            .unwrap();
        assert!(ok_outcome.result.is_ok());
        assert!(fail_outcome
            .result
            .as_ref()
            .unwrap_err()
            .contains("selective embedding unavailable"));
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&ok_source_id),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&fail_source_id),
                )
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn embedding_disabled_ingest_builds_lexical_index_without_embedding_calls() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let embedding = RecordingEmbeddingClient::new();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            embedding.clone(),
            tempdir.path().to_path_buf(),
        )
        .with_embedding_enabled(false);
        let source_path = tempdir.path().join("bm25-only.txt");
        fs::write(
            &source_path,
            "alpha lexical retrieval evidence for BM25-only ingest\n",
        )
        .unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();

        let cache_stats = pipeline.ingest_source(&source_id).await.unwrap();

        assert!(embedding.calls().is_empty());
        assert_eq!(cache_stats.embedded_chunks, 0);
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&source_id),
                )
                .unwrap(),
            0
        );

        let lexical_index = pipeline.lexical_index();
        let retrieval_config = RetrievalConfig {
            dense_top_k: 4,
            bm25_top_k: 4,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let retrieval = RetrievalPipeline::new(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &embedding,
            &retrieval_config,
        )
        .with_embedding_enabled(false);
        let (results, debug) = retrieval
            .search_source_set_with_debug("alpha BM25 ingest", None)
            .await
            .unwrap();

        assert!(results
            .iter()
            .any(|result| result.chunk.source_id == source_id));
        assert!(debug.dense_hits.is_empty());
        assert_eq!(debug.query_embedding_latency_ms, None);
        assert!(!debug.bm25_hits.is_empty());
    }

    #[tokio::test]
    async fn ingest_all_embedding_failure_commits_independent_sources_in_flush() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            SelectiveFailingEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_embedding_controls(1, 2);
        let task_id = TaskId("task-ingest-all-partial-embedding-failure".into());
        let ok_source_id = SourceId("src-ingest-all-ok".into());
        let fail_source_id = SourceId("src-ingest-all-fail".into());
        pipeline
            .store()
            .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
            .unwrap();
        for (source_id, text) in [
            (&ok_source_id, "successful all-source retrieval text\n"),
            (&fail_source_id, "FAIL_EMBED all-source retrieval text\n"),
        ] {
            let path = tempdir.path().join(format!("{}.txt", source_id.0));
            fs::write(&path, text).unwrap();
            let source = test_source(&source_id.0, path);
            pipeline.store().add_source(&source).unwrap();
        }

        let err = pipeline
            .ingest_all_with_task(true, &task_id)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("selective embedding unavailable"));
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&ok_source_id),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&fail_source_id),
                )
                .unwrap(),
            0
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn remove_source_deletes_qdrant_points_by_source_after_commit() {
        let (qdrant_url, handle) = spawn_qdrant_server(vec![
            (200, r#"{"status":"ok","result":{}}"#),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":1}}"#,
            ),
        ]);
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let chunks = [first_chunk, second_chunk];
        store_vectors_for_chunks(&store, &chunks);
        let hnsw = hnsw_with_chunks(&chunks);
        let qdrant = QdrantClient::new(qdrant_test_config(qdrant_url));
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_qdrant_client(qdrant);

        pipeline.remove_source(&first.id).await.unwrap();

        let requests = handle.join().unwrap();
        assert_eq!(requests[0].line, "GET /collections/verbatim HTTP/1.1");
        assert_eq!(
            requests[1].line,
            "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        );
        let body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["filter"]["must"][0]["key"], "source_id");
        assert_eq!(body["filter"]["must"][0]["match"]["value"], "src-1");
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn reconcile_deletions_retries_pending_qdrant_erasure_after_unavailability() {
        let (qdrant_url, handle) = spawn_qdrant_server(vec![
            (200, r#"{"status":"ok","result":{}}"#),
            (
                503,
                r#"{"status":{"error":"temporarily unavailable"},"result":null}"#,
            ),
            (200, r#"{"status":"ok","result":{}}"#),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":2}}"#,
            ),
        ]);
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-reconcile", tempdir.path().join("reconcile.txt"));
        let chunk = insert_source_with_child(&store, &source, "chunk-reconcile").unwrap();
        store_vectors_for_chunks(&store, &[chunk.clone()]);
        let qdrant = QdrantClient::new(qdrant_test_config(qdrant_url));
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw_with_chunks(&[chunk]),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_qdrant_client(qdrant);

        let initial = pipeline.remove_source(&source.id).await.unwrap();
        assert!(pipeline.store().get_source(&source.id).unwrap().is_none());
        assert_eq!(
            initial.status_for(DeletionProduct::Qdrant),
            Some(DeletionOutcome::Pending),
        );

        let reconciled = pipeline.reconcile_deletions().await.unwrap();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(
            reconciled[0].status_for(DeletionProduct::Qdrant),
            Some(DeletionOutcome::Erased),
        );
        assert!(pipeline
            .store()
            .pending_qdrant_deletion_source_ids()
            .unwrap()
            .is_empty());

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[1].line,
            "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        );
        assert_eq!(
            requests[3].line,
            "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn force_ingest_rewrites_active_qdrant_profile_after_local_ingest() {
        let (qdrant_url, handle) = spawn_qdrant_server(vec![
            (404, r#"{"status":{"error":"missing"},"result":null}"#),
            (404, r#"{"status":{"error":"missing"},"result":null}"#),
            (200, r#"{"status":"ok","result":true}"#),
            (
                200,
                r#"{"status":"ok","result":{"config":{"params":{"vectors":{"size":2,"distance":"Cosine"}}},"payload_schema":{}}}"#,
            ),
            (200, r#"{"status":"ok","result":true}"#),
            (200, r#"{"status":"ok","result":true}"#),
            (200, QDRANT_COLLECTION_INFO),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":1}}"#,
            ),
            (200, QDRANT_COLLECTION_INFO),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":2}}"#,
            ),
            (200, QDRANT_COLLECTION_INFO),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":3}}"#,
            ),
        ]);
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("force.txt");
        std::fs::write(&path, "force qdrant sync paragraph\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-force", path);
        store.add_source(&source).unwrap();
        let qdrant = QdrantClient::new(qdrant_test_config(qdrant_url));
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_qdrant_client(qdrant);

        let ingested = pipeline.ingest_all(true).await.unwrap();

        assert_eq!(ingested.source_count, 1);
        let requests = handle.join().unwrap();
        assert_eq!(
            requests[9].line,
            "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        );
        let delete_body: serde_json::Value = serde_json::from_str(&requests[9].body).unwrap();
        assert_eq!(delete_body["filter"]["must"][0]["key"], "profile_id");
        assert_eq!(
            delete_body["filter"]["must"][0]["match"]["value"],
            "default"
        );
        assert_eq!(
            requests[11].line,
            "PUT /collections/verbatim/points?wait=true HTTP/1.1"
        );
        let body: serde_json::Value = serde_json::from_str(&requests[11].body).unwrap();
        assert_eq!(body["points"][0]["payload"]["profile_id"], "default");
        assert_eq!(body["points"][0]["payload"]["source_id"], "src-force");
        assert!(body["points"][0]["payload"]["chunk_id"]
            .as_str()
            .unwrap()
            .contains("src-force"));
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn vectors_only_profile_build_syncs_qdrant_with_profile_filter() {
        let (qdrant_url, handle) = spawn_qdrant_server(vec![
            (200, QDRANT_COLLECTION_INFO),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":1}}"#,
            ),
            (200, QDRANT_COLLECTION_INFO),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":2}}"#,
            ),
        ]);
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-alt", tempdir.path().join("alt.txt"));
        insert_source_with_child_text(&store, &source, "chunk-alt", "alt profile text").unwrap();
        let qdrant = QdrantClient::new(qdrant_test_config(qdrant_url));
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_qdrant_client(qdrant);
        let alt_profile = EmbeddingProfileId::new("alt").unwrap();

        let count = pipeline
            .build_embedding_profile(&alt_profile, None)
            .await
            .unwrap();

        assert_eq!(count.source_count, 1);
        let requests = handle.join().unwrap();
        assert_eq!(
            requests[1].line,
            "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        );
        let delete_body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(delete_body["filter"]["must"][0]["key"], "profile_id");
        assert_eq!(delete_body["filter"]["must"][0]["match"]["value"], "alt");
        assert_eq!(
            requests[3].line,
            "PUT /collections/verbatim/points?wait=true HTTP/1.1"
        );
        let upsert_body: serde_json::Value = serde_json::from_str(&requests[3].body).unwrap();
        assert_eq!(upsert_body["points"][0]["payload"]["profile_id"], "alt");
        assert_eq!(upsert_body["points"][0]["payload"]["source_id"], "src-alt");
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn source_scoped_profile_reset_full_syncs_qdrant_and_filters_stale_remote_hits() {
        let (qdrant_url, handle) = spawn_qdrant_server(vec![
            (200, QDRANT_COLLECTION_INFO),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":1}}"#,
            ),
            (200, QDRANT_COLLECTION_INFO),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":2}}"#,
            ),
            (
                200,
                r#"{"status":"ok","result":[{"score":0.99,"payload":{"chunk_id":"chunk-stale-remote"}}]}"#,
            ),
        ]);
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-reset-first", tempdir.path().join("first.txt"));
        let second = test_source("src-reset-second", tempdir.path().join("second.txt"));
        let first_chunk =
            insert_source_with_child_text(&store, &first, "chunk-reset-fresh", "alpha fresh")
                .unwrap();
        let second_chunk =
            insert_source_with_child_text(&store, &second, "chunk-stale-remote", "alpha stale")
                .unwrap();
        let alt_profile = EmbeddingProfileId::new("alt-reset").unwrap();
        store
            .ensure_embedding_profile(
                &alt_profile,
                EmbeddingProfileConfig {
                    requested_model: Some("old-embedding"),
                    ..crate::store::tests::test_profile_config(
                        "test",
                        "test-embedding",
                        2,
                        true,
                        "",
                        "",
                    )
                },
            )
            .unwrap();
        store
            .replace_all_vector_documents_for_profile(
                &alt_profile,
                &[
                    VectorDocument {
                        chunk_id: first_chunk.id.clone(),
                        source_id: first.id.clone(),
                        vector: vec![1.0, 0.0],
                    },
                    VectorDocument {
                        chunk_id: second_chunk.id.clone(),
                        source_id: second.id.clone(),
                        vector: vec![0.0, 1.0],
                    },
                ],
            )
            .unwrap();
        let qdrant = QdrantClient::new(qdrant_test_config(qdrant_url.clone()));
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_qdrant_client(qdrant);

        pipeline
            .build_embedding_profile(&alt_profile, Some(&first.id))
            .await
            .unwrap();

        let mut hnsw = HnswIndex::new();
        hnsw.rebuild_from_store_for_profile(pipeline.store(), &alt_profile)
            .unwrap();
        let lexical_index = SqliteFtsIndex::new(pipeline.store());
        let mut qdrant_search = qdrant_test_config(qdrant_url);
        qdrant_search.prefer_for_search = true;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let embed_client = StaticEmbeddingClient;
        let results = RetrievalPipeline::new(
            &hnsw,
            &lexical_index,
            pipeline.store(),
            &embed_client,
            &retrieval_config,
        )
        .require_embedding_profile(&alt_profile)
        .with_qdrant_search(&qdrant_search)
        .search("alpha")
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, first_chunk.id);
        assert!(results
            .iter()
            .all(|result| result.chunk_id != second_chunk.id));
        let requests = handle.join().unwrap();
        assert_eq!(
            requests[1].line,
            "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        );
        let delete_body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(delete_body["filter"]["must"][0]["key"], "profile_id");
        assert_eq!(
            delete_body["filter"]["must"][0]["match"]["value"],
            "alt-reset"
        );
        assert_eq!(
            requests[3].line,
            "PUT /collections/verbatim/points?wait=true HTTP/1.1"
        );
        let upsert_body: serde_json::Value = serde_json::from_str(&requests[3].body).unwrap();
        assert_eq!(upsert_body["points"].as_array().unwrap().len(), 1);
        assert_eq!(
            upsert_body["points"][0]["payload"]["source_id"],
            "src-reset-first"
        );
        assert_eq!(
            requests[4].line,
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn ingest_source_replaces_stale_lexical_and_dense_state() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.txt");
        std::fs::write(&path, "freshterm text for ingest\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        let old_chunk =
            insert_source_with_child_text(&store, &source, "old-chunk", "obsoleteword").unwrap();
        let hnsw = hnsw_with_chunks(&[old_chunk]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        assert!(pipeline
            .lexical_index()
            .search("obsoleteword", 5)
            .unwrap()
            .is_empty());
        assert!(!pipeline
            .lexical_index()
            .search("freshterm", 5)
            .unwrap()
            .is_empty());
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|chunk| chunk.id.0 != "old-chunk"));
        let vectors = pipeline.store().list_vector_documents().unwrap();
        assert_eq!(
            vectors.len(),
            chunks
                .iter()
                .filter(|c| c.chunk_type == ChunkType::Child)
                .count()
        );
        assert!(vectors
            .iter()
            .all(|vector| vector.chunk_id.0 != "old-chunk"));
    }

    #[tokio::test]
    async fn low_memory_ingest_does_not_publish_hnsw_artifact_but_keeps_sqlite_vectors() {
        assert_ingest_vector_artifact_residency(VectorIndexResidency::LowMemory, false).await;
    }

    #[tokio::test]
    async fn resident_hnsw_ingest_publishes_hnsw_artifact() {
        assert_ingest_vector_artifact_residency(VectorIndexResidency::ResidentHnsw, true).await;
    }

    async fn assert_ingest_vector_artifact_residency(
        residency: VectorIndexResidency,
        expect_hnsw_artifact: bool,
    ) {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.txt");
        std::fs::write(&path, "alpha vector artifact regression\n").unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        pipeline.vector_residency = residency;
        let source_id = pipeline.add_source(&path).unwrap();
        let profile = EmbeddingProfileId::default_profile();

        pipeline.ingest_source(&source_id).await.unwrap();

        let generation = pipeline
            .store()
            .index_generation_for_profile(&profile)
            .unwrap();
        let manifest = read_index_manifest(tempdir.path(), &profile)
            .unwrap()
            .expect("published index manifest");
        assert_eq!(manifest.generation, generation);
        let hnsw_path =
            index_generation_dir(tempdir.path(), &profile, generation).join("vectors.hnsw");
        assert_eq!(hnsw_path.exists(), expect_hnsw_artifact);
        let vectors = pipeline
            .store()
            .list_vector_documents_for_profile(&profile)
            .unwrap();
        assert!(!vectors.is_empty());
        let hits = pipeline
            .store()
            .search_vector_documents_for_profile(&profile, &[1.0, 0.0], 5, None)
            .unwrap();
        assert!(!hits.is_empty());
        match residency {
            VectorIndexResidency::LowMemory => assert!(pipeline.hnsw().is_empty()),
            VectorIndexResidency::ResidentHnsw => assert_eq!(pipeline.hnsw().len(), vectors.len()),
        }
    }

    #[tokio::test]
    async fn markdown_reingest_reuses_cached_vectors_after_insertion_and_deletion() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.md");
        let original = "# Alpha\n\nAlpha body.\n\n# Stable\n\nStable body.\n";
        let inserted =
            "# Inserted\n\nInserted body.\n\n# Alpha\n\nAlpha body.\n\n# Stable\n\nStable body.\n";
        std::fs::write(&path, original).unwrap();
        let store = Store::in_memory().unwrap();
        let embedding = RecordingEmbeddingClient::new();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            embedding.clone(),
            tempdir.path().to_path_buf(),
        );
        let source_id = pipeline.add_source(&path).unwrap();

        let first = pipeline.ingest_source(&source_id).await.unwrap();
        let first_chunks = pipeline.store().list_chunks_by_source(&source_id).unwrap();
        let first_alpha = child_chunk_for_heading(&first_chunks, "Alpha");
        let first_stable = child_chunk_for_heading(&first_chunks, "Stable");

        assert_eq!(first.cache_hits, 0);
        assert_eq!(first.cache_misses, 2);
        assert_eq!(first.embedded_chunks, 2);
        assert_eq!(embedding.calls().len(), 1);
        let first_cleanup = pipeline.store().vector_json_cleanup_dry_run().unwrap();
        assert_eq!(first_cleanup.tables.chunk_vectors.eligible, 0);
        assert_eq!(first_cleanup.tables.chunk_vectors.already_clean, 2);
        assert_eq!(first_cleanup.tables.embedding_cache.eligible, 0);
        assert_eq!(first_cleanup.tables.embedding_cache.already_clean, 2);

        std::fs::write(&path, inserted).unwrap();
        let second = pipeline.ingest_source(&source_id).await.unwrap();
        let second_chunks = pipeline.store().list_chunks_by_source(&source_id).unwrap();
        let second_alpha = child_chunk_for_heading(&second_chunks, "Alpha");
        let second_stable = child_chunk_for_heading(&second_chunks, "Stable");

        assert_eq!(second.cache_hits, 2);
        assert_eq!(second.cache_misses, 1);
        assert_eq!(second.reused_chunks, 2);
        assert_eq!(second.changed_chunks, 1);
        assert_eq!(second.embedded_chunks, 1);
        assert_eq!(first_alpha.id, second_alpha.id);
        assert_eq!(first_alpha.chunk_hash, second_alpha.chunk_hash);
        assert_eq!(first_stable.id, second_stable.id);
        assert_eq!(first_stable.chunk_hash, second_stable.chunk_hash);
        assert_eq!(embedding.calls().len(), 2);

        std::fs::write(&path, original).unwrap();
        let third = pipeline.ingest_source(&source_id).await.unwrap();

        assert_eq!(third.cache_hits, 2);
        assert_eq!(third.cache_misses, 0);
        assert_eq!(third.reused_chunks, 2);
        assert_eq!(third.embedded_chunks, 0);
        assert_eq!(embedding.calls().len(), 2);
        let cleanup = pipeline.store().cleanup_vector_json_payloads().unwrap();
        assert_eq!(cleanup.cleared.chunk_vectors, 0);
        assert_eq!(cleanup.cleared.embedding_cache, 0);

        let lexical_index = pipeline.lexical_index();
        let retrieval_config = RetrievalConfig::default();
        let retrieval = RetrievalPipeline::new(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &embedding,
            &retrieval_config,
        );
        let (results, _debug) = retrieval
            .search_source_set_with_debug("Alpha body", None)
            .await
            .unwrap();
        assert!(results
            .iter()
            .any(|result| result.chunk.source_id == source_id));
    }

    #[tokio::test]
    async fn unchanged_reingest_skips_vector_index_publish() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.md");
        std::fs::write(&path, "# Alpha\n\nAlpha body.\n").unwrap();
        let store = Store::in_memory().unwrap();
        let embedding = RecordingEmbeddingClient::new();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            embedding.clone(),
            tempdir.path().to_path_buf(),
        );
        let source_id = pipeline.add_source(&path).unwrap();
        let first_task = TaskId("task-noop-first".into());
        pipeline
            .store()
            .create_task(
                &first_task,
                TaskKind::Ingest,
                &serde_json::json!({ "source_id": source_id.0 }),
            )
            .unwrap();
        pipeline.store().start_task(&first_task).unwrap();
        let first = pipeline
            .ingest_source_with_task(&source_id, &first_task)
            .await
            .unwrap();
        assert_eq!(first.changed_chunks, 1);
        let generation_after_first = pipeline.store().index_generation().unwrap();

        let second_task = TaskId("task-noop-second".into());
        pipeline
            .store()
            .create_task(
                &second_task,
                TaskKind::Ingest,
                &serde_json::json!({ "source_id": source_id.0 }),
            )
            .unwrap();
        pipeline.store().start_task(&second_task).unwrap();
        let second = pipeline
            .ingest_source_with_task(&source_id, &second_task)
            .await
            .unwrap();

        assert_eq!(second.cache_hits, 1);
        assert_eq!(second.cache_misses, 0);
        assert_eq!(second.reused_chunks, 1);
        assert_eq!(second.changed_chunks, 0);
        assert_eq!(second.embedded_chunks, 0);
        assert_eq!(
            pipeline.store().index_generation().unwrap(),
            generation_after_first
        );
        assert_eq!(embedding.calls().len(), 1);
        let second_spans = pipeline.store().list_task_spans(&second_task).unwrap();
        assert!(!second_spans
            .iter()
            .any(|span| span.phase == IngestTaskStage::VectorIndex.as_str()));
        let second_events = pipeline
            .store()
            .list_task_events(&second_task, None, 100)
            .unwrap();
        assert!(second_events.iter().any(|event| {
            event.event_type == "noop"
                && event.payload["operation"] == "source_ingest_noop"
                && event.payload["vector_index_published"] == false
        }));
    }

    #[tokio::test]
    async fn unchanged_reingest_skips_embedding_cache_vector_payload_read() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.md");
        std::fs::write(&path, "# Alpha\n\nAlpha body.\n").unwrap();
        let store = Store::in_memory().unwrap();
        let embedding = RecordingEmbeddingClient::new();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            embedding.clone(),
            tempdir.path().to_path_buf(),
        );
        let source_id = pipeline.add_source(&path).unwrap();

        let first = pipeline.ingest_source(&source_id).await.unwrap();
        assert_eq!(first.cache_misses, 1);
        assert_eq!(embedding.calls().len(), 1);
        let generation_after_first = pipeline.store().index_generation().unwrap();
        let corrupted_rows = pipeline
            .store()
            .connection()
            .execute(
                "UPDATE embedding_cache SET vector_blob = X'01', vector_json = ''",
                [],
            )
            .unwrap();
        assert_eq!(corrupted_rows, 1);

        let second = pipeline.ingest_source(&source_id).await.unwrap();

        assert_eq!(second.cache_hits, 1);
        assert_eq!(second.cache_misses, 0);
        assert_eq!(second.reused_chunks, 1);
        assert_eq!(second.embedded_chunks, 0);
        assert_eq!(embedding.calls().len(), 1);
        assert_eq!(
            pipeline.store().index_generation().unwrap(),
            generation_after_first
        );
    }

    #[tokio::test]
    async fn cancelled_unchanged_reingest_observes_cancellation_before_noop() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.md");
        std::fs::write(&path, "# Alpha\n\nAlpha body.\n").unwrap();
        let store = Store::in_memory().unwrap();
        let embedding = RecordingEmbeddingClient::new();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            embedding.clone(),
            tempdir.path().to_path_buf(),
        );
        let source_id = pipeline.add_source(&path).unwrap();

        let first = pipeline.ingest_source(&source_id).await.unwrap();
        assert_eq!(first.cache_misses, 1);
        assert_eq!(embedding.calls().len(), 1);
        let generation_after_first = pipeline.store().index_generation().unwrap();

        let second_task = TaskId("task-cancel-unchanged-noop".into());
        pipeline
            .store()
            .create_task(
                &second_task,
                TaskKind::Ingest,
                &serde_json::json!({ "source_id": source_id.0 }),
            )
            .unwrap();
        pipeline.store().start_task(&second_task).unwrap();
        pipeline.store().cancel_task(&second_task).unwrap();

        let error = pipeline
            .ingest_source_with_task(&source_id, &second_task)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("ingest task cancelled"));
        assert_eq!(embedding.calls().len(), 1);
        assert_eq!(
            pipeline.store().index_generation().unwrap(),
            generation_after_first
        );
        let second_events = pipeline
            .store()
            .list_task_events(&second_task, None, 100)
            .unwrap();
        assert!(second_events
            .iter()
            .any(|event| event.event_type == "cancelled"));
        assert!(!second_events.iter().any(|event| {
            event.event_type == "noop" && event.payload["operation"] == "source_ingest_noop"
        }));
    }

    fn child_chunk_for_heading<'a>(chunks: &'a [Chunk], heading: &str) -> &'a Chunk {
        chunks
            .iter()
            .find(|chunk| {
                chunk.chunk_type == ChunkType::Child
                    && chunk.heading_path == vec![heading.to_string()]
            })
            .expect("child chunk for heading")
    }

    #[tokio::test]
    async fn ingest_source_builds_graph_and_reingest_replaces_stale_nodes() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("graph.md");
        let fresh_paragraph = format!("Fresh graph paragraph {}.", "alpha ".repeat(360));
        let stale_paragraph = format!("Stale graph paragraph {}.", "beta ".repeat(360));
        std::fs::write(
            &path,
            format!("# Intro\n\n{fresh_paragraph}\n\n{stale_paragraph}\n"),
        )
        .unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-graph", path.clone());
        store.add_source(&source).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        let first_nodes = pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap();
        assert!(first_nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::Source));
        assert!(first_nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::Section));
        assert!(first_nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::Chunk));
        assert!(first_nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::EvidenceUnit));
        let child_edges = pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::Child)
            .unwrap();
        let source_node = first_nodes
            .iter()
            .find(|node| node.kind == GraphNodeKind::Source)
            .unwrap();
        assert!(child_edges
            .iter()
            .any(|edge| edge.from_node_id == source_node.id));
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        let child_chunk_node_ids = chunks
            .iter()
            .filter(|chunk| chunk.chunk_type == ChunkType::Child)
            .map(|chunk| GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &chunk.id.0).0)
            .collect::<HashSet<_>>();
        let parent_chunk_node_ids = chunks
            .iter()
            .filter(|chunk| chunk.chunk_type == ChunkType::Parent)
            .map(|chunk| GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &chunk.id.0).0)
            .collect::<HashSet<_>>();
        assert!(child_chunk_node_ids.len() >= 2);
        assert!(child_edges.iter().any(|edge| {
            parent_chunk_node_ids.contains(&edge.from_node_id.0)
                && child_chunk_node_ids.contains(&edge.to_node_id.0)
        }));
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::Parent)
            .unwrap()
            .iter()
            .any(|edge| {
                child_chunk_node_ids.contains(&edge.from_node_id.0)
                    && parent_chunk_node_ids.contains(&edge.to_node_id.0)
            }));
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::Next)
            .unwrap()
            .iter()
            .any(|edge| {
                child_chunk_node_ids.contains(&edge.from_node_id.0)
                    && child_chunk_node_ids.contains(&edge.to_node_id.0)
            }));
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::Previous)
            .unwrap()
            .iter()
            .any(|edge| {
                child_chunk_node_ids.contains(&edge.from_node_id.0)
                    && child_chunk_node_ids.contains(&edge.to_node_id.0)
            }));
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::SectionContains)
            .unwrap()
            .iter()
            .any(|edge| child_chunk_node_ids.contains(&edge.to_node_id.0)));
        let same_source_edges = pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::SameSource)
            .unwrap();
        assert!(same_source_edges.len() <= first_nodes.len().saturating_sub(1));
        assert!(same_source_edges.iter().all(|edge| {
            !(child_chunk_node_ids.contains(&edge.from_node_id.0)
                && child_chunk_node_ids.contains(&edge.to_node_id.0))
        }));
        let stale_evidence = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .find(|unit| unit.text.contains("Stale graph paragraph"))
            .unwrap();
        let stale_node_id = GraphNodeId::new(
            &source.id,
            GraphNodeKind::EvidenceUnit,
            &stale_evidence.id.0,
        );

        std::fs::write(&path, format!("# Intro\n\n{fresh_paragraph}\n")).unwrap();
        pipeline.ingest_source(&source.id).await.unwrap();

        assert!(pipeline
            .store()
            .get_evidence(&stale_evidence.id)
            .unwrap()
            .is_none());
        let second_nodes = pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap();
        assert!(second_nodes.iter().all(|node| node.id != stale_node_id));
        let second_edges = pipeline
            .store()
            .list_graph_edges_by_source(&source.id)
            .unwrap();
        let second_node_ids = sorted_graph_node_ids(&second_nodes);
        let second_edge_ids = sorted_graph_edge_ids(&second_edges);

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(
            sorted_graph_node_ids(
                &pipeline
                    .store()
                    .list_graph_nodes_by_source(&source.id)
                    .unwrap()
            ),
            second_node_ids
        );
        assert_eq!(
            sorted_graph_edge_ids(
                &pipeline
                    .store()
                    .list_graph_edges_by_source(&source.id)
                    .unwrap()
            ),
            second_edge_ids
        );

        pipeline.remove_source(&source.id).await.unwrap();

        assert!(pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .store()
            .list_graph_edges_by_source(&source.id)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn graph_extraction_disabled_does_not_call_chat_model() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("graph-extraction-disabled.md");
        std::fs::write(&path, "Feature A supports Component B.\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-graph-extraction-disabled", path);
        store.add_source(&source).unwrap();
        let chat_model = MockGraphChatModel::succeeds();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_graph_extractor(
            GraphExtractionConfig::default(),
            GraphExtractor::from_chat_model(Arc::new(chat_model.clone())),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(chat_model.call_count(), 0);
        assert!(pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap()
            .iter()
            .all(|node| node.kind != GraphNodeKind::GeneratedEntity
                && node.kind != GraphNodeKind::GeneratedClaim));
    }

    #[tokio::test]
    async fn graph_extraction_stores_generated_rows_with_provenance_separate_from_deterministic_edges(
    ) {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("graph-extraction.md");
        std::fs::write(&path, "Feature A supports Component B.\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-graph-extraction", path);
        store.add_source(&source).unwrap();
        let chat_model = MockGraphChatModel::succeeds();
        let config = GraphExtractionConfig {
            enabled: true,
            max_chunks: 1,
            max_retries: 0,
            ..GraphExtractionConfig::default()
        };
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_graph_extractor(
            config,
            GraphExtractor::from_chat_model(Arc::new(chat_model.clone())),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(chat_model.call_count(), 1);
        let nodes = pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap();
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node.kind == GraphNodeKind::GeneratedEntity)
                .count(),
            2
        );
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node.kind == GraphNodeKind::GeneratedClaim)
                .count(),
            1
        );
        assert!(nodes
            .iter()
            .filter(|node| {
                node.kind == GraphNodeKind::GeneratedEntity
                    || node.kind == GraphNodeKind::GeneratedClaim
            })
            .all(|node| node
                .metadata
                .as_ref()
                .and_then(|value| value.get("origin"))
                .and_then(serde_json::Value::as_str)
                == Some("llm_generated")));

        let generated_edges = pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::GeneratedSupports)
            .unwrap();
        assert_eq!(generated_edges.len(), 1);
        assert_eq!(generated_edges[0].weight, Some(0.75));
        assert_eq!(
            generated_edges[0]
                .metadata
                .as_ref()
                .and_then(|value| value.get("source_spans"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );

        let deterministic_child_edges = pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::Child)
            .unwrap();
        assert!(!deterministic_child_edges.is_empty());
        assert!(deterministic_child_edges.iter().all(|edge| edge
            .metadata
            .as_ref()
            .and_then(|value| value.get("origin"))
            .and_then(serde_json::Value::as_str)
            != Some("llm_generated")));
    }

    #[tokio::test]
    async fn graph_extraction_provider_failure_keeps_deterministic_ingest() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("graph-extraction-provider-failure.md");
        std::fs::write(&path, "Feature A supports Component B.\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-graph-extraction-provider-failure", path);
        store.add_source(&source).unwrap();
        let chat_model = MockGraphChatModel::fails();
        let config = GraphExtractionConfig {
            enabled: true,
            max_chunks: 1,
            max_retries: 0,
            ..GraphExtractionConfig::default()
        };
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_graph_extractor(
            config,
            GraphExtractor::from_chat_model(Arc::new(chat_model.clone())),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(chat_model.call_count(), 1);
        let nodes = pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap();
        assert!(nodes.iter().any(|node| node.kind == GraphNodeKind::Chunk));
        assert!(nodes
            .iter()
            .all(|node| node.kind != GraphNodeKind::GeneratedEntity
                && node.kind != GraphNodeKind::GeneratedClaim));
        assert!(!pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::Child)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn pdf_ingest_writes_stable_image_artifacts_and_removes_them_with_source() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[255u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        let first_artifacts = pipeline
            .store()
            .list_image_artifacts_by_source(&source.id)
            .unwrap();
        assert_eq!(first_artifacts.len(), 1);
        let first = &first_artifacts[0];
        assert_eq!(first.page, 1);
        assert_eq!(first.image_index, 1);
        assert_eq!(first.mime_type, "image/png");
        let first_image_id = first.image_id.clone();
        let first_relative_path = first.relative_path.clone();
        let first_artifact_path = tempdir.path().join(&first_relative_path);
        assert!(first_artifact_path.exists());
        assert_eq!(
            pipeline
                .store()
                .get_image_artifact_by_evidence(&first.evidence_id)
                .unwrap()
                .as_ref()
                .map(|artifact| &artifact.image_id),
            Some(&first_image_id)
        );
        let image_evidence = pipeline
            .store()
            .get_evidence(&first.evidence_id)
            .unwrap()
            .expect("image evidence should be stored");
        assert_eq!(image_evidence.kind, EvidenceKind::Image);
        assert!(matches!(
            image_evidence.locator,
            SourceLocator::PdfImage { .. }
        ));
        let image_evidence_node_id = GraphNodeId::new(
            &source.id,
            GraphNodeKind::EvidenceUnit,
            &first.evidence_id.0,
        );
        let image_artifact_node_id =
            GraphNodeId::new(&source.id, GraphNodeKind::ImageArtifact, &first.image_id.0);
        let page_node_id = GraphNodeId::new(&source.id, GraphNodeKind::Page, "page:1");
        let graph_nodes = pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap();
        assert!(graph_nodes
            .iter()
            .any(|node| node.id == image_evidence_node_id));
        assert!(graph_nodes
            .iter()
            .any(|node| node.id == image_artifact_node_id));
        assert!(graph_nodes.iter().any(|node| node.id == page_node_id));
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::DerivedFrom)
            .unwrap()
            .iter()
            .any(|edge| {
                edge.from_node_id == image_evidence_node_id
                    && edge.to_node_id == image_artifact_node_id
            }));
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::PageContainsImage)
            .unwrap()
            .iter()
            .any(|edge| {
                edge.from_node_id == page_node_id && edge.to_node_id == image_artifact_node_id
            }));
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::ImageNearText)
            .unwrap()
            .is_empty());

        pipeline.ingest_source(&source.id).await.unwrap();

        let second_artifacts = pipeline
            .store()
            .list_image_artifacts_by_source(&source.id)
            .unwrap();
        assert_eq!(second_artifacts.len(), 1);
        assert_eq!(second_artifacts[0].image_id, first_image_id);
        assert_eq!(second_artifacts[0].relative_path, first_relative_path);
        let artifact_files =
            fs::read_dir(source_image_artifact_dir(tempdir.path(), &source.id).unwrap())
                .unwrap()
                .count();
        assert_eq!(artifact_files, 1);

        pipeline.remove_source(&source.id).await.unwrap();

        assert!(pipeline.store().get_source(&source.id).unwrap().is_none());
        assert!(pipeline
            .store()
            .list_image_artifacts_by_source(&source.id)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .store()
            .list_graph_edges_by_source(&source.id)
            .unwrap()
            .is_empty());
        assert!(!source_image_artifact_dir(tempdir.path(), &source.id)
            .unwrap()
            .exists());
    }

    #[tokio::test]
    async fn pdf_image_caption_success_reuses_cache_without_persistent_evidence() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[255u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let vision = MockVisionModel::new(vec![valid_caption_json("An indexing flow diagram.")]);
        let vision_calls = vision.clone();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(vision_calls.call_count(), 1);
        let caption_records = pipeline.store().list_image_captions().unwrap();
        assert_eq!(caption_records.len(), 1);
        assert_eq!(caption_records[0].status, ImageCaptionStatus::Success);
        assert_eq!(caption_records[0].model, "local-vision");
        assert_eq!(caption_records[0].prompt_hash, vision_caption_prompt_hash());
        assert_eq!(caption_records[0].attempt_count, 1);
        assert_eq!(caption_records[0].cache_hits, 0);
        let evidence = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap();
        assert!(evidence
            .iter()
            .all(|unit| unit.kind != EvidenceKind::Generated));
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        assert!(chunks
            .iter()
            .all(|chunk| !chunk.text.contains("An indexing flow diagram.")));
        let vector_documents = pipeline.store().list_vector_documents().unwrap();
        assert!(vector_documents.iter().all(|document| {
            chunks
                .iter()
                .find(|chunk| chunk.id == document.chunk_id)
                .is_some_and(|chunk| !chunk.text.contains("An indexing flow diagram."))
        }));
        let graph_nodes = pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap();
        let graph_json = serde_json::to_string(&graph_nodes).unwrap();
        assert!(!graph_json.contains("An indexing flow diagram."));

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(vision_calls.call_count(), 1);
        let caption_records = pipeline.store().list_image_captions().unwrap();
        assert_eq!(caption_records.len(), 1);
        assert_eq!(caption_records[0].cache_hits, 1);
        let evidence_after_cache = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|unit| unit.kind == EvidenceKind::Generated)
            .collect::<Vec<_>>();
        assert!(evidence_after_cache.is_empty());
        let caption_chunks_after_cache = pipeline
            .store()
            .list_chunks_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|chunk| chunk.text.contains("An indexing flow diagram."))
            .collect::<Vec<_>>();
        assert!(caption_chunks_after_cache.is_empty());
    }

    #[tokio::test]
    async fn pdf_image_caption_disabled_preserves_successful_cache() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[255u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let vision = MockVisionModel::new(vec![valid_caption_json("An indexing flow diagram.")]);
        let vision_calls = vision.clone();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        pipeline.ingest_source(&source.id).await.unwrap();
        let cached = pipeline.store().list_image_captions().unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].status, ImageCaptionStatus::Success);
        assert!(cached[0].caption.is_some());
        assert!(cached[0].raw_response.is_some());

        pipeline.vision_model = None;
        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(vision_calls.call_count(), 1);
        let after_disabled = pipeline.store().list_image_captions().unwrap();
        assert_eq!(after_disabled.len(), 1);
        assert_eq!(after_disabled[0].status, ImageCaptionStatus::Success);
        assert_eq!(after_disabled[0].caption, cached[0].caption);
        assert_eq!(after_disabled[0].raw_response, cached[0].raw_response);
        assert_eq!(after_disabled[0].attempt_count, cached[0].attempt_count);
        assert_eq!(after_disabled[0].cache_hits, cached[0].cache_hits);
        let generated_after_disabled = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|unit| unit.kind == EvidenceKind::Generated)
            .collect::<Vec<_>>();
        assert!(generated_after_disabled.is_empty());
        assert!(pipeline
            .store()
            .list_chunks_by_source(&source.id)
            .unwrap()
            .iter()
            .all(|chunk| !chunk.text.contains("An indexing flow diagram.")));
    }

    #[tokio::test]
    async fn image_caption_cache_has_no_lexical_or_dense_search_output() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("caption-search.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[7u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-caption-search", path);
        store.add_source(&source).unwrap();
        let vision = MockVisionModel::new(vec![valid_caption_json(
            "A captionneedle indexing flow diagram.",
        )]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            CaptionKeywordEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        pipeline.ingest_source(&source.id).await.unwrap();

        let evidence = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap();
        assert!(evidence.iter().any(|unit| {
            unit.kind == EvidenceKind::Text && unit.text.contains("Unsupported image filter text")
        }));
        assert!(evidence.iter().all(|unit| {
            unit.kind != EvidenceKind::Generated && !unit.text.contains("captionneedle")
        }));
        assert!(pipeline
            .store()
            .list_image_captions()
            .unwrap()
            .iter()
            .any(|record| record.status == ImageCaptionStatus::Success));
        assert!(pipeline
            .lexical_index()
            .search("captionneedle", 5)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .hnsw()
            .search(&[1.0, 0.0], 5)
            .iter()
            .all(|(chunk_id, _)| pipeline
                .store()
                .get_chunk(chunk_id)
                .unwrap()
                .is_none_or(|chunk| !chunk.text.contains("captionneedle"))));
        assert!(pipeline
            .store()
            .list_vector_documents()
            .unwrap()
            .iter()
            .all(|document| pipeline
                .store()
                .get_chunk(&document.chunk_id)
                .unwrap()
                .is_none_or(|chunk| !chunk.text.contains("captionneedle"))));

        let lexical_index = pipeline.lexical_index();
        let embed_client = CaptionKeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let retrieval = RetrievalPipeline::new(
            pipeline.hnsw(),
            &lexical_index,
            pipeline.store(),
            &embed_client,
            &retrieval_config,
        );
        let retrieval_results = retrieval.search("captionneedle").await.unwrap();
        assert!(retrieval_results.iter().all(|result| {
            !result.chunk.text.contains("captionneedle")
                && result.evidence_units.iter().all(|unit| {
                    unit.kind != EvidenceKind::Generated && !unit.text.contains("captionneedle")
                })
        }));
    }

    #[tokio::test]
    async fn image_caption_cache_is_replaced_on_reingest_and_survives_source_removal() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("caption-cleanup.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[11u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-caption-cleanup", path.clone());
        store.add_source(&source).unwrap();
        let vision = MockVisionModel::new(vec![
            valid_caption_json("A stalecaptionneedle diagram."),
            valid_caption_json("A freshcaptionneedle diagram."),
        ]);
        let vision_calls = vision.clone();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            CaptionKeywordEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        pipeline.ingest_source(&source.id).await.unwrap();
        assert_eq!(vision_calls.call_count(), 1);
        assert!(pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .iter()
            .all(|unit| unit.kind != EvidenceKind::Generated));
        assert!(pipeline
            .lexical_index()
            .search("stalecaptionneedle", 5)
            .unwrap()
            .is_empty());

        write_pdf_with_text_and_image_filter(&path, None, &[22u8; 8 * 8 * 3]);
        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(vision_calls.call_count(), 2);
        assert!(pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .iter()
            .all(|unit| unit.kind != EvidenceKind::Generated));
        assert!(pipeline
            .lexical_index()
            .search("stalecaptionneedle", 5)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .lexical_index()
            .search("freshcaptionneedle", 5)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .store()
            .list_vector_documents()
            .unwrap()
            .iter()
            .all(|document| pipeline
                .store()
                .get_chunk(&document.chunk_id)
                .unwrap()
                .is_none_or(|chunk| {
                    !chunk.text.contains("stalecaptionneedle")
                        && !chunk.text.contains("freshcaptionneedle")
                })));

        pipeline.remove_source(&source.id).await.unwrap();

        assert!(pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn pdf_image_caption_repairs_malformed_json_once() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[255u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let vision = MockVisionModel::new(vec![
            "not json".to_string(),
            valid_caption_json("A repaired diagram caption."),
        ]);
        let vision_calls = vision.clone();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(vision_calls.call_count(), 2);
        let caption_records = pipeline.store().list_image_captions().unwrap();
        assert_eq!(caption_records.len(), 1);
        assert_eq!(caption_records[0].status, ImageCaptionStatus::Success);
        assert_eq!(caption_records[0].attempt_count, 2);
        assert_eq!(
            caption_records[0]
                .caption
                .as_ref()
                .map(|caption| caption.short_caption.as_str()),
            Some("A repaired diagram caption."),
        );
        assert!(pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .iter()
            .all(|unit| unit.kind != EvidenceKind::Generated));
    }

    #[tokio::test]
    async fn pdf_image_caption_records_repair_failure_without_aborting_ingest() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[255u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let vision = MockVisionModel::new(vec![
            "not json".to_string(),
            r#"{"type":"photo"}"#.to_string(),
        ]);
        let vision_calls = vision.clone();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(vision_calls.call_count(), 2);
        let stored_source = pipeline
            .store()
            .get_source(&source.id)
            .unwrap()
            .expect("source should remain indexed");
        assert_eq!(stored_source.status, SourceStatus::Indexed);
        assert_eq!(
            pipeline
                .store()
                .list_image_artifacts_by_source(&source.id)
                .unwrap()
                .len(),
            1
        );
        let caption_records = pipeline.store().list_image_captions().unwrap();
        assert_eq!(caption_records.len(), 1);
        assert_eq!(caption_records[0].status, ImageCaptionStatus::Failed);
        assert_eq!(caption_records[0].attempt_count, 2);
        assert!(caption_records[0]
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("caption JSON repair failed"));
        let generated = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|unit| unit.kind == EvidenceKind::Generated)
            .count();
        assert_eq!(generated, 0);
    }

    #[tokio::test]
    async fn pdf_image_caption_disabled_records_skip_and_keeps_text_ingest() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("text-and-image.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[255u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        let evidence = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap();
        assert!(evidence.iter().any(|unit| {
            unit.kind == EvidenceKind::Text
                && unit.text.contains("Unsupported image filter text evidence")
        }));
        assert!(evidence
            .iter()
            .all(|unit| unit.kind != EvidenceKind::Generated));
        let caption_records = pipeline.store().list_image_captions().unwrap();
        assert_eq!(caption_records.len(), 1);
        assert_eq!(caption_records[0].status, ImageCaptionStatus::Skipped);
        assert!(caption_records[0]
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("disabled or not configured"));
    }

    #[tokio::test]
    async fn pdf_ingest_skips_unsupported_image_filter_and_keeps_text_evidence() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("unsupported-image-filter.pdf");
        write_pdf_with_text_and_image_filter(
            &path,
            Some("FlateDecode"),
            &[120, 156, 3, 0, 0, 0, 0, 1],
        );
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        let evidence = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap();
        assert!(evidence.iter().any(|unit| {
            unit.kind == EvidenceKind::Text
                && unit.text.contains("Unsupported image filter text evidence")
        }));
        assert!(pipeline
            .store()
            .list_image_artifacts_by_source(&source.id)
            .unwrap()
            .is_empty());
        assert!(!source_image_artifact_dir(tempdir.path(), &source.id)
            .unwrap()
            .exists());
    }

    #[tokio::test]
    async fn pdf_ingest_keeps_image_resource_limits_hard() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-limit.pdf");
        write_pdf_with_text_and_image_filter(&path, None, &[255u8; 8 * 8 * 3]);
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        store.add_source(&source).unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        pipeline.image_artifact_limits = ImageArtifactLimits {
            max_images_per_source: 0,
            ..ImageArtifactLimits::default()
        };

        let err = pipeline.ingest_source(&source.id).await.unwrap_err();

        assert!(matches!(
            image_limit_error(&err),
            ImageArtifactLimitError::TooManyImages {
                stage: ImageArtifactLimitStage::Parser,
                limit: 0,
                attempted: 1,
                page: 1,
                image_index: 1
            }
        ));
        assert!(pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .store()
            .list_image_artifacts_by_source(&source.id)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn remove_unknown_source_does_not_cleanup_image_artifacts() {
        let tempdir = tempfile::tempdir().unwrap();
        let data_dir = tempdir.path();
        let root = image_artifacts_root_dir(data_dir);
        let sibling_file = root.join("safe-sibling").join("keep.bin");
        fs::create_dir_all(sibling_file.parent().unwrap()).unwrap();
        fs::write(&sibling_file, b"keep").unwrap();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            data_dir.to_path_buf(),
        );

        let err = pipeline
            .remove_source(&SourceId("---".to_string()))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("source not found"));
        assert!(data_dir.exists());
        assert!(root.exists());
        assert!(sibling_file.exists());
    }

    #[tokio::test]
    async fn remove_source_publishes_dense_index_when_image_cleanup_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let chunks = [first_chunk, second_chunk];
        store_vectors_for_chunks(&store, &chunks);
        let hnsw = hnsw_with_chunks(&chunks);
        write_blocking_image_artifact_path(tempdir.path(), &first.id);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let report = pipeline.remove_source(&first.id).await.unwrap();

        assert_eq!(
            report.status_for(DeletionProduct::Images),
            Some(DeletionOutcome::Pending),
        );
        assert!(pipeline.store().get_source(&first.id).unwrap().is_none());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert_eq!(pipeline.store().list_vector_documents().unwrap().len(), 1);
        assert_eq!(pipeline.hnsw().len(), 1);
        let results = pipeline.hnsw().search(&[1.0, 0.0], 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0 .0, "chunk-2");
    }

    #[tokio::test]
    async fn ingest_source_publishes_dense_index_when_stale_image_cleanup_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.txt");
        std::fs::write(&path, "freshterm text for ingest\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        let old_chunk = insert_source_with_child(&store, &source, "old-chunk").unwrap();
        let hnsw = hnsw_with_chunks(&[old_chunk]);
        write_blocking_image_artifact_path(tempdir.path(), &source.id);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let err = pipeline.ingest_source(&source.id).await.unwrap_err();

        assert!(err
            .to_string()
            .contains("cleanup stale image artifacts after committed source ingest"));
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|chunk| chunk.id.0 != "old-chunk"));
        let vectors = pipeline.store().list_vector_documents().unwrap();
        assert!(!vectors.is_empty());
        assert!(vectors
            .iter()
            .all(|vector| vector.chunk_id.0 != "old-chunk"));
        assert_eq!(pipeline.hnsw().len(), vectors.len());
        let results = pipeline.hnsw().search(&[1.0, 0.0], vectors.len() + 1);
        assert_eq!(results.len(), vectors.len());
        assert!(results
            .iter()
            .all(|(chunk_id, _score)| chunk_id.0 != "old-chunk"));
    }

    #[tokio::test]
    async fn remove_source_does_not_call_embedding_client_and_keeps_remaining_vectors() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let chunks = [first_chunk, second_chunk];
        store_vectors_for_chunks(&store, &chunks);
        let hnsw = hnsw_with_chunks(&chunks);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            FailingEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        pipeline.remove_source(&first.id).await.unwrap();

        assert!(pipeline.store().get_source(&first.id).unwrap().is_none());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert_eq!(pipeline.store().list_child_chunks().unwrap().len(), 1);
        assert_eq!(pipeline.store().list_vector_documents().unwrap().len(), 1);
        assert_eq!(pipeline.hnsw().len(), 1);
    }

    #[tokio::test]
    async fn ingest_source_keeps_existing_rows_when_embedding_rebuild_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("doc.txt");
        std::fs::write(&path, "new text for ingest\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        let old_chunk = insert_source_with_child(&store, &source, "old-chunk").unwrap();
        let hnsw = hnsw_with_chunks(&[old_chunk]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            FailingEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let err = pipeline.ingest_source(&source.id).await.unwrap_err();

        assert!(err.to_string().contains("embedding unavailable"));
        assert!(pipeline.store().get_source(&source.id).unwrap().is_some());
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id.0, "old-chunk");
        assert_eq!(pipeline.hnsw().len(), 1);
    }

    #[tokio::test]
    async fn remove_source_keeps_store_when_index_publication_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let blocked_data_dir = tempfile::NamedTempFile::new().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let chunks = [first_chunk, second_chunk];
        store_vectors_for_chunks(&store, &chunks);
        let hnsw = hnsw_with_chunks(&chunks);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            blocked_data_dir.path().to_path_buf(),
        );

        let err = pipeline.remove_source(&first.id).await.unwrap_err();

        assert!(err.to_string().contains("create index staging dir"));
        assert!(pipeline.store().get_source(&first.id).unwrap().is_some());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert_eq!(pipeline.store().list_child_chunks().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn ingest_source_keeps_existing_rows_when_index_publication_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let blocked_data_dir = tempfile::NamedTempFile::new().unwrap();
        let path = tempdir.path().join("doc.txt");
        std::fs::write(&path, "new text for ingest\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        let old_chunk = insert_source_with_child(&store, &source, "old-chunk").unwrap();
        let hnsw = hnsw_with_chunks(&[old_chunk]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            blocked_data_dir.path().to_path_buf(),
        );

        let err = pipeline.ingest_source(&source.id).await.unwrap_err();

        assert!(err.to_string().contains("create index staging dir"));
        assert!(pipeline.store().get_source(&source.id).unwrap().is_some());
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id.0, "old-chunk");
        assert_eq!(pipeline.hnsw().len(), 1);
    }

    #[tokio::test]
    async fn remove_source_ignores_unmanifested_indexes_when_manifest_write_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let default_profile = EmbeddingProfileId::default_profile();
        let manifest_tmp_path =
            index_manifest_path(tempdir.path(), &default_profile).with_extension("json.tmp");
        std::fs::create_dir_all(manifest_tmp_path.parent().unwrap()).unwrap();
        std::fs::create_dir(&manifest_tmp_path).unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let chunks = [first_chunk, second_chunk];
        store_vectors_for_chunks(&store, &chunks);
        let hnsw = hnsw_with_chunks(&chunks);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let before_generation = pipeline.store().index_generation().unwrap();

        let err = pipeline.remove_source(&first.id).await.unwrap_err();

        assert!(err.to_string().contains("write index manifest temp"));
        assert!(pipeline.store().get_source(&first.id).unwrap().is_none());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert_eq!(
            pipeline.store().index_generation().unwrap(),
            before_generation + 1
        );
        assert!(read_index_manifest(tempdir.path(), &default_profile)
            .unwrap()
            .is_none());
        let loaded_hnsw =
            load_published_vector_index(tempdir.path(), pipeline.store(), &default_profile)
                .unwrap();
        assert_eq!(loaded_hnsw.len(), 1);
        assert!(pipeline.hnsw().is_empty());
    }

    #[tokio::test]
    async fn ingest_source_ignores_unmanifested_indexes_when_manifest_write_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let default_profile = EmbeddingProfileId::default_profile();
        let manifest_tmp_path =
            index_manifest_path(tempdir.path(), &default_profile).with_extension("json.tmp");
        std::fs::create_dir_all(manifest_tmp_path.parent().unwrap()).unwrap();
        std::fs::create_dir(&manifest_tmp_path).unwrap();
        let path = tempdir.path().join("doc.txt");
        std::fs::write(&path, "new text for ingest\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        let old_chunk = insert_source_with_child(&store, &source, "old-chunk").unwrap();
        let hnsw = hnsw_with_chunks(&[old_chunk]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        let before_generation = pipeline.store().index_generation().unwrap();

        let err = pipeline.ingest_source(&source.id).await.unwrap_err();

        assert!(err.to_string().contains("write index manifest temp"));
        assert_eq!(
            pipeline.store().index_generation().unwrap(),
            before_generation + 1
        );
        assert!(read_index_manifest(tempdir.path(), &default_profile)
            .unwrap()
            .is_none());
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|chunk| chunk.id.0 != "old-chunk"));
        let loaded_hnsw =
            load_published_vector_index(tempdir.path(), pipeline.store(), &default_profile)
                .unwrap();
        assert_eq!(
            loaded_hnsw.len(),
            chunks
                .iter()
                .filter(|chunk| chunk.chunk_type == ChunkType::Child)
                .count()
        );
        assert!(pipeline.hnsw().is_empty());
    }

    #[test]
    fn post_publish_gc_removes_old_generations_after_successful_publish() {
        let tempdir = tempfile::tempdir().unwrap();
        let default_profile = EmbeddingProfileId::default_profile();
        let store = Store::in_memory().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );
        pipeline.index_gc_policy = IndexGcPolicy {
            retain_previous_generations: 0,
            stale_staging_age: Duration::from_secs(60),
        };

        pipeline
            .store()
            .replace_all_vector_documents_for_profile(&default_profile, &[])
            .unwrap();
        let first_generation = pipeline
            .store()
            .index_generation_for_profile(&default_profile)
            .unwrap();
        pipeline
            .publish_prepared_indexes(
                &default_profile,
                PreparedIndexes {
                    hnsw: HnswIndex::new(),
                    vectors: Vec::new(),
                    cache_stats: EmbeddingCacheStats::default(),
                    _memory_reservation: None,
                },
            )
            .unwrap();
        let first_generation_dir =
            index_generation_dir(tempdir.path(), &default_profile, first_generation);
        assert!(first_generation_dir.exists());

        pipeline
            .store()
            .replace_all_vector_documents_for_profile(&default_profile, &[])
            .unwrap();
        let second_generation = pipeline
            .store()
            .index_generation_for_profile(&default_profile)
            .unwrap();
        pipeline
            .publish_prepared_indexes(
                &default_profile,
                PreparedIndexes {
                    hnsw: HnswIndex::new(),
                    vectors: Vec::new(),
                    cache_stats: EmbeddingCacheStats::default(),
                    _memory_reservation: None,
                },
            )
            .unwrap();

        assert!(!first_generation_dir.exists());
        assert!(index_generation_dir(tempdir.path(), &default_profile, second_generation).exists());
    }

    #[tokio::test]
    async fn ingest_source_uses_stored_source_id_for_legacy_sources() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("legacy.txt");
        std::fs::write(&path, "new text for legacy ingest\n").unwrap();
        let store = Store::in_memory().unwrap();
        let legacy_source = test_source("legacy", path.clone());
        let old_chunk = insert_source_with_child(&store, &legacy_source, "old-chunk").unwrap();
        let hnsw = hnsw_with_chunks(&[old_chunk]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let path_derived_id = SourceId::from_path(&path);
        assert_ne!(path_derived_id, legacy_source.id);

        pipeline.ingest_source(&legacy_source.id).await.unwrap();

        assert!(pipeline
            .store()
            .get_source(&legacy_source.id)
            .unwrap()
            .is_some());
        assert!(pipeline
            .store()
            .get_source(&path_derived_id)
            .unwrap()
            .is_none());
        let evidence = pipeline
            .store()
            .list_evidence_by_source(&legacy_source.id)
            .unwrap();
        assert!(!evidence.is_empty());
        assert!(evidence
            .iter()
            .all(|unit| unit.source_id == legacy_source.id));
        let chunks = pipeline
            .store()
            .list_chunks_by_source(&legacy_source.id)
            .unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks
            .iter()
            .all(|chunk| chunk.source_id == legacy_source.id));
    }
}
