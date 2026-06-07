use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::chunker::{chunk_evidence, ChunkerConfig};
use crate::config::Config;
use crate::context::ContextGenerator;
use crate::embed::OpenAiEmbeddingClient;
use crate::index::hnsw::HnswIndex;
use crate::index::tantivy_bm25::Bm25Index;
use crate::parser;
use crate::store::Store;
use crate::traits::EmbeddingClient;
use crate::types::{ChunkType, Source, SourceId, SourceStatus};

pub struct IngestPipeline {
    store: Store,
    hnsw: HnswIndex,
    bm25: Bm25Index,
    embed_client: OpenAiEmbeddingClient,
    context_gen: Option<ContextGenerator>,
    data_dir: PathBuf,
}

impl IngestPipeline {
    pub fn new(config: &Config, data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create data dir: {}", data_dir.display()))?;

        let db_path = data_dir.join("verbatim.db");
        let store = Store::new(&db_path)?;

        let mut hnsw = HnswIndex::new();
        let hnsw_path = data_dir.join("vectors.hnsw");
        if hnsw_path.exists() {
            hnsw.load(&hnsw_path)?;
        }

        let tantivy_dir = data_dir.join("tantivy");
        let bm25 = Bm25Index::open_or_create(&tantivy_dir)?;

        let embed_client = OpenAiEmbeddingClient::new(&config.embedding);

        let context_gen = if config.context.enabled {
            Some(ContextGenerator::new(&config.chat))
        } else {
            None
        };

        Ok(Self {
            store,
            hnsw,
            bm25,
            embed_client,
            context_gen,
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn hnsw(&self) -> &HnswIndex {
        &self.hnsw
    }

    pub fn bm25(&self) -> &Bm25Index {
        &self.bm25
    }

    pub fn add_source(&self, path: &Path) -> Result<SourceId> {
        let abs_path = std::fs::canonicalize(path)
            .with_context(|| format!("resolve path: {}", path.display()))?;
        let hash = file_hash(&abs_path)?;
        let id = SourceId(
            abs_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string(),
        );

        let source = Source {
            id: id.clone(),
            path: abs_path,
            hash,
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        };
        self.store.add_source(&source)?;
        Ok(id)
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
        let stale = self.store.find_stale_sources(&current_hashes)?;
        for id in &stale {
            self.store.update_source_status(id, &SourceStatus::Stale)?;
        }
        Ok(stale)
    }

    pub async fn ingest_source(&mut self, source_id: &SourceId) -> Result<()> {
        let source = self
            .store
            .get_source(source_id)?
            .with_context(|| format!("source not found: {}", source_id.0))?;

        tracing::info!(source = %source_id.0, path = %source.path.display(), "ingesting");

        self.store.remove_source(source_id)?;

        let hash = file_hash(&source.path)?;
        let new_source = Source {
            id: source_id.clone(),
            path: source.path.clone(),
            hash,
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        };
        self.store.add_source(&new_source)?;

        let parser = parser::parser_for_extension(&source.path)?;
        tracing::info!(parser = parser.name(), "parsing");
        let evidence = parser.parse(&source.path)?;
        tracing::info!(evidence_count = evidence.len(), "parsed");

        self.store.bulk_insert_evidence(&evidence)?;

        let chunker_config = ChunkerConfig::default();
        let output = chunk_evidence(source_id, &evidence, &chunker_config);
        tracing::info!(chunk_count = output.chunks.len(), "chunked");

        self.store.bulk_insert_chunks(&output.chunks)?;
        self.store.link_chunk_evidence(&output.links)?;

        let mut chunks = output.chunks;
        if let Some(ctx_gen) = &self.context_gen {
            let title = source
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document");
            let enriched = ctx_gen.enrich_chunks(&mut chunks, title, 8).await?;
            tracing::info!(enriched, "contextual retrieval done");
        }

        let child_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Child)
            .collect();

        if !child_chunks.is_empty() {
            let texts: Vec<String> = child_chunks
                .iter()
                .map(|c| {
                    let heading = c.heading_path.join(" > ");
                    let text = c
                        .context_text
                        .as_ref()
                        .map(|ctx| format!("{ctx} {}", c.text))
                        .unwrap_or_else(|| c.text.clone());
                    self.embed_client.prepare_document(&text, &heading)
                })
                .collect();

            tracing::info!(count = texts.len(), "embedding");
            let embeddings = self.embed_client.embed(&texts).await?;

            for (chunk, embedding) in child_chunks.iter().zip(embeddings) {
                self.hnsw.add(&chunk.id, embedding);
            }
        }

        tracing::info!("building HNSW index");
        self.hnsw.build()?;
        self.hnsw.save(&self.data_dir.join("vectors.hnsw"))?;

        let bm25_docs: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Child)
            .map(|c| {
                let heading = c.heading_path.join(" > ");
                let text = c
                    .context_text
                    .as_ref()
                    .map(|ctx| format!("{ctx} {}", c.text))
                    .unwrap_or_else(|| c.text.clone());
                (c.id.clone(), text, heading)
            })
            .collect();
        self.bm25.add_documents(&bm25_docs)?;

        self.store
            .update_source_status(source_id, &SourceStatus::Indexed)?;
        self.store
            .update_source_hash(source_id, &file_hash(&source.path)?)?;

        tracing::info!(source = %source_id.0, "ingest complete");
        Ok(())
    }

    pub async fn ingest_all(&mut self, force: bool) -> Result<usize> {
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
        for (i, source_id) in to_ingest.iter().enumerate() {
            tracing::info!(progress = format!("{}/{}", i + 1, total), source = %source_id.0);
            self.ingest_source(source_id).await?;
        }

        Ok(total)
    }
}

fn file_hash(path: &Path) -> Result<String> {
    let data = std::fs::read(path).with_context(|| format!("read file: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}
