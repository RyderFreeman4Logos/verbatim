use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::image_limits::ImageArtifactLimits;
use crate::store::Store;
use crate::types::{ChunkId, EvidenceUnit, ParsedImageArtifact, SourceId};

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
}
