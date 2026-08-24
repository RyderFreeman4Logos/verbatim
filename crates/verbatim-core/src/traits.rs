use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::image_limits::ImageArtifactLimits;
use crate::store::Store;
use crate::types::{ChunkId, EvidenceUnit, ParsedImageArtifact, SourceId};

/// Sanitized embedding endpoint/runtime semantics that can affect vector identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingEndpointCapabilities {
    /// Endpoint identity with query strings/fragments stripped so secrets are never persisted.
    pub endpoint_identity: Option<String>,
    /// Requested model name sent to the embedding endpoint.
    pub requested_model: Option<String>,
    /// Served/base model identity when exposed or configured.
    pub served_model: Option<String>,
    /// Effective embedding context window used for adaptive chunking.
    pub max_context_tokens: Option<usize>,
    /// Exposed or configured weight dtype.
    pub dtype: Option<String>,
    /// Exposed or configured quantization/runtime format.
    pub quantization: Option<String>,
    /// Exposed or configured immutable weight/revision identity.
    pub weight_identity: Option<String>,
}

pub trait Parser: Send + Sync {
    fn name(&self) -> &str;
    fn supported_extensions(&self) -> &[&str];
    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>>;
    fn extract_image_artifacts(&self, path: &Path) -> Result<Vec<ParsedImageArtifact>> {
        self.extract_image_artifacts_with_limits(path, ImageArtifactLimits::default())
    }
    fn extract_image_artifacts_with_limits(
        &self,
        _path: &Path,
        _limits: ImageArtifactLimits,
    ) -> Result<Vec<ParsedImageArtifact>> {
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait EmbeddingClient: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn endpoint_capabilities(&self) -> Result<EmbeddingEndpointCapabilities> {
        Ok(EmbeddingEndpointCapabilities::default())
    }

    fn dimension(&self) -> usize;

    fn prepare_query(&self, query: &str) -> String {
        query.to_string()
    }

    fn prepare_document(&self, text: &str, heading: &str) -> String {
        if heading.is_empty() {
            text.to_string()
        } else {
            format!("{heading}: {text}")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Searchable text derived from a child chunk for lexical indexing.
pub struct LexicalDocument {
    pub chunk_id: ChunkId,
    pub source_id: SourceId,
    pub text: String,
    pub heading: String,
}

/// Lexical index boundary for BM25-style retrieval.
pub trait LexicalIndex {
    /// Insert or replace the indexed representation of one child chunk.
    fn upsert(&self, document: &LexicalDocument) -> Result<()>;
    /// Remove every lexical entry derived from a source.
    fn delete_source(&self, source_id: &SourceId) -> Result<()>;
    /// Return ranked child chunk IDs for a user query.
    fn search(&self, query: &str, top_k: usize) -> Result<Vec<(ChunkId, f32)>>;
    /// Return ranked child chunk IDs after applying a strict source filter.
    ///
    /// Backends without native source filtering fail closed instead of
    /// returning an unscoped candidate prefix for post-filtering.
    fn search_filtered(
        &self,
        query: &str,
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if top_k == 0 || source_filter.is_none() {
            return self.search(query, top_k);
        }
        Err(anyhow!(
            crate::overfetch::OverfetchError::UnsupportedStrictFilter
        ))
    }
    /// Rebuild the complete lexical index from SQLite's authoritative chunks.
    fn rebuild_from_store(&self, store: &Store) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Dense vector derived from a child chunk for local vector indexing.
pub struct VectorDocument {
    pub chunk_id: ChunkId,
    pub source_id: SourceId,
    pub vector: Vec<f32>,
}

/// Local dense index boundary for retrieval over stored child chunk vectors.
pub trait VectorIndex: Send + Sync {
    /// Insert or replace one chunk vector.
    fn upsert(&mut self, document: VectorDocument);
    /// Remove every vector derived from a source.
    fn delete_source(&mut self, source_id: &SourceId) -> Result<()>;
    /// Return ranked child chunk IDs for a query vector.
    fn search(&self, query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)>;
    /// Return ranked child chunk IDs for a query vector and optional source filter.
    fn search_filtered(
        &self,
        query: &[f32],
        top_k: usize,
        source_filter: Option<&SourceId>,
    ) -> Vec<(ChunkId, f32)> {
        let _ = source_filter;
        self.search(query, top_k)
    }
    /// Whether this index can apply source filtering before ranking.
    fn supports_source_filter(&self) -> bool {
        false
    }
    /// Rebuild the complete vector index from SQLite's stored vectors.
    fn rebuild_from_store(&mut self, store: &Store) -> Result<()>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
pub trait Reranker: Send + Sync {
    async fn rerank(&self, query: &str, docs: &[String], top_n: usize)
        -> Result<Vec<(usize, f32)>>;

    async fn rerank_with_diagnostics(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> Result<RerankResponse> {
        Ok(RerankResponse {
            hits: self.rerank(query, docs, top_n).await?,
            diagnostics: RerankDiagnostics::default(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RerankResponse {
    pub hits: Vec<(usize, f32)>,
    pub diagnostics: RerankDiagnostics,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RerankDiagnostics {
    pub capability: Option<RerankCapabilityDiagnostics>,
    pub request: Option<RerankRequestDiagnostics>,
    pub retried_after_context_limit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankCapabilityDiagnostics {
    pub state: RerankCapabilityState,
    pub max_context_tokens: Option<usize>,
    pub max_candidates: Option<usize>,
    pub max_documents: Option<usize>,
    pub max_document_chars: Option<usize>,
    pub max_payload_chars: Option<usize>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankCapabilityState {
    Cached,
    Refreshed,
    Unavailable,
    RefreshFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankRequestDiagnostics {
    pub candidate_count: usize,
    pub document_char_limit: usize,
    pub top_n: usize,
}

#[derive(Debug)]
pub struct RerankError {
    source: anyhow::Error,
    diagnostics: RerankDiagnostics,
}

impl RerankError {
    pub fn new(source: anyhow::Error, diagnostics: RerankDiagnostics) -> Self {
        Self {
            source,
            diagnostics,
        }
    }

    pub fn source_error(&self) -> &anyhow::Error {
        &self.source
    }

    pub fn diagnostics(&self) -> &RerankDiagnostics {
        &self.diagnostics
    }
}

impl fmt::Display for RerankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rerank failed: {}", self.source)
    }
}

impl std::error::Error for RerankError {}
