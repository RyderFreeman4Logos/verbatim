use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chunker::{chunk_evidence, estimate_tokens, ChunkOutput, ChunkerConfig};
use crate::config::Config;
use crate::context::ContextGenerator;
use crate::embed::OpenAiEmbeddingClient;
use crate::image_limits::{
    ImageArtifactBudget, ImageArtifactLimitError, ImageArtifactLimitStage, ImageArtifactLimits,
};
use crate::index::hnsw::HnswIndex;
use crate::index::sqlite_fts::SqliteFtsIndex;
use crate::parser;
use crate::provider::openai_compatible::OpenAiCompatibleVisionModel;
use crate::provider::VisionModel;
use crate::store::{SourceContentsReplacement, Store};
use crate::traits::{EmbeddingClient, LexicalIndex, Parser, VectorDocument, VectorIndex};
use crate::types::{
    hex_sha256, Chunk, ChunkId, ChunkType, EdgeType, EvidenceId, EvidenceKind, EvidenceUnit,
    GraphEdge, GraphEdgeId, GraphNode, GraphNodeId, GraphNodeKind, ImageArtifact, ImageId,
    ParsedImageArtifact, Source, SourceId, SourceLocator, SourceStatus,
};
use crate::vision_caption::{
    caption_derived_evidence, request_image_caption, vision_caption_prompt_hash, CaptionAttempt,
    ImageCaptionStatus, VISION_CAPTION_PROMPT_VERSION,
};

pub struct IngestPipeline<E = OpenAiEmbeddingClient> {
    store: Store,
    hnsw: HnswIndex,
    embed_client: E,
    context_gen: Option<ContextGenerator>,
    vision_model: Option<Box<dyn VisionModel>>,
    vision_caption_model: String,
    vision_caption_prompt_hash: String,
    data_dir: PathBuf,
    image_artifact_limits: ImageArtifactLimits,
}

struct PreparedIndexes {
    hnsw: HnswIndex,
    vectors: Vec<VectorDocument>,
}

#[derive(Debug)]
struct PreparedImageArtifacts {
    evidence: Vec<EvidenceUnit>,
    artifacts: Vec<ImageArtifact>,
    files: Vec<PreparedImageFile>,
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

impl IngestPipeline<OpenAiEmbeddingClient> {
    pub fn new(config: &Config, data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create data dir: {}", data_dir.display()))?;

        let db_path = data_dir.join("verbatim.db");
        let store = Store::new(&db_path)?;
        SqliteFtsIndex::new(&store).rebuild_from_store(&store)?;

        let hnsw = load_published_vector_index(data_dir, &store)?;

        let embed_client = OpenAiEmbeddingClient::new(&config.embedding);

        let context_gen = if config.context.enabled {
            Some(ContextGenerator::new(&config.chat))
        } else {
            None
        };
        let (vision_model, vision_caption_model) = configured_vision_model(config);

        Ok(Self {
            store,
            hnsw,
            embed_client,
            context_gen,
            vision_model,
            vision_caption_model,
            vision_caption_prompt_hash: vision_caption_prompt_hash(),
            data_dir: data_dir.to_path_buf(),
            image_artifact_limits: config.parser.image_artifacts,
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

    pub fn vector_index(&self) -> &dyn VectorIndex {
        &self.hnsw
    }

    pub fn lexical_index(&self) -> SqliteFtsIndex<'_> {
        SqliteFtsIndex::new(&self.store)
    }

    #[cfg(test)]
    fn from_parts(store: Store, hnsw: HnswIndex, embed_client: E, data_dir: PathBuf) -> Self {
        Self {
            store,
            hnsw,
            embed_client,
            context_gen: None,
            vision_model: None,
            vision_caption_model: "vision-disabled".to_string(),
            vision_caption_prompt_hash: vision_caption_prompt_hash(),
            data_dir,
            image_artifact_limits: ImageArtifactLimits::default(),
        }
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
        self.store
            .get_source(source_id)?
            .with_context(|| format!("source not found: {}", source_id.0))?;
        let child_chunks = self.child_chunks_without_source(source_id)?;
        let prepared = self.prepare_indexes_for_chunks(&child_chunks).await?;
        let staged = stage_prepared_index_artifacts(&self.data_dir, &prepared)?;
        let generation = match self
            .store
            .remove_source_and_replace_vectors(source_id, &prepared.vectors)
        {
            Ok(generation) => generation,
            Err(err) => {
                let _ = remove_dir_if_exists(&staged);
                return Err(err);
            }
        };
        self.publish_committed_indexes(generation, staged, prepared)?;
        remove_source_image_artifacts(&self.data_dir, source_id).with_context(|| {
            format!(
                "cleanup image artifacts after committed source removal: {}",
                source_id.0
            )
        })?;
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
        let caption_evidence = self
            .caption_prepared_image_artifacts(
                source_id,
                &prepared_image_artifacts,
                evidence.len() as u32 + prepared_image_artifacts.evidence.len() as u32,
            )
            .await?;
        evidence.extend(prepared_image_artifacts.evidence.clone());
        tracing::info!(evidence_count = evidence.len(), "parsed");

        let chunker_config = ChunkerConfig::default();
        let output = chunk_evidence(source_id, &evidence, &chunker_config);
        tracing::info!(chunk_count = output.chunks.len(), "chunked");

        let mut chunks = output.chunks;
        let mut links = output.links;
        if let Some(ctx_gen) = &self.context_gen {
            let title = source
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document");
            let enriched = ctx_gen.enrich_chunks(&mut chunks, title, 8).await?;
            tracing::info!(enriched, "contextual retrieval done");
        }

        let caption_output = chunk_caption_evidence(source_id, &caption_evidence);
        chunks.extend(caption_output.chunks);
        links.extend(caption_output.links);
        evidence.extend(caption_evidence);
        let (graph_nodes, graph_edges) = build_evidence_graph(
            &new_source,
            &evidence,
            &chunks,
            &links,
            &prepared_image_artifacts.artifacts,
        );

        let mut child_chunks = self.child_chunks_without_source(source_id)?;
        child_chunks.extend(
            chunks
                .iter()
                .filter(|chunk| chunk.chunk_type == ChunkType::Child)
                .cloned(),
        );
        let prepared = self.prepare_indexes_for_chunks(&child_chunks).await?;

        let staged = stage_prepared_index_artifacts(&self.data_dir, &prepared)?;
        let written_image_files = match write_image_artifact_files(
            &prepared_image_artifacts.files,
            self.image_artifact_limits,
        ) {
            Ok(written) => written,
            Err(err) => {
                let _ = remove_dir_if_exists(&staged);
                return Err(err);
            }
        };
        let generation = match self
            .store
            .replace_source_contents(SourceContentsReplacement {
                source: &new_source,
                evidence: &evidence,
                chunks: &chunks,
                vectors: &prepared.vectors,
                links: &links,
                image_artifacts: &prepared_image_artifacts.artifacts,
                graph_nodes: &graph_nodes,
                graph_edges: &graph_edges,
            }) {
            Ok(generation) => generation,
            Err(err) => {
                cleanup_written_image_files(&written_image_files);
                let _ = remove_dir_if_exists(&staged);
                return Err(err);
            }
        };
        self.publish_committed_indexes(generation, staged, prepared)?;
        cleanup_stale_source_image_artifacts(
            &self.data_dir,
            source_id,
            &prepared_image_artifacts.artifacts,
        )
        .with_context(|| {
            format!(
                "cleanup stale image artifacts after committed source ingest: {}",
                source_id.0
            )
        })?;

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
        self.store.replace_all_vector_documents(&prepared.vectors)?;
        self.lexical_index().rebuild_from_store(&self.store)?;
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
        let mut vectors = Vec::new();
        if !child_chunks.is_empty() {
            let texts: Vec<String> = child_chunks
                .iter()
                .map(|c| self.embedding_text(c))
                .collect();
            let embeddings = self.embed_client.embed(&texts).await?;
            if embeddings.len() != child_chunks.len() {
                bail!(
                    "embedding count mismatch: expected {}, got {}",
                    child_chunks.len(),
                    embeddings.len()
                );
            }
            for (chunk, embedding) in child_chunks.iter().zip(embeddings) {
                let document = VectorDocument {
                    chunk_id: chunk.id.clone(),
                    source_id: chunk.source_id.clone(),
                    vector: embedding,
                };
                hnsw.upsert(document.clone());
                vectors.push(document);
            }
        }
        hnsw.build()?;

        Ok(PreparedIndexes { hnsw, vectors })
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
        match publish_staged_index_artifacts(&self.data_dir, generation, &staged) {
            Ok(()) => {}
            Err(err) => {
                self.invalidate_live_indexes()?;
                return Err(err);
            }
        };
        self.hnsw = prepared.hnsw;
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

    async fn caption_prepared_image_artifacts(
        &self,
        source_id: &SourceId,
        prepared: &PreparedImageArtifacts,
        start_position: u32,
    ) -> Result<Vec<EvidenceUnit>> {
        let mut evidence = Vec::new();
        for (artifact, file) in prepared.artifacts.iter().zip(&prepared.files) {
            if let Some(unit) = self
                .caption_prepared_image(
                    source_id,
                    artifact,
                    file,
                    start_position + evidence.len() as u32,
                )
                .await?
            {
                evidence.push(unit);
            }
        }
        Ok(evidence)
    }

    async fn caption_prepared_image(
        &self,
        source_id: &SourceId,
        artifact: &ImageArtifact,
        file: &PreparedImageFile,
        position: u32,
    ) -> Result<Option<EvidenceUnit>> {
        let Some(model) = &self.vision_model else {
            if let Some(record) = self.store.get_successful_image_caption(
                &artifact.content_hash,
                &self.vision_caption_model,
                &self.vision_caption_prompt_hash,
            )? {
                tracing::debug!(
                    image_id = %artifact.image_id.0,
                    image_hash = %artifact.content_hash,
                    "indexing successful image caption cache while vision provider is disabled"
                );
                if let Some(caption) = record.caption {
                    return Ok(Some(caption_derived_evidence(
                        source_id,
                        artifact,
                        &caption,
                        &self.vision_caption_model,
                        &self.vision_caption_prompt_hash,
                        position,
                    )));
                }
                return Ok(None);
            }
            let attempt =
                CaptionAttempt::skipped("vision caption provider is disabled or not configured");
            self.store.upsert_image_caption_attempt(
                &artifact.content_hash,
                &self.vision_caption_model,
                VISION_CAPTION_PROMPT_VERSION,
                &self.vision_caption_prompt_hash,
                &attempt,
            )?;
            return Ok(None);
        };

        if let Some(record) = self.store.get_successful_image_caption(
            &artifact.content_hash,
            &self.vision_caption_model,
            &self.vision_caption_prompt_hash,
        )? {
            self.store.record_image_caption_cache_hit(
                &artifact.content_hash,
                &self.vision_caption_model,
                &self.vision_caption_prompt_hash,
            )?;
            if let Some(caption) = record.caption {
                return Ok(Some(caption_derived_evidence(
                    source_id,
                    artifact,
                    &caption,
                    &self.vision_caption_model,
                    &self.vision_caption_prompt_hash,
                    position,
                )));
            }
        }

        let attempt = request_image_caption(model.as_ref(), &file.bytes, &artifact.mime_type).await;

        self.store.upsert_image_caption_attempt(
            &artifact.content_hash,
            &self.vision_caption_model,
            VISION_CAPTION_PROMPT_VERSION,
            &self.vision_caption_prompt_hash,
            &attempt,
        )?;

        if attempt.status != ImageCaptionStatus::Success {
            tracing::warn!(
                image_id = %artifact.image_id.0,
                image_hash = %artifact.content_hash,
                status = ?attempt.status,
                error = ?attempt.error_message,
                "image caption unavailable; continuing ingest"
            );
            return Ok(None);
        }

        Ok(attempt.caption.map(|caption| {
            caption_derived_evidence(
                source_id,
                artifact,
                &caption,
                &self.vision_caption_model,
                &self.vision_caption_prompt_hash,
                position,
            )
        }))
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

fn load_published_vector_index(data_dir: &Path, store: &Store) -> Result<HnswIndex> {
    let store_generation = store.index_generation()?;
    let manifest_generation = read_index_manifest(data_dir)?.map(|manifest| manifest.generation);
    if manifest_generation == Some(store_generation) {
        let generation_dir = index_generation_dir(data_dir, store_generation);
        let hnsw_path = generation_dir.join("vectors.hnsw");
        if hnsw_path.exists() {
            let mut hnsw = HnswIndex::new();
            match hnsw.load(&hnsw_path) {
                Ok(()) => return Ok(hnsw),
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
    hnsw.rebuild_from_store(store)?;
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

fn publish_staged_index_artifacts(
    data_dir: &Path,
    generation: u64,
    staging_dir: &Path,
) -> Result<()> {
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

    write_index_manifest(data_dir, generation)?;
    Ok(())
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

fn chunk_caption_evidence(source_id: &SourceId, evidence: &[EvidenceUnit]) -> ChunkOutput {
    let mut chunks = Vec::with_capacity(evidence.len());
    let mut links = Vec::with_capacity(evidence.len());

    for unit in evidence {
        let chunk_id = ChunkId(format!("{}:chunk", unit.id.0));
        chunks.push(Chunk {
            id: chunk_id.clone(),
            source_id: source_id.clone(),
            text: unit.text.clone(),
            context_text: None,
            token_count: estimate_tokens(&unit.text),
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: unit.heading_path.clone(),
            evidence_unit_ids: vec![unit.id.clone()],
        });
        links.push((chunk_id, unit.id.clone()));
    }

    ChunkOutput { chunks, links }
}

fn build_evidence_graph(
    source: &Source,
    evidence: &[EvidenceUnit],
    chunks: &[Chunk],
    links: &[(ChunkId, EvidenceId)],
    image_artifacts: &[ImageArtifact],
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
        self.add_source_contains_edge(&node_id, Some(unit.position));

        if let Some(page) = locator_page(&unit.locator) {
            let page_node_id = self.ensure_page_node(page);
            self.push_edge(
                EdgeType::Contains,
                &page_node_id,
                &node_id,
                Some(unit.position),
            );
        }

        if let Some(section_node_id) =
            self.ensure_section_nodes(&unit.heading_path, Some(unit.position))
        {
            self.push_edge(
                EdgeType::Contains,
                &section_node_id,
                &node_id,
                Some(unit.position),
            );
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
        self.add_source_contains_edge(&node_id, Some(ordinal));

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
            self.push_edge(EdgeType::Contains, &page_node_id, &node_id, Some(ordinal));
        }

        if let Some(section_node_id) = self.ensure_section_nodes(&chunk.heading_path, Some(ordinal))
        {
            self.push_edge(
                EdgeType::Contains,
                &section_node_id,
                &node_id,
                Some(ordinal),
            );
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
        self.add_source_contains_edge(&node_id, Some(artifact.image_index));

        let page_node_id = self.ensure_page_node(artifact.page);
        self.push_edge(
            EdgeType::Contains,
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
        self.push_edge(
            EdgeType::Contains,
            &chunk_node_id,
            &evidence_node_id,
            Some(ordinal),
        );
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
        self.push_edge(EdgeType::Contains, &parent_node_id, &child_node_id, None);
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
        self.add_source_contains_edge(&node_id, Some(page));
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
                self.add_source_contains_edge(&node_id, ordinal);
                if let Some(parent) = &parent_node_id {
                    self.push_edge(EdgeType::Contains, parent, &node_id, ordinal);
                }
                node_id
            };
            parent_node_id = Some(node_id);
        }

        parent_node_id
    }

    fn add_source_contains_edge(&mut self, to_node_id: &GraphNodeId, ordinal: Option<u32>) {
        let source_node_id = self.source_node_id.clone();
        self.push_edge(EdgeType::Contains, &source_node_id, to_node_id, ordinal);
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
        SourceLocator::Pdf { page, .. } | SourceLocator::PdfImage { page, .. } => Some(*page),
        SourceLocator::Document { .. } => None,
    }
}

fn evidence_kind_label(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Text => "text",
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

fn normalize_evidence_source_ids(
    evidence: &mut [crate::types::EvidenceUnit],
    source_id: &SourceId,
) {
    for unit in evidence {
        unit.source_id = source_id.clone();
    }
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
    }

    Ok(PreparedImageArtifacts {
        evidence,
        artifacts,
        files,
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
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::config::RetrievalConfig;
    use crate::image_limits::ImageArtifactLimitError;
    use crate::provider::{ImageDescribeRequest, ImageDescription, ProviderResult, VisionModel};
    use crate::retrieve::RetrievalPipeline;
    use crate::types::{
        ChunkId, EdgeType, EvidenceId, EvidenceKind, EvidenceUnit, GraphEdge, GraphNode,
        GraphNodeId, GraphNodeKind, SourceLocator,
    };
    use crate::vision_caption::{vision_caption_prompt_hash, ImageCaptionStatus};

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
        insert_source_with_child_text(store, source, chunk_id, "old text")
    }

    fn insert_source_with_child_text(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        text: &str,
    ) -> Result<Chunk> {
        let evidence = test_evidence(&source.id, &format!("evidence-{chunk_id}"), text);
        let chunk = test_child(&source.id, chunk_id, &evidence.id, text);
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
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
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
    async fn ingest_source_builds_graph_and_reingest_replaces_stale_nodes() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("graph.md");
        std::fs::write(
            &path,
            "# Intro\n\nFresh graph paragraph.\n\nStale graph paragraph.\n",
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
        let first_edges = pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::Contains)
            .unwrap();
        let source_node = first_nodes
            .iter()
            .find(|node| node.kind == GraphNodeKind::Source)
            .unwrap();
        assert!(first_edges
            .iter()
            .any(|edge| edge.from_node_id == source_node.id));
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

        std::fs::write(&path, "# Intro\n\nFresh graph paragraph.\n").unwrap();
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
    async fn pdf_ingest_writes_stable_image_artifacts_and_removes_them_with_source() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&path);
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
        assert!(pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::DerivedFrom)
            .unwrap()
            .iter()
            .any(|edge| {
                edge.from_node_id == image_evidence_node_id
                    && edge.to_node_id == image_artifact_node_id
            }));

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
    async fn pdf_image_caption_success_persists_derived_evidence_and_reuses_cache() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&path);
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
        let generated = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|unit| unit.kind == EvidenceKind::Generated)
            .collect::<Vec<_>>();
        assert_eq!(generated.len(), 1);
        let image_artifacts = pipeline
            .store()
            .list_image_artifacts_by_source(&source.id)
            .unwrap();
        assert_eq!(image_artifacts.len(), 1);
        assert_eq!(
            generated[0].derived_from,
            Some(image_artifacts[0].evidence_id.clone())
        );
        assert!(generated[0]
            .text
            .contains("Generated image caption (derived evidence"));
        assert!(generated[0].text.contains("not exact OCR"));
        assert!(generated[0].text.contains("Detailed description"));
        assert!(generated[0]
            .text
            .contains("Visible text noted by the vision model"));
        assert!(generated[0].text.contains("Key entities: Input; Index."));
        assert!(generated[0]
            .text
            .contains("Relationships: Input -> Index (feeds)."));
        assert!(generated[0]
            .text
            .contains("Answerable questions: What feeds the index?"));
        assert!(generated[0]
            .text
            .contains("Uncertainties: The small footer text is not legible."));
        assert!(matches!(
            generated[0].locator,
            SourceLocator::PdfImage {
                page: 1,
                image_index: 1,
                ..
            }
        ));
        let caption_chunks = pipeline
            .store()
            .list_chunks_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|chunk| chunk.evidence_unit_ids == vec![generated[0].id.clone()])
            .collect::<Vec<_>>();
        assert_eq!(caption_chunks.len(), 1);
        assert!(caption_chunks[0].text.contains("An indexing flow diagram."));
        assert!(caption_chunks[0].context_text.is_none());
        let generated_node_id =
            GraphNodeId::new(&source.id, GraphNodeKind::EvidenceUnit, &generated[0].id.0);
        let image_evidence_node_id = GraphNodeId::new(
            &source.id,
            GraphNodeKind::EvidenceUnit,
            &image_artifacts[0].evidence_id.0,
        );
        let image_artifact_node_id = GraphNodeId::new(
            &source.id,
            GraphNodeKind::ImageArtifact,
            &image_artifacts[0].image_id.0,
        );
        let caption_chunk_node_id =
            GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &caption_chunks[0].id.0);
        let first_graph_nodes = pipeline
            .store()
            .list_graph_nodes_by_source(&source.id)
            .unwrap();
        assert!(first_graph_nodes
            .iter()
            .any(|node| node.id == generated_node_id));
        assert!(first_graph_nodes
            .iter()
            .any(|node| node.id == image_evidence_node_id));
        assert!(first_graph_nodes
            .iter()
            .any(|node| node.id == image_artifact_node_id));
        assert!(first_graph_nodes
            .iter()
            .any(|node| node.id == caption_chunk_node_id));
        let derived_edges = pipeline
            .store()
            .list_graph_edges_by_type(&source.id, EdgeType::DerivedFrom)
            .unwrap();
        assert!(derived_edges.iter().any(|edge| {
            edge.from_node_id == generated_node_id && edge.to_node_id == image_evidence_node_id
        }));
        assert!(derived_edges.iter().any(|edge| {
            edge.from_node_id == image_evidence_node_id && edge.to_node_id == image_artifact_node_id
        }));
        let first_graph_node_ids = sorted_graph_node_ids(&first_graph_nodes);
        let first_graph_edge_ids = sorted_graph_edge_ids(
            &pipeline
                .store()
                .list_graph_edges_by_source(&source.id)
                .unwrap(),
        );

        pipeline.ingest_source(&source.id).await.unwrap();

        assert_eq!(vision_calls.call_count(), 1);
        let caption_records = pipeline.store().list_image_captions().unwrap();
        assert_eq!(caption_records.len(), 1);
        assert_eq!(caption_records[0].cache_hits, 1);
        let generated_after_cache = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|unit| unit.kind == EvidenceKind::Generated)
            .collect::<Vec<_>>();
        assert_eq!(generated_after_cache.len(), 1);
        let caption_chunks_after_cache = pipeline
            .store()
            .list_chunks_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|chunk| chunk.evidence_unit_ids == vec![generated_after_cache[0].id.clone()])
            .collect::<Vec<_>>();
        assert_eq!(caption_chunks_after_cache.len(), 1);
        assert_eq!(
            sorted_graph_node_ids(
                &pipeline
                    .store()
                    .list_graph_nodes_by_source(&source.id)
                    .unwrap()
            ),
            first_graph_node_ids
        );
        assert_eq!(
            sorted_graph_edge_ids(
                &pipeline
                    .store()
                    .list_graph_edges_by_source(&source.id)
                    .unwrap()
            ),
            first_graph_edge_ids
        );
    }

    #[tokio::test]
    async fn pdf_image_caption_disabled_preserves_successful_cache() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&path);
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
        assert_eq!(generated_after_disabled.len(), 1);
        let caption_chunks_after_disabled = pipeline
            .store()
            .list_chunks_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|chunk| chunk.evidence_unit_ids == vec![generated_after_disabled[0].id.clone()])
            .collect::<Vec<_>>();
        assert_eq!(caption_chunks_after_disabled.len(), 1);
    }

    #[tokio::test]
    async fn image_caption_chunks_are_lexically_and_densely_searchable_with_image_chain() {
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
            unit.kind == EvidenceKind::Generated || !unit.text.contains("captionneedle")
        }));
        let generated = evidence
            .iter()
            .find(|unit| unit.kind == EvidenceKind::Generated)
            .expect("caption evidence should be generated");
        let original_image_id = generated
            .derived_from
            .clone()
            .expect("caption evidence should point to original image evidence");
        let caption_chunk = pipeline
            .store()
            .list_chunks_by_source(&source.id)
            .unwrap()
            .into_iter()
            .find(|chunk| chunk.evidence_unit_ids == vec![generated.id.clone()])
            .expect("caption evidence should have a dedicated child chunk");

        let lexical_hits = pipeline.lexical_index().search("captionneedle", 5).unwrap();
        assert!(lexical_hits
            .iter()
            .any(|(chunk_id, _)| chunk_id == &caption_chunk.id));
        let dense_hits = pipeline.hnsw().search(&[1.0, 0.0], 5);
        assert!(dense_hits
            .iter()
            .any(|(chunk_id, _)| chunk_id == &caption_chunk.id));
        assert!(pipeline
            .store()
            .list_vector_documents()
            .unwrap()
            .iter()
            .any(|document| document.chunk_id == caption_chunk.id));

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
        let results = retrieval.search("captionneedle").await.unwrap();
        let hit = results
            .iter()
            .find(|result| result.chunk_id == caption_chunk.id)
            .expect("caption query should retrieve caption chunk");

        assert!(hit
            .evidence_units
            .iter()
            .any(|unit| unit.id == generated.id));
        let original_image = hit
            .evidence_units
            .iter()
            .find(|unit| unit.id == original_image_id)
            .expect("retrieval should include original image evidence");
        assert_eq!(original_image.kind, EvidenceKind::Image);
        assert!(matches!(
            original_image.locator,
            SourceLocator::PdfImage {
                page: 1,
                image_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn image_caption_index_state_is_cleaned_on_reingest_and_source_removal() {
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
        let mut pipeline = IngestPipeline::from_parts(
            store,
            HnswIndex::new(),
            CaptionKeywordEmbeddingClient,
            tempdir.path().to_path_buf(),
        )
        .with_vision_model("local-vision", vision);

        pipeline.ingest_source(&source.id).await.unwrap();
        let stale_generated = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .find(|unit| unit.kind == EvidenceKind::Generated)
            .unwrap();
        let stale_chunk = pipeline
            .store()
            .list_chunks_by_source(&source.id)
            .unwrap()
            .into_iter()
            .find(|chunk| chunk.evidence_unit_ids == vec![stale_generated.id.clone()])
            .unwrap();
        assert!(!pipeline
            .lexical_index()
            .search("stalecaptionneedle", 5)
            .unwrap()
            .is_empty());

        write_pdf_with_text_and_image_filter(&path, None, &[22u8; 8 * 8 * 3]);
        pipeline.ingest_source(&source.id).await.unwrap();

        assert!(pipeline
            .store()
            .get_evidence(&stale_generated.id)
            .unwrap()
            .is_none());
        assert!(pipeline
            .store()
            .get_chunk(&stale_chunk.id)
            .unwrap()
            .is_none());
        assert!(pipeline
            .lexical_index()
            .search("stalecaptionneedle", 5)
            .unwrap()
            .is_empty());
        assert!(!pipeline
            .lexical_index()
            .search("freshcaptionneedle", 5)
            .unwrap()
            .is_empty());
        assert!(pipeline
            .store()
            .list_vector_documents()
            .unwrap()
            .iter()
            .all(|document| document.chunk_id != stale_chunk.id));

        pipeline.remove_source(&source.id).await.unwrap();

        assert!(pipeline
            .lexical_index()
            .search("freshcaptionneedle", 5)
            .unwrap()
            .is_empty());
        assert!(pipeline.store().list_vector_documents().unwrap().is_empty());
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
        write_pdf_with_image(&path);
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
        let generated = pipeline
            .store()
            .list_evidence_by_source(&source.id)
            .unwrap()
            .into_iter()
            .filter(|unit| unit.kind == EvidenceKind::Generated)
            .collect::<Vec<_>>();
        assert_eq!(generated.len(), 1);
        assert!(generated[0].text.contains("A repaired diagram caption."));
    }

    #[tokio::test]
    async fn pdf_image_caption_records_repair_failure_without_aborting_ingest() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("image-fixture.pdf");
        write_pdf_with_image(&path);
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
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
        write_blocking_image_artifact_path(tempdir.path(), &first.id);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let err = pipeline.remove_source(&first.id).await.unwrap_err();

        assert!(err
            .to_string()
            .contains("cleanup image artifacts after committed source removal"));
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
    async fn remove_source_keeps_store_and_hnsw_when_embedding_rebuild_fails() {
        let tempdir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
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
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
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
        std::fs::create_dir(index_manifest_path(tempdir.path()).with_extension("json.tmp"))
            .unwrap();
        let store = Store::in_memory().unwrap();
        let first = test_source("src-1", tempdir.path().join("first.txt"));
        let second = test_source("src-2", tempdir.path().join("second.txt"));
        let first_chunk = insert_source_with_child(&store, &first, "chunk-1").unwrap();
        let second_chunk = insert_source_with_child(&store, &second, "chunk-2").unwrap();
        let hnsw = hnsw_with_chunks(&[first_chunk, second_chunk]);
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
            StaticEmbeddingClient,
            tempdir.path().to_path_buf(),
        );

        let err = pipeline.remove_source(&first.id).await.unwrap_err();

        assert!(err.to_string().contains("write index manifest temp"));
        assert!(pipeline.store().get_source(&first.id).unwrap().is_none());
        assert!(pipeline.store().get_source(&second.id).unwrap().is_some());
        assert_eq!(pipeline.store().index_generation().unwrap(), 1);
        assert!(read_index_manifest(tempdir.path()).unwrap().is_none());
        let loaded_hnsw = load_published_vector_index(tempdir.path(), pipeline.store()).unwrap();
        assert_eq!(loaded_hnsw.len(), 1);
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
        let mut pipeline = IngestPipeline::from_parts(
            store,
            hnsw,
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
        let loaded_hnsw = load_published_vector_index(tempdir.path(), pipeline.store()).unwrap();
        assert_eq!(
            loaded_hnsw.len(),
            chunks
                .iter()
                .filter(|chunk| chunk.chunk_type == ChunkType::Child)
                .count()
        );
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
