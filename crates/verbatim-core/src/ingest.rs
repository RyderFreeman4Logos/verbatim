use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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
use crate::types::{Chunk, ChunkId, ChunkType, Source, SourceId, SourceStatus};

pub struct IngestPipeline<E = OpenAiEmbeddingClient> {
    store: Store,
    hnsw: HnswIndex,
    bm25: Bm25Index,
    embed_client: E,
    context_gen: Option<ContextGenerator>,
    data_dir: PathBuf,
}

struct PreparedIndexes {
    hnsw: HnswIndex,
    bm25_docs: Vec<(ChunkId, String, String)>,
}

#[derive(Debug, Deserialize, Serialize)]
struct IndexManifest {
    generation: u64,
}

impl IngestPipeline<OpenAiEmbeddingClient> {
    pub fn new(config: &Config, data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create data dir: {}", data_dir.display()))?;

        let db_path = data_dir.join("verbatim.db");
        let store = Store::new(&db_path)?;

        let (hnsw, bm25) = load_published_indexes(data_dir, store.index_generation()?)?;

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
}

impl<E> IngestPipeline<E>
where
    E: EmbeddingClient,
{
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn hnsw(&self) -> &HnswIndex {
        &self.hnsw
    }

    pub fn bm25(&self) -> &Bm25Index {
        &self.bm25
    }

    #[cfg(test)]
    fn from_parts(
        store: Store,
        hnsw: HnswIndex,
        bm25: Bm25Index,
        embed_client: E,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            store,
            hnsw,
            bm25,
            embed_client,
            context_gen: None,
            data_dir,
        }
    }

    pub fn add_source(&self, path: &Path) -> Result<SourceId> {
        let abs_path = std::fs::canonicalize(path)
            .with_context(|| format!("resolve path: {}", path.display()))?;
        let hash = file_hash(&abs_path)?;
        let id = SourceId::from_path(&abs_path);

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

    pub async fn remove_source(&mut self, source_id: &SourceId) -> Result<()> {
        let child_chunks = self.child_chunks_without_source(source_id)?;
        let prepared = self.prepare_indexes_for_chunks(&child_chunks).await?;
        let staged = stage_prepared_index_artifacts(&self.data_dir, &prepared)?;
        let generation = match self.store.remove_source(source_id) {
            Ok(generation) => generation,
            Err(err) => {
                let _ = remove_dir_if_exists(&staged);
                return Err(err);
            }
        };
        self.publish_committed_indexes(generation, staged, prepared)?;
        Ok(())
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
        let mut evidence = parser.parse(&source.path)?;
        normalize_evidence_source_ids(&mut evidence, source_id);
        tracing::info!(evidence_count = evidence.len(), "parsed");

        let chunker_config = ChunkerConfig::default();
        let output = chunk_evidence(source_id, &evidence, &chunker_config);
        tracing::info!(chunk_count = output.chunks.len(), "chunked");

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

        let mut child_chunks = self.child_chunks_without_source(source_id)?;
        child_chunks.extend(
            chunks
                .iter()
                .filter(|chunk| chunk.chunk_type == ChunkType::Child)
                .cloned(),
        );
        let prepared = self.prepare_indexes_for_chunks(&child_chunks).await?;

        let staged = stage_prepared_index_artifacts(&self.data_dir, &prepared)?;
        let generation =
            match self
                .store
                .replace_source_contents(&new_source, &evidence, &chunks, &output.links)
            {
                Ok(generation) => generation,
                Err(err) => {
                    let _ = remove_dir_if_exists(&staged);
                    return Err(err);
                }
            };
        self.publish_committed_indexes(generation, staged, prepared)?;

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

    pub async fn rebuild_indexes_from_store(&mut self) -> Result<()> {
        let child_chunks = self.store.list_child_chunks()?;
        tracing::info!(count = child_chunks.len(), "rebuilding local indexes");
        let prepared = self.prepare_indexes_for_chunks(&child_chunks).await?;
        self.publish_prepared_indexes(prepared)?;

        Ok(())
    }

    fn child_chunks_without_source(&self, source_id: &SourceId) -> Result<Vec<Chunk>> {
        Ok(self
            .store
            .list_child_chunks()?
            .into_iter()
            .filter(|chunk| chunk.source_id != *source_id)
            .collect())
    }

    async fn prepare_indexes_for_chunks(&self, child_chunks: &[Chunk]) -> Result<PreparedIndexes> {
        let mut hnsw = HnswIndex::new();
        if !child_chunks.is_empty() {
            let texts: Vec<String> = child_chunks
                .iter()
                .map(|c| self.embedding_text(c))
                .collect();
            let embeddings = self.embed_client.embed(&texts).await?;
            for (chunk, embedding) in child_chunks.iter().zip(embeddings) {
                hnsw.add(&chunk.id, embedding);
            }
        }
        hnsw.build()?;

        let bm25_docs: Vec<_> = child_chunks
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    chunk_search_text(c),
                    c.heading_path.join(" > "),
                )
            })
            .collect();

        Ok(PreparedIndexes { hnsw, bm25_docs })
    }

    fn publish_prepared_indexes(&mut self, prepared: PreparedIndexes) -> Result<()> {
        let generation = self.store.index_generation()?;
        let staged = stage_prepared_index_artifacts(&self.data_dir, &prepared)?;
        self.publish_committed_indexes(generation, staged, prepared)
    }

    fn publish_committed_indexes(
        &mut self,
        generation: u64,
        staged: PathBuf,
        prepared: PreparedIndexes,
    ) -> Result<()> {
        let new_bm25 = match publish_staged_index_artifacts(&self.data_dir, generation, &staged) {
            Ok(new_bm25) => new_bm25,
            Err(err) => {
                self.invalidate_live_indexes()?;
                return Err(err);
            }
        };
        self.hnsw = prepared.hnsw;
        self.bm25 = new_bm25;
        Ok(())
    }

    fn invalidate_live_indexes(&mut self) -> Result<()> {
        self.hnsw = HnswIndex::new();
        self.bm25 = open_unpublished_bm25(&self.data_dir)?;
        Ok(())
    }

    fn embedding_text(&self, chunk: &Chunk) -> String {
        self.embed_client
            .prepare_document(&chunk_search_text(chunk), &chunk.heading_path.join(" > "))
    }
}

fn load_published_indexes(
    data_dir: &Path,
    store_generation: u64,
) -> Result<(HnswIndex, Bm25Index)> {
    let manifest_generation = read_index_manifest(data_dir)?.map(|manifest| manifest.generation);
    if manifest_generation == Some(store_generation) {
        let generation_dir = index_generation_dir(data_dir, store_generation);
        let hnsw_path = generation_dir.join("vectors.hnsw");
        let tantivy_dir = generation_dir.join("tantivy");
        if hnsw_path.exists() && tantivy_dir.exists() {
            let mut hnsw = HnswIndex::new();
            hnsw.load(&hnsw_path)?;
            return Ok((hnsw, Bm25Index::open_or_create(&tantivy_dir)?));
        }
    }

    Ok((HnswIndex::new(), open_unpublished_bm25(data_dir)?))
}

fn stage_prepared_index_artifacts(data_dir: &Path, prepared: &PreparedIndexes) -> Result<PathBuf> {
    let staging_dir = unique_staging_dir(data_dir);
    if staging_dir.exists() {
        remove_dir_if_exists(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)
        .with_context(|| format!("create index staging dir: {}", staging_dir.display()))?;

    prepared.hnsw.save(&staging_dir.join("vectors.hnsw"))?;
    let staged_bm25 = Bm25Index::open_or_create(&staging_dir.join("tantivy"))?;
    staged_bm25.clear_and_rebuild(&prepared.bm25_docs)?;

    Ok(staging_dir)
}

fn publish_staged_index_artifacts(
    data_dir: &Path,
    generation: u64,
    staging_dir: &Path,
) -> Result<Bm25Index> {
    let generation_dir = index_generation_dir(data_dir, generation);
    if generation_dir.exists() {
        remove_dir_if_exists(&generation_dir)?;
    }
    fs::create_dir_all(index_root_dir(data_dir))
        .with_context(|| format!("create index root: {}", index_root_dir(data_dir).display()))?;
    fs::rename(staging_dir, &generation_dir).with_context(|| {
        format!(
            "publish staged index generation: {} -> {}",
            staging_dir.display(),
            generation_dir.display()
        )
    })?;

    let bm25 = Bm25Index::open_or_create(&generation_dir.join("tantivy"))?;
    write_index_manifest(data_dir, generation)?;
    Ok(bm25)
}

fn read_index_manifest(data_dir: &Path) -> Result<Option<IndexManifest>> {
    let path = index_manifest_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let data =
        fs::read(&path).with_context(|| format!("read index manifest: {}", path.display()))?;
    serde_json::from_slice(&data)
        .with_context(|| format!("parse index manifest: {}", path.display()))
        .map(Some)
}

fn write_index_manifest(data_dir: &Path, generation: u64) -> Result<()> {
    let path = index_manifest_path(data_dir);
    let tmp_path = path.with_extension("json.tmp");
    let data = serde_json::to_vec(&IndexManifest { generation })?;
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

fn open_unpublished_bm25(data_dir: &Path) -> Result<Bm25Index> {
    let path = index_root_dir(data_dir).join("unpublished-tantivy");
    remove_dir_if_exists(&path)?;
    Bm25Index::open_or_create(&path)
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove dir: {}", path.display())),
    }
}

fn index_manifest_path(data_dir: &Path) -> PathBuf {
    data_dir.join("index-manifest.json")
}

fn index_root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes")
}

fn index_generation_dir(data_dir: &Path, generation: u64) -> PathBuf {
    index_root_dir(data_dir).join(format!("gen-{generation}"))
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

fn normalize_evidence_source_ids(
    evidence: &mut [crate::types::EvidenceUnit],
    source_id: &SourceId,
) {
    for unit in evidence {
        unit.source_id = source_id.clone();
    }
}

fn file_hash(path: &Path) -> Result<String> {
    let data = std::fs::read(path).with_context(|| format!("read file: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use async_trait::async_trait;

    use crate::types::{EvidenceId, EvidenceUnit, SourceLocator};

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

    fn test_evidence(source_id: &SourceId, id: &str, text: &str) -> EvidenceUnit {
        EvidenceUnit {
            id: EvidenceId(id.to_string()),
            source_id: source_id.clone(),
            locator: SourceLocator::Document {
                path_or_url: source_id.0.clone(),
                line_start: 1,
                line_end: None,
            },
            text: text.to_string(),
            text_hash: format!("hash-{id}"),
            heading_path: Vec::new(),
            position: 0,
        }
    }

    fn test_child(source_id: &SourceId, id: &str, evidence_id: &EvidenceId, text: &str) -> Chunk {
        Chunk {
            id: ChunkId(id.to_string()),
            source_id: source_id.clone(),
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
        let evidence = test_evidence(&source.id, &format!("evidence-{chunk_id}"), "old text");
        let chunk = test_child(&source.id, chunk_id, &evidence.id, "old text");
        store.add_source(source)?;
        store.bulk_insert_evidence(&[evidence])?;
        store.bulk_insert_chunks(std::slice::from_ref(&chunk))?;
        store.link_chunk_evidence(&[(chunk.id.clone(), chunk.evidence_unit_ids[0].clone())])?;
        Ok(chunk)
    }

    fn hnsw_with_chunks(chunks: &[Chunk]) -> HnswIndex {
        let mut hnsw = HnswIndex::new();
        for (idx, chunk) in chunks.iter().enumerate() {
            hnsw.add(&chunk.id, vec![idx as f32, 1.0]);
        }
        hnsw.build().unwrap();
        hnsw
    }

    #[tokio::test]
    async fn remove_source_keeps_store_and_hnsw_when_embedding_rebuild_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
        let bm25 = Bm25Index::create_in_ram().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            bm25,
            FailingEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let err = pipeline.remove_source(&first.id).await.unwrap_err();

        assert!(err.to_string().contains("embedding unavailable"));
        assert!(pipeline.store().get_source(&first.id).unwrap().is_some());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert_eq!(pipeline.store().list_child_chunks().unwrap().len(), 2);
        assert_eq!(pipeline.hnsw().len(), 2);
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
        let bm25 = Bm25Index::create_in_ram().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            bm25,
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
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
        let bm25 = Bm25Index::create_in_ram().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            bm25,
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
        let bm25 = Bm25Index::create_in_ram().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            bm25,
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
        std::fs::create_dir(index_manifest_path(tempdir.path()).with_extension("json.tmp"))
            .unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
        let bm25 = Bm25Index::create_in_ram().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            bm25,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let err = pipeline.remove_source(&first.id).await.unwrap_err();

        assert!(err.to_string().contains("write index manifest temp"));
        assert!(pipeline.store().get_source(&first.id).unwrap().is_none());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert_eq!(pipeline.store().index_generation().unwrap(), 1);
        assert!(read_index_manifest(tempdir.path()).unwrap().is_none());
        let (loaded_hnsw, loaded_bm25) =
            load_published_indexes(tempdir.path(), pipeline.store().index_generation().unwrap())
                .unwrap();
        assert!(loaded_hnsw.is_empty());
        assert!(loaded_bm25.search("old text", 5).unwrap().is_empty());
        assert!(pipeline.hnsw().is_empty());
    }

    #[tokio::test]
    async fn ingest_source_ignores_unmanifested_indexes_when_manifest_write_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::create_dir(index_manifest_path(tempdir.path()).with_extension("json.tmp"))
            .unwrap();
        let path = tempdir.path().join("doc.txt");
        std::fs::write(&path, "new text for ingest\n").unwrap();
        let store = Store::in_memory().unwrap();
        let source = test_source("src-1", path);
        let old_chunk = insert_source_with_child(&store, &source, "old-chunk").unwrap();
        let hnsw = hnsw_with_chunks(&[old_chunk]);
        let bm25 = Bm25Index::create_in_ram().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            bm25,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let err = pipeline.ingest_source(&source.id).await.unwrap_err();

        assert!(err.to_string().contains("write index manifest temp"));
        assert_eq!(pipeline.store().index_generation().unwrap(), 1);
        assert!(read_index_manifest(tempdir.path()).unwrap().is_none());
        let chunks = pipeline.store().list_chunks_by_source(&source.id).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|chunk| chunk.id.0 != "old-chunk"));
        let (loaded_hnsw, loaded_bm25) =
            load_published_indexes(tempdir.path(), pipeline.store().index_generation().unwrap())
                .unwrap();
        assert!(loaded_hnsw.is_empty());
        assert!(loaded_bm25.search("new", 5).unwrap().is_empty());
        assert!(pipeline.hnsw().is_empty());
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
        let bm25 = Bm25Index::create_in_ram().unwrap();
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            bm25,
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
