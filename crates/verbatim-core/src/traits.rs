use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;

use crate::types::{ChunkId, EvidenceUnit};

pub trait Parser: Send + Sync {
    fn name(&self) -> &str;
    fn supported_extensions(&self) -> &[&str];
    fn parse(&self, path: &Path) -> Result<Vec<EvidenceUnit>>;
}

#[async_trait]
pub trait EmbeddingClient: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
}

pub trait VectorIndex: Send + Sync {
    fn add(&mut self, chunk_id: &ChunkId, vector: Vec<f32>);
    fn build(&mut self) -> Result<()>;
    fn search(&self, query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)>;
    fn save(&self, path: &Path) -> Result<()>;
    fn load(&mut self, path: &Path) -> Result<()>;
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
