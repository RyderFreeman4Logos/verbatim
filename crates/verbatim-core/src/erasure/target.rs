//! Inventory of backends and artifacts affected by a source erasure.

use serde::{Deserialize, Serialize};

/// A backend or artifact family that must participate in cross-backend erasure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionTarget {
    Sqlite,
    Tantivy,
    Hnsw,
    Qdrant,
    GraphNodes,
    GraphEdges,
    GraphReports,
    Blobs,
    Images,
    QueryCache,
    ContextCache,
    AnswerCache,
    Tasks,
    WorkflowArtifacts,
    Exports,
    TemporaryUploads,
}

impl DeletionTarget {
    /// Complete inventory. `Sqlite` covers source, evidence, chunks, vectors,
    /// and cache rows held by the authoritative local store.
    pub const ALL: [Self; 16] = [
        Self::Sqlite,
        Self::Tantivy,
        Self::Hnsw,
        Self::Qdrant,
        Self::GraphNodes,
        Self::GraphEdges,
        Self::GraphReports,
        Self::Blobs,
        Self::Images,
        Self::QueryCache,
        Self::ContextCache,
        Self::AnswerCache,
        Self::Tasks,
        Self::WorkflowArtifacts,
        Self::Exports,
        Self::TemporaryUploads,
    ];

    pub const fn is_remote_replica(self) -> bool {
        matches!(self, Self::Qdrant)
    }
}
