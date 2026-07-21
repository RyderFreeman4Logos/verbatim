use std::time::Instant;

use crate::evidence_spans::ChunkEvidenceSpan;
use crate::traits::VectorDocument;
use crate::types::{
    Chunk, ChunkId, EmbeddingProfileId, EvidenceId, EvidenceUnit, GraphEdge, GraphNode,
    ImageArtifact, Source,
};

use super::unix_timestamp_string;

pub struct SourceContentsReplacement<'a> {
    pub source: &'a Source,
    pub evidence: &'a [EvidenceUnit],
    pub chunks: &'a [Chunk],
    pub embedding_profile_id: &'a EmbeddingProfileId,
    pub vectors: &'a [VectorDocument],
    pub links: &'a [(ChunkId, EvidenceId)],
    pub evidence_spans: &'a [ChunkEvidenceSpan],
    pub image_artifacts: &'a [ImageArtifact],
    pub graph_nodes: &'a [GraphNode],
    pub graph_edges: &'a [GraphEdge],
}

/// Result of replacing one source's stored contents and derived indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceContentsReplacementReport {
    pub generation: u64,
    pub lexical_update: SourceLexicalIndexUpdate,
}

/// Bounded timing for SQLite FTS updates triggered by source content replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLexicalIndexUpdate {
    pub started_at: String,
    pub duration_ms: u64,
    pub deleted_child_chunks: usize,
    pub indexed_child_chunks: usize,
}

impl SourceLexicalIndexUpdate {
    pub(super) fn start(deleted_child_chunks: usize) -> Self {
        Self {
            started_at: unix_timestamp_string(),
            duration_ms: 0,
            deleted_child_chunks,
            indexed_child_chunks: 0,
        }
    }

    pub(super) fn add_elapsed_since(&mut self, started: Instant) {
        let elapsed_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.duration_ms = self.duration_ms.saturating_add(elapsed_ms);
    }
}
