//! LexicalSearch, VectorSearch, and GraphSearch derived ports.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::{
    ChunkId, EmbeddingProfileId, GraphEdge, GraphEdgeId, GraphNode, GraphNodeId, SourceId,
};

use super::common::{
    PageRequest, PageResponse, StorageAuthContext, StorageCapability, StorageGeneration,
    StorageResult,
};

// ---------------------------------------------------------------------------
// LexicalSearch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalSearchRequest {
    pub auth: StorageAuthContext,
    pub query: String,
    pub page: PageRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_filter: Option<SourceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_filter: Option<String>,
    /// Optional generation fence; stale hits must surface as StaleGeneration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_generation: Option<StorageGeneration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalSearchHit {
    pub chunk_id: ChunkId,
    pub source_id: SourceId,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalSearchResponse {
    pub page: PageResponse<LexicalSearchHit>,
    pub generation: StorageGeneration,
}

/// Derived lexical / BM25 search port. Rebuild stays inside adapters.
#[async_trait]
pub trait LexicalSearch: StorageCapability + Send + Sync {
    async fn search(&self, request: LexicalSearchRequest) -> StorageResult<LexicalSearchResponse>;
}

// ---------------------------------------------------------------------------
// VectorSearch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchRequest {
    pub auth: StorageAuthContext,
    pub query_vector: Vec<f32>,
    pub page: PageRequest,
    pub profile_id: EmbeddingProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_filter: Option<SourceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_generation: Option<StorageGeneration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchHit {
    pub chunk_id: ChunkId,
    pub source_id: SourceId,
    pub score: f32,
    pub profile_generation: StorageGeneration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchResponse {
    pub page: PageResponse<VectorSearchHit>,
    pub generation: StorageGeneration,
}

/// Derived dense-vector similarity search port.
#[async_trait]
pub trait VectorSearch: StorageCapability + Send + Sync {
    async fn search(&self, request: VectorSearchRequest) -> StorageResult<VectorSearchResponse>;
}

// ---------------------------------------------------------------------------
// GraphSearch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphGetNodeRequest {
    pub auth: StorageAuthContext,
    pub node_id: GraphNodeId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphGetNodeResponse {
    pub node: GraphNode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNeighborsRequest {
    pub auth: StorageAuthContext,
    pub node_id: GraphNodeId,
    pub page: PageRequest,
    /// Optional edge-type allow-list (string form of `EdgeType`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNeighbor {
    pub edge: GraphEdge,
    pub node: GraphNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNeighborsResponse {
    pub page: PageResponse<GraphNeighbor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphGetEdgeRequest {
    pub auth: StorageAuthContext,
    pub edge_id: GraphEdgeId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphGetEdgeResponse {
    pub edge: GraphEdge,
}

/// Graph / entity query port. Interface is stable; adapters may start as
/// unsupported stubs that return [`super::common::StorageError::Unsupported`].
#[async_trait]
pub trait GraphSearch: StorageCapability + Send + Sync {
    async fn get_node(&self, request: GraphGetNodeRequest) -> StorageResult<GraphGetNodeResponse>;

    async fn neighbors(
        &self,
        request: GraphNeighborsRequest,
    ) -> StorageResult<GraphNeighborsResponse>;

    async fn get_edge(&self, request: GraphGetEdgeRequest) -> StorageResult<GraphGetEdgeResponse>;
}
