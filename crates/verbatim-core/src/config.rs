use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::image_limits::ImageArtifactLimits;
use crate::index_gc::IndexGcConfig;
use crate::types::{EdgeType, EmbeddingProfileId, OcrProfile, VectorIndexResidency};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub store: StoreConfig,
    #[serde(default)]
    pub parser: ParserConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub retrieval: RetrievalConfig,
    #[serde(default)]
    pub vector_index: VectorIndexConfig,
    #[serde(default)]
    pub graph: GraphConfig,
    #[serde(default)]
    pub rerank: RerankConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub vision: VisionConfig,
    #[serde(default)]
    pub ocr: OcrConfig,
    #[serde(default)]
    pub chat: ChatConfig,
    #[serde(default)]
    pub verifier: VerifierConfig,
    #[serde(default)]
    pub qdrant: QdrantConfig,
    #[serde(default)]
    pub index_gc: IndexGcConfig,
    #[serde(default)]
    pub cli: CliConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub collection_watcher: CollectionWatcherConfig,
}

pub const DEFAULT_TASK_WAIT_TIMEOUT_SECONDS: u64 = 1500;
pub const DEFAULT_MODEL_ENDPOINT_MAX_CONCURRENT_REQUESTS: usize = 4;
pub const DEFAULT_MODEL_ENDPOINT_QUEUE_CAPACITY: usize = 128;
pub const DEFAULT_MODEL_ENDPOINT_QUEUE_TIMEOUT_SECONDS: u64 = 300;
pub const DEFAULT_MODEL_RETRY_MAX_RETRIES: u32 = 3;
pub const DEFAULT_MODEL_RETRY_INITIAL_BACKOFF_MILLIS: u64 = 500;
pub const DEFAULT_MODEL_RETRY_MAX_BACKOFF_MILLIS: u64 = 5_000;
pub const DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS: u64 = 60;
pub const DEFAULT_RESOURCE_QUEUE_TIMEOUT_SECONDS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEndpointRuntimeConfig {
    #[serde(default = "default_model_endpoint_max_concurrent_requests")]
    pub max_concurrent_requests: usize,
    #[serde(default = "default_model_endpoint_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_model_endpoint_queue_timeout_seconds")]
    pub queue_timeout_seconds: u64,
    #[serde(default)]
    pub retry: ModelRetryConfig,
}

impl Default for ModelEndpointRuntimeConfig {
    fn default() -> Self {
        Self {
            max_concurrent_requests: default_model_endpoint_max_concurrent_requests(),
            queue_capacity: default_model_endpoint_queue_capacity(),
            queue_timeout_seconds: default_model_endpoint_queue_timeout_seconds(),
            retry: ModelRetryConfig::default(),
        }
    }
}

impl ModelEndpointRuntimeConfig {
    pub fn bounded(&self) -> Self {
        Self {
            max_concurrent_requests: self.max_concurrent_requests.max(1),
            queue_capacity: self.queue_capacity.max(1),
            queue_timeout_seconds: self.queue_timeout_seconds.max(1),
            retry: self.retry.bounded(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRetryConfig {
    #[serde(default = "default_model_retry_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_model_retry_initial_backoff_millis")]
    pub initial_backoff_millis: u64,
    #[serde(default = "default_model_retry_max_backoff_millis")]
    pub max_backoff_millis: u64,
}

impl Default for ModelRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_model_retry_max_retries(),
            initial_backoff_millis: default_model_retry_initial_backoff_millis(),
            max_backoff_millis: default_model_retry_max_backoff_millis(),
        }
    }
}

impl ModelRetryConfig {
    pub fn bounded(&self) -> Self {
        Self {
            max_retries: self.max_retries,
            initial_backoff_millis: self.initial_backoff_millis.max(1),
            max_backoff_millis: self
                .max_backoff_millis
                .max(self.initial_backoff_millis.max(1)),
        }
    }
}

fn default_model_endpoint_max_concurrent_requests() -> usize {
    DEFAULT_MODEL_ENDPOINT_MAX_CONCURRENT_REQUESTS
}

fn default_model_endpoint_queue_capacity() -> usize {
    DEFAULT_MODEL_ENDPOINT_QUEUE_CAPACITY
}

fn default_model_endpoint_queue_timeout_seconds() -> u64 {
    DEFAULT_MODEL_ENDPOINT_QUEUE_TIMEOUT_SECONDS
}

fn default_model_retry_max_retries() -> u32 {
    DEFAULT_MODEL_RETRY_MAX_RETRIES
}

fn default_model_retry_initial_backoff_millis() -> u64 {
    DEFAULT_MODEL_RETRY_INITIAL_BACKOFF_MILLIS
}

fn default_model_retry_max_backoff_millis() -> u64 {
    DEFAULT_MODEL_RETRY_MAX_BACKOFF_MILLIS
}

fn default_rerank_capability_cache_ttl_seconds() -> u64 {
    DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS
}

fn default_endpoint_capability_cache_ttl_seconds() -> u64 {
    DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    pub path: String,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            path: "~/.local/share/verbatim".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParserConfig {
    pub default: String,
    #[serde(default)]
    pub image_artifacts: ImageArtifactLimits,
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            default: "pdf_oxide".into(),
            image_artifacts: ImageArtifactLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingConfig {
    #[serde(default)]
    pub profile_id: EmbeddingProfileId,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_openai_provider")]
    pub provider: String,
    #[serde(default = "default_embedding_base_url")]
    pub base_url: String,
    #[serde(default = "default_embedding_model")]
    pub model: String,
    #[serde(default = "default_dimension")]
    pub dimension: usize,
    #[serde(default = "default_normalize_embeddings")]
    pub normalize: bool,
    #[serde(default)]
    pub context_window_tokens: Option<usize>,
    #[serde(default)]
    pub served_model: Option<String>,
    #[serde(default)]
    pub dtype: Option<String>,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub weight_identity: Option<String>,
    #[serde(default = "default_query_instruction")]
    pub query_instruction: String,
    #[serde(default)]
    pub document_instruction: String,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_embedding_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_endpoint_capability_cache_ttl_seconds")]
    pub capability_cache_ttl_seconds: u64,
    #[serde(default, flatten)]
    pub endpoint_runtime: ModelEndpointRuntimeConfig,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            profile_id: EmbeddingProfileId::default_profile(),
            enabled: false,
            provider: default_openai_provider(),
            base_url: default_embedding_base_url(),
            model: default_embedding_model(),
            dimension: default_dimension(),
            normalize: true,
            context_window_tokens: None,
            served_model: None,
            dtype: None,
            quantization: None,
            weight_identity: None,
            query_instruction: default_query_instruction(),
            document_instruction: String::new(),
            batch_size: default_batch_size(),
            timeout_seconds: default_embedding_timeout_seconds(),
            api_key: String::new(),
            capability_cache_ttl_seconds: default_endpoint_capability_cache_ttl_seconds(),
            endpoint_runtime: ModelEndpointRuntimeConfig::default(),
        }
    }
}

impl<'de> Deserialize<'de> for EmbeddingConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        struct RawEmbeddingConfig {
            #[serde(default)]
            profile_id: Option<EmbeddingProfileId>,
            #[serde(default)]
            enabled: Option<bool>,
            #[serde(default)]
            provider: Option<String>,
            #[serde(default)]
            base_url: Option<String>,
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            dimension: Option<usize>,
            #[serde(default)]
            normalize: Option<bool>,
            #[serde(default)]
            context_window_tokens: Option<usize>,
            #[serde(default)]
            served_model: Option<String>,
            #[serde(default)]
            dtype: Option<String>,
            #[serde(default)]
            quantization: Option<String>,
            #[serde(default)]
            weight_identity: Option<String>,
            #[serde(default)]
            query_instruction: Option<String>,
            #[serde(default)]
            document_instruction: Option<String>,
            #[serde(default)]
            batch_size: Option<usize>,
            #[serde(default)]
            timeout_seconds: Option<u64>,
            #[serde(default)]
            api_key: Option<String>,
            #[serde(default)]
            capability_cache_ttl_seconds: Option<u64>,
            #[serde(default, flatten)]
            endpoint_runtime: ModelEndpointRuntimeConfig,
        }

        let raw = RawEmbeddingConfig::deserialize(deserializer)?;
        let endpoint_configured = raw.base_url.is_some() || raw.model.is_some();
        Ok(Self {
            profile_id: raw
                .profile_id
                .unwrap_or_else(EmbeddingProfileId::default_profile),
            enabled: raw.enabled.unwrap_or(endpoint_configured),
            provider: raw.provider.unwrap_or_else(default_openai_provider),
            base_url: raw.base_url.unwrap_or_else(default_embedding_base_url),
            model: raw.model.unwrap_or_else(default_embedding_model),
            dimension: raw.dimension.unwrap_or_else(default_dimension),
            normalize: raw.normalize.unwrap_or_else(default_normalize_embeddings),
            context_window_tokens: raw.context_window_tokens,
            served_model: raw.served_model,
            dtype: raw.dtype,
            quantization: raw.quantization,
            weight_identity: raw.weight_identity,
            query_instruction: raw
                .query_instruction
                .unwrap_or_else(default_query_instruction),
            document_instruction: raw.document_instruction.unwrap_or_default(),
            batch_size: raw.batch_size.unwrap_or_else(default_batch_size),
            timeout_seconds: raw
                .timeout_seconds
                .unwrap_or_else(default_embedding_timeout_seconds),
            api_key: raw.api_key.unwrap_or_default(),
            capability_cache_ttl_seconds: raw
                .capability_cache_ttl_seconds
                .unwrap_or_else(default_endpoint_capability_cache_ttl_seconds),
            endpoint_runtime: raw.endpoint_runtime,
        })
    }
}

fn default_enabled() -> bool {
    true
}

fn default_openai_provider() -> String {
    "openai_compatible".into()
}

fn default_embedding_base_url() -> String {
    "http://127.0.0.1:8002/v1".into()
}

fn default_embedding_model() -> String {
    "Qwen/Qwen3-Embedding-8B".into()
}

fn default_dimension() -> usize {
    4096
}

fn default_normalize_embeddings() -> bool {
    true
}

fn default_query_instruction() -> String {
    "Given a user's question about a document, retrieve the exact passages that directly support a grounded answer with source-level citations.".into()
}

fn default_batch_size() -> usize {
    16
}

fn default_embedding_timeout_seconds() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    pub dense_top_k: usize,
    pub bm25_top_k: usize,
    pub rrf_k: usize,
    #[serde(default = "default_retrieval_limit")]
    pub default_limit: usize,
    #[serde(default = "default_retrieval_page_size")]
    pub default_page_size: usize,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            dense_top_k: 80,
            bm25_top_k: 50,
            rrf_k: 60,
            default_limit: default_retrieval_limit(),
            default_page_size: default_retrieval_page_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorIndexConfig {
    #[serde(default)]
    pub residency: VectorIndexResidency,
}

impl Default for VectorIndexConfig {
    fn default() -> Self {
        Self {
            residency: VectorIndexResidency::LowMemory,
        }
    }
}

fn default_retrieval_limit() -> usize {
    12
}

fn default_retrieval_page_size() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphConfig {
    #[serde(default = "default_graph_enabled")]
    pub enabled: bool,
    #[serde(default = "default_graph_max_hops")]
    pub max_hops: usize,
    #[serde(default = "default_graph_max_expanded_chunks")]
    pub max_expanded_chunks: usize,
    #[serde(default = "default_graph_max_neighbors_per_seed")]
    pub max_neighbors_per_seed: usize,
    #[serde(default = "default_graph_edge_types")]
    pub edge_types: Vec<EdgeType>,
    #[serde(default)]
    pub extraction: GraphExtractionConfig,
    #[serde(default)]
    pub global_search: GraphGlobalSearchConfig,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            enabled: default_graph_enabled(),
            max_hops: default_graph_max_hops(),
            max_expanded_chunks: default_graph_max_expanded_chunks(),
            max_neighbors_per_seed: default_graph_max_neighbors_per_seed(),
            edge_types: default_graph_edge_types(),
            extraction: GraphExtractionConfig::default(),
            global_search: GraphGlobalSearchConfig::default(),
        }
    }
}

fn default_graph_enabled() -> bool {
    true
}

fn default_graph_max_hops() -> usize {
    1
}

fn default_graph_max_expanded_chunks() -> usize {
    30
}

fn default_graph_max_neighbors_per_seed() -> usize {
    6
}

fn default_graph_edge_types() -> Vec<EdgeType> {
    vec![
        EdgeType::Parent,
        EdgeType::Previous,
        EdgeType::Next,
        EdgeType::SectionContains,
        EdgeType::PageContainsImage,
        EdgeType::ImageNearText,
        EdgeType::MarkdownLinksTo,
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphExtractionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_graph_extraction_max_chunks")]
    pub max_chunks: usize,
    #[serde(default = "default_graph_extraction_max_chunk_chars")]
    pub max_chunk_chars: usize,
    #[serde(default = "default_graph_extraction_max_entities")]
    pub max_entities: usize,
    #[serde(default = "default_graph_extraction_max_relationships")]
    pub max_relationships: usize,
    #[serde(default = "default_graph_extraction_max_claims")]
    pub max_claims: usize,
    #[serde(default = "default_graph_extraction_max_retries")]
    pub max_retries: usize,
    #[serde(default = "default_graph_extraction_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default = "default_graph_extraction_max_response_chars")]
    pub max_response_chars: usize,
    #[serde(default = "default_graph_extraction_max_error_chars")]
    pub max_error_chars: usize,
}

impl Default for GraphExtractionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_chunks: default_graph_extraction_max_chunks(),
            max_chunk_chars: default_graph_extraction_max_chunk_chars(),
            max_entities: default_graph_extraction_max_entities(),
            max_relationships: default_graph_extraction_max_relationships(),
            max_claims: default_graph_extraction_max_claims(),
            max_retries: default_graph_extraction_max_retries(),
            max_output_tokens: default_graph_extraction_max_output_tokens(),
            max_response_chars: default_graph_extraction_max_response_chars(),
            max_error_chars: default_graph_extraction_max_error_chars(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphGlobalSearchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_global_search_max_communities")]
    pub max_communities: usize,
    #[serde(default = "default_global_search_max_report_claims")]
    pub max_report_claims: usize,
    #[serde(default = "default_global_search_max_report_chars")]
    pub max_report_chars: usize,
    #[serde(default = "default_global_search_max_evidence_per_report")]
    pub max_evidence_per_report: usize,
    #[serde(default = "default_global_search_max_search_results")]
    pub max_search_results: usize,
    #[serde(default)]
    pub drift: GraphDriftSearchConfig,
}

impl Default for GraphGlobalSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_communities: default_global_search_max_communities(),
            max_report_claims: default_global_search_max_report_claims(),
            max_report_chars: default_global_search_max_report_chars(),
            max_evidence_per_report: default_global_search_max_evidence_per_report(),
            max_search_results: default_global_search_max_search_results(),
            drift: GraphDriftSearchConfig::default(),
        }
    }
}

fn default_global_search_max_communities() -> usize {
    128
}

fn default_global_search_max_report_claims() -> usize {
    12
}

fn default_global_search_max_report_chars() -> usize {
    4_000
}

fn default_global_search_max_evidence_per_report() -> usize {
    12
}

fn default_global_search_max_search_results() -> usize {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphDriftSearchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_drift_max_subqueries")]
    pub max_subqueries: usize,
}

impl Default for GraphDriftSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_subqueries: default_drift_max_subqueries(),
        }
    }
}

fn default_drift_max_subqueries() -> usize {
    4
}

fn default_graph_extraction_max_chunks() -> usize {
    8
}

fn default_graph_extraction_max_chunk_chars() -> usize {
    3_000
}

fn default_graph_extraction_max_entities() -> usize {
    24
}

fn default_graph_extraction_max_relationships() -> usize {
    32
}

fn default_graph_extraction_max_claims() -> usize {
    32
}

fn default_graph_extraction_max_retries() -> usize {
    1
}

fn default_graph_extraction_max_output_tokens() -> u32 {
    2_048
}

fn default_graph_extraction_max_response_chars() -> usize {
    32 * 1024
}

fn default_graph_extraction_max_error_chars() -> usize {
    256
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RerankStrategy {
    #[default]
    Endpoint,
    Llm,
}

#[derive(Debug, Clone, Serialize)]
pub struct RerankConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub strategy: RerankStrategy,
    #[serde(default = "default_rerank_provider")]
    pub provider: String,
    #[serde(default = "default_rerank_base_url")]
    pub base_url: String,
    #[serde(default = "default_rerank_model")]
    pub model: String,
    #[serde(default = "default_rerank_top_n")]
    pub top_n: usize,
    #[serde(default = "default_rerank_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_rerank_capability_cache_ttl_seconds")]
    pub capability_cache_ttl_seconds: u64,
    #[serde(default, flatten)]
    pub endpoint_runtime: ModelEndpointRuntimeConfig,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            strategy: RerankStrategy::Endpoint,
            provider: default_rerank_provider(),
            base_url: default_rerank_base_url(),
            model: default_rerank_model(),
            top_n: 12,
            timeout_seconds: default_rerank_timeout_seconds(),
            api_key: String::new(),
            capability_cache_ttl_seconds: default_rerank_capability_cache_ttl_seconds(),
            endpoint_runtime: ModelEndpointRuntimeConfig::default(),
        }
    }
}

impl<'de> Deserialize<'de> for RerankConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        struct RawRerankConfig {
            #[serde(default)]
            enabled: Option<bool>,
            #[serde(default)]
            strategy: Option<RerankStrategy>,
            #[serde(default)]
            provider: Option<String>,
            #[serde(default)]
            base_url: Option<String>,
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            top_n: Option<usize>,
            #[serde(default)]
            timeout_seconds: Option<u64>,
            #[serde(default)]
            api_key: Option<String>,
            #[serde(default)]
            capability_cache_ttl_seconds: Option<u64>,
            #[serde(default, flatten)]
            endpoint_runtime: ModelEndpointRuntimeConfig,
        }

        let raw = RawRerankConfig::deserialize(deserializer)?;
        let endpoint_or_model_configured = raw.base_url.is_some() || raw.model.is_some();
        let strategy = raw.strategy.unwrap_or_default();
        if strategy == RerankStrategy::Llm
            && raw.enabled != Some(false)
            && (raw.base_url.is_none() || raw.model.is_none())
        {
            return Err(de::Error::custom(
                "rerank strategy 'llm' requires explicit rerank.base_url and rerank.model",
            ));
        }

        Ok(Self {
            enabled: raw.enabled.unwrap_or(endpoint_or_model_configured),
            strategy,
            provider: raw.provider.unwrap_or_else(default_rerank_provider),
            base_url: raw.base_url.unwrap_or_else(default_rerank_base_url),
            model: raw.model.unwrap_or_else(default_rerank_model),
            top_n: raw.top_n.unwrap_or_else(default_rerank_top_n),
            timeout_seconds: raw
                .timeout_seconds
                .unwrap_or_else(default_rerank_timeout_seconds),
            api_key: raw.api_key.unwrap_or_default(),
            capability_cache_ttl_seconds: raw
                .capability_cache_ttl_seconds
                .unwrap_or_else(default_rerank_capability_cache_ttl_seconds),
            endpoint_runtime: raw.endpoint_runtime,
        })
    }
}

fn default_rerank_provider() -> String {
    "vllm".into()
}

fn default_rerank_base_url() -> String {
    "http://127.0.0.1:8003".into()
}

fn default_rerank_model() -> String {
    "Qwen/Qwen3-Reranker-4B".into()
}

fn default_rerank_top_n() -> usize {
    12
}

fn default_rerank_timeout_seconds() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    pub enabled: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_openai_provider")]
    pub provider: String,
    #[serde(default = "default_chat_base_url")]
    pub base_url: String,
    #[serde(default = "default_chat_model")]
    pub model: String,
    #[serde(default)]
    pub temperature: f32,
    #[serde(default = "default_chat_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub vision_attachments: ChatVisionAttachmentConfig,
    #[serde(default, flatten)]
    pub endpoint_runtime: ModelEndpointRuntimeConfig,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: default_openai_provider(),
            base_url: default_chat_base_url(),
            model: default_chat_model(),
            temperature: 0.0,
            timeout_seconds: default_chat_timeout_seconds(),
            api_key: String::new(),
            vision_attachments: ChatVisionAttachmentConfig::default(),
            endpoint_runtime: ModelEndpointRuntimeConfig::default(),
        }
    }
}

fn default_chat_base_url() -> String {
    "http://127.0.0.1:8000/v1".into()
}

fn default_chat_model() -> String {
    "Qwen/Qwen3.6-27B".into()
}

fn default_chat_timeout_seconds() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatVisionAttachmentConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub model_supports_vision: bool,
    #[serde(default = "default_chat_vision_attachment_max_images")]
    pub max_images: usize,
    #[serde(default = "default_chat_vision_attachment_max_total_bytes")]
    pub max_total_bytes: usize,
    #[serde(default = "default_chat_vision_attachment_detail")]
    pub detail: String,
}

impl Default for ChatVisionAttachmentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model_supports_vision: false,
            max_images: default_chat_vision_attachment_max_images(),
            max_total_bytes: default_chat_vision_attachment_max_total_bytes(),
            detail: default_chat_vision_attachment_detail(),
        }
    }
}

impl ChatVisionAttachmentConfig {
    pub fn can_attach_images(&self) -> bool {
        self.enabled
            && self.model_supports_vision
            && self.max_images > 0
            && self.max_total_bytes > 0
    }

    pub fn detail_value(&self) -> Option<String> {
        let detail = self.detail.trim();
        if detail.is_empty() {
            None
        } else {
            Some(detail.to_string())
        }
    }
}

fn default_chat_vision_attachment_max_images() -> usize {
    2
}

fn default_chat_vision_attachment_max_total_bytes() -> usize {
    8 * 1024 * 1024
}

fn default_chat_vision_attachment_detail() -> String {
    "auto".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_openai_provider")]
    pub provider: String,
    #[serde(default = "default_chat_base_url")]
    pub base_url: String,
    #[serde(default = "default_chat_model")]
    pub model: String,
    #[serde(default)]
    pub temperature: f32,
    #[serde(default = "default_vision_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, flatten)]
    pub endpoint_runtime: ModelEndpointRuntimeConfig,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_openai_provider(),
            base_url: default_chat_base_url(),
            model: default_chat_model(),
            temperature: 0.0,
            timeout_seconds: default_vision_timeout_seconds(),
            api_key: String::new(),
            endpoint_runtime: ModelEndpointRuntimeConfig::default(),
        }
    }
}

fn default_vision_timeout_seconds() -> u64 {
    180
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ocr_provider")]
    pub provider: String,
    #[serde(default = "default_ocr_engine")]
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    #[serde(default = "default_ocr_language")]
    pub language: String,
    #[serde(default = "default_ocr_profile")]
    pub profile: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_ocr_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_ocr_max_stdout_bytes")]
    pub max_stdout_bytes: usize,
    #[serde(default = "default_ocr_max_stderr_bytes")]
    pub max_stderr_bytes: usize,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_ocr_provider(),
            engine: default_ocr_engine(),
            engine_version: None,
            language: default_ocr_language(),
            profile: default_ocr_profile(),
            command: String::new(),
            args: Vec::new(),
            timeout_seconds: default_ocr_timeout_seconds(),
            max_stdout_bytes: default_ocr_max_stdout_bytes(),
            max_stderr_bytes: default_ocr_max_stderr_bytes(),
        }
    }
}

impl OcrConfig {
    pub fn profile(&self) -> OcrProfile {
        OcrProfile {
            provider: self.provider.clone(),
            engine: self.engine.clone(),
            engine_version: self.engine_version.clone(),
            language: self.language.clone(),
            profile: self.profile.clone(),
        }
    }
}

fn default_ocr_provider() -> String {
    "external_command".into()
}

fn default_ocr_engine() -> String {
    "external".into()
}

fn default_ocr_language() -> String {
    "eng".into()
}

fn default_ocr_profile() -> String {
    "default".into()
}

fn default_ocr_timeout_seconds() -> u64 {
    120
}

fn default_ocr_max_stdout_bytes() -> usize {
    4 * 1024 * 1024
}

fn default_ocr_max_stderr_bytes() -> usize {
    64 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifierConfig {
    pub enabled: bool,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_qdrant_url")]
    pub url: String,
    #[serde(default = "default_qdrant_collection")]
    pub collection: String,
    #[serde(default)]
    pub prefer_for_search: bool,
    #[serde(default = "default_qdrant_timeout_seconds")]
    pub timeout_seconds: u64,
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: default_qdrant_url(),
            collection: default_qdrant_collection(),
            prefer_for_search: false,
            timeout_seconds: default_qdrant_timeout_seconds(),
        }
    }
}

fn default_qdrant_url() -> String {
    "http://rpi4b:6334".into()
}

fn default_qdrant_collection() -> String {
    "verbatim".into()
}

fn default_qdrant_timeout_seconds() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub bind: String,
    #[serde(default)]
    pub resources: DaemonResourceConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7700".into(),
            resources: DaemonResourceConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonResourceConfig {
    #[serde(default = "default_sqlite_writer_concurrency")]
    pub sqlite_writer_concurrency: usize,
    #[serde(default = "default_sqlite_writer_queue_capacity")]
    pub sqlite_writer_queue_capacity: usize,
    #[serde(default = "default_resource_queue_timeout_seconds")]
    pub sqlite_writer_queue_timeout_seconds: u64,
    #[serde(default = "default_sqlite_reader_concurrency")]
    pub sqlite_reader_concurrency: usize,
    #[serde(default = "default_sqlite_reader_queue_capacity")]
    pub sqlite_reader_queue_capacity: usize,
    #[serde(default = "default_resource_queue_timeout_seconds")]
    pub sqlite_reader_queue_timeout_seconds: u64,
    #[serde(default = "default_cpu_worker_concurrency")]
    pub cpu_worker_concurrency: usize,
    #[serde(default = "default_cpu_worker_queue_capacity")]
    pub cpu_worker_queue_capacity: usize,
    #[serde(default = "default_resource_queue_timeout_seconds")]
    pub cpu_worker_queue_timeout_seconds: u64,
    #[serde(default = "default_index_publish_concurrency")]
    pub index_publish_concurrency: usize,
    #[serde(default = "default_index_publish_queue_capacity")]
    pub index_publish_queue_capacity: usize,
    #[serde(default = "default_resource_queue_timeout_seconds")]
    pub index_publish_queue_timeout_seconds: u64,
    #[serde(default = "default_qdrant_upsert_concurrency")]
    pub qdrant_upsert_concurrency: usize,
    #[serde(default = "default_qdrant_upsert_queue_capacity")]
    pub qdrant_upsert_queue_capacity: usize,
    #[serde(default = "default_resource_queue_timeout_seconds")]
    pub qdrant_upsert_queue_timeout_seconds: u64,
}

impl Default for DaemonResourceConfig {
    fn default() -> Self {
        Self {
            sqlite_writer_concurrency: default_sqlite_writer_concurrency(),
            sqlite_writer_queue_capacity: default_sqlite_writer_queue_capacity(),
            sqlite_writer_queue_timeout_seconds: default_resource_queue_timeout_seconds(),
            sqlite_reader_concurrency: default_sqlite_reader_concurrency(),
            sqlite_reader_queue_capacity: default_sqlite_reader_queue_capacity(),
            sqlite_reader_queue_timeout_seconds: default_resource_queue_timeout_seconds(),
            cpu_worker_concurrency: default_cpu_worker_concurrency(),
            cpu_worker_queue_capacity: default_cpu_worker_queue_capacity(),
            cpu_worker_queue_timeout_seconds: default_resource_queue_timeout_seconds(),
            index_publish_concurrency: default_index_publish_concurrency(),
            index_publish_queue_capacity: default_index_publish_queue_capacity(),
            index_publish_queue_timeout_seconds: default_resource_queue_timeout_seconds(),
            qdrant_upsert_concurrency: default_qdrant_upsert_concurrency(),
            qdrant_upsert_queue_capacity: default_qdrant_upsert_queue_capacity(),
            qdrant_upsert_queue_timeout_seconds: default_resource_queue_timeout_seconds(),
        }
    }
}

impl DaemonResourceConfig {
    pub fn bounded(&self) -> Self {
        Self {
            sqlite_writer_concurrency: self.sqlite_writer_concurrency.max(1),
            sqlite_writer_queue_capacity: self.sqlite_writer_queue_capacity.max(1),
            sqlite_writer_queue_timeout_seconds: self.sqlite_writer_queue_timeout_seconds.max(1),
            sqlite_reader_concurrency: self.sqlite_reader_concurrency.max(1),
            sqlite_reader_queue_capacity: self.sqlite_reader_queue_capacity.max(1),
            sqlite_reader_queue_timeout_seconds: self.sqlite_reader_queue_timeout_seconds.max(1),
            cpu_worker_concurrency: self.cpu_worker_concurrency.max(1),
            cpu_worker_queue_capacity: self.cpu_worker_queue_capacity.max(1),
            cpu_worker_queue_timeout_seconds: self.cpu_worker_queue_timeout_seconds.max(1),
            index_publish_concurrency: self.index_publish_concurrency.max(1),
            index_publish_queue_capacity: self.index_publish_queue_capacity.max(1),
            index_publish_queue_timeout_seconds: self.index_publish_queue_timeout_seconds.max(1),
            qdrant_upsert_concurrency: self.qdrant_upsert_concurrency.max(1),
            qdrant_upsert_queue_capacity: self.qdrant_upsert_queue_capacity.max(1),
            qdrant_upsert_queue_timeout_seconds: self.qdrant_upsert_queue_timeout_seconds.max(1),
        }
    }
}

fn default_sqlite_writer_concurrency() -> usize {
    1
}

fn default_sqlite_writer_queue_capacity() -> usize {
    512
}

fn default_sqlite_reader_concurrency() -> usize {
    4
}

fn default_sqlite_reader_queue_capacity() -> usize {
    512
}

fn default_cpu_worker_concurrency() -> usize {
    2
}

fn default_cpu_worker_queue_capacity() -> usize {
    128
}

fn default_index_publish_concurrency() -> usize {
    1
}

fn default_index_publish_queue_capacity() -> usize {
    128
}

fn default_qdrant_upsert_concurrency() -> usize {
    1
}

fn default_qdrant_upsert_queue_capacity() -> usize {
    128
}

fn default_resource_queue_timeout_seconds() -> u64 {
    DEFAULT_RESOURCE_QUEUE_TIMEOUT_SECONDS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionWatcherConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_collection_watcher_debounce_millis")]
    pub debounce_millis: u64,
    #[serde(default = "default_collection_watcher_max_depth")]
    pub max_depth: usize,
    #[serde(default = "default_collection_watcher_max_queued_tasks")]
    pub max_queued_tasks: usize,
    #[serde(default)]
    pub ignore_collections: Vec<String>,
    #[serde(default)]
    pub ignore_paths: Vec<String>,
}

impl Default for CollectionWatcherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            debounce_millis: default_collection_watcher_debounce_millis(),
            max_depth: default_collection_watcher_max_depth(),
            max_queued_tasks: default_collection_watcher_max_queued_tasks(),
            ignore_collections: Vec::new(),
            ignore_paths: Vec::new(),
        }
    }
}

fn default_collection_watcher_debounce_millis() -> u64 {
    500
}

fn default_collection_watcher_max_depth() -> usize {
    crate::collection::DEFAULT_COLLECTION_SYNC_MAX_DEPTH
}

fn default_collection_watcher_max_queued_tasks() -> usize {
    128
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliConfig {
    #[serde(default = "default_task_wait_timeout_seconds")]
    pub task_wait_timeout_seconds: u64,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            task_wait_timeout_seconds: default_task_wait_timeout_seconds(),
        }
    }
}

fn default_task_wait_timeout_seconds() -> u64 {
    DEFAULT_TASK_WAIT_TIMEOUT_SECONDS
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("VERBATIM_CONFIG") {
        return PathBuf::from(path);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("verbatim")
        .join("config.toml")
}

pub fn data_dir(config: &Config) -> PathBuf {
    expand_tilde(&config.store.path)
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| "failed to parse config TOML")?;
        Ok(config)
    }

    pub fn show(&self) -> Result<String> {
        toml::to_string_pretty(self).with_context(|| "failed to serialize config")
    }

    pub fn redacted_json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}));
        redact_secrets(&mut value);
        value
    }

    pub fn reload_plan(&self, candidate: &Self) -> Result<ConfigReloadPlan> {
        let previous = serde_json::to_value(self).context("serialize active config")?;
        let next = serde_json::to_value(candidate).context("serialize candidate config")?;
        let mut changed_keys = Vec::new();
        collect_changed_config_keys("", &previous, &next, &mut changed_keys);
        changed_keys.sort();

        let mut reload_safe_keys = Vec::new();
        let mut restart_required_keys = Vec::new();
        for key in changed_keys {
            if is_reload_safe_key(&key) {
                reload_safe_keys.push(key);
            } else {
                restart_required_keys.push(ConfigRestartRequiredKey {
                    key: key.clone(),
                    reason: restart_required_reason(&key).to_string(),
                });
            }
        }

        Ok(ConfigReloadPlan {
            reload_safe_keys,
            restart_required_keys,
        })
    }

    pub fn apply_reload_safe_changes(&self, candidate: &Self) -> Self {
        let mut next = self.clone();

        next.embedding.enabled = candidate.embedding.enabled;
        next.embedding.base_url = candidate.embedding.base_url.clone();
        next.embedding.batch_size = candidate.embedding.batch_size;
        next.embedding.timeout_seconds = candidate.embedding.timeout_seconds;
        next.embedding.api_key = candidate.embedding.api_key.clone();
        next.embedding.capability_cache_ttl_seconds =
            candidate.embedding.capability_cache_ttl_seconds;
        next.embedding.endpoint_runtime = candidate.embedding.endpoint_runtime.clone();

        next.retrieval = candidate.retrieval.clone();

        next.graph.enabled = candidate.graph.enabled;
        next.graph.max_hops = candidate.graph.max_hops;
        next.graph.max_expanded_chunks = candidate.graph.max_expanded_chunks;
        next.graph.max_neighbors_per_seed = candidate.graph.max_neighbors_per_seed;
        next.graph.edge_types = candidate.graph.edge_types.clone();
        next.graph.global_search = candidate.graph.global_search.clone();

        next.rerank = candidate.rerank.clone();
        next.context = candidate.context.clone();
        next.vision = candidate.vision.clone();
        next.chat = candidate.chat.clone();
        next.verifier = candidate.verifier.clone();
        next.index_gc = candidate.index_gc.clone();
        next.cli = candidate.cli.clone();
        next.collection_watcher = candidate.collection_watcher.clone();

        next
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigReloadPlan {
    #[serde(default)]
    pub reload_safe_keys: Vec<String>,
    #[serde(default)]
    pub restart_required_keys: Vec<ConfigRestartRequiredKey>,
}

impl ConfigReloadPlan {
    pub fn has_reload_safe_changes(&self) -> bool {
        !self.reload_safe_keys.is_empty()
    }

    pub fn has_restart_required_changes(&self) -> bool {
        !self.restart_required_keys.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigRestartRequiredKey {
    pub key: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigReloadMetadata {
    pub active_config_path: String,
    pub loaded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reload_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reload_error: Option<String>,
    #[serde(default)]
    pub last_applied_reload_safe_keys: Vec<String>,
    #[serde(default)]
    pub last_restart_required_keys: Vec<ConfigRestartRequiredKey>,
}

fn collect_changed_config_keys(
    prefix: &str,
    previous: &serde_json::Value,
    next: &serde_json::Value,
    changed_keys: &mut Vec<String>,
) {
    match (previous, next) {
        (serde_json::Value::Object(previous_map), serde_json::Value::Object(next_map)) => {
            let mut keys = previous_map
                .keys()
                .chain(next_map.keys())
                .collect::<Vec<_>>();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child_prefix = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_changed_config_keys(
                    &child_prefix,
                    previous_map.get(key).unwrap_or(&serde_json::Value::Null),
                    next_map.get(key).unwrap_or(&serde_json::Value::Null),
                    changed_keys,
                );
            }
        }
        _ if previous != next => changed_keys.push(prefix.to_string()),
        _ => {}
    }
}

fn is_reload_safe_key(key: &str) -> bool {
    matches!(
        key,
        "embedding.base_url"
            | "embedding.enabled"
            | "embedding.batch_size"
            | "embedding.timeout_seconds"
            | "embedding.api_key"
            | "embedding.capability_cache_ttl_seconds"
            | "embedding.max_concurrent_requests"
            | "embedding.queue_timeout_seconds"
            | "embedding.retry.max_retries"
            | "embedding.retry.initial_backoff_millis"
            | "embedding.retry.max_backoff_millis"
            | "retrieval.dense_top_k"
            | "retrieval.bm25_top_k"
            | "retrieval.rrf_k"
            | "retrieval.default_limit"
            | "retrieval.default_page_size"
            | "graph.enabled"
            | "graph.max_hops"
            | "graph.max_expanded_chunks"
            | "graph.max_neighbors_per_seed"
            | "graph.edge_types"
            | "graph.global_search.enabled"
            | "graph.global_search.max_communities"
            | "graph.global_search.max_report_claims"
            | "graph.global_search.max_report_chars"
            | "graph.global_search.max_evidence_per_report"
            | "graph.global_search.max_search_results"
            | "graph.global_search.drift.enabled"
            | "graph.global_search.drift.max_subqueries"
            | "rerank.enabled"
            | "rerank.strategy"
            | "rerank.provider"
            | "rerank.base_url"
            | "rerank.model"
            | "rerank.top_n"
            | "rerank.timeout_seconds"
            | "rerank.api_key"
            | "rerank.capability_cache_ttl_seconds"
            | "rerank.max_concurrent_requests"
            | "rerank.queue_timeout_seconds"
            | "rerank.retry.max_retries"
            | "rerank.retry.initial_backoff_millis"
            | "rerank.retry.max_backoff_millis"
            | "context.enabled"
            | "vision.enabled"
            | "vision.provider"
            | "vision.base_url"
            | "vision.model"
            | "vision.temperature"
            | "vision.timeout_seconds"
            | "vision.api_key"
            | "vision.max_concurrent_requests"
            | "vision.queue_timeout_seconds"
            | "vision.retry.max_retries"
            | "vision.retry.initial_backoff_millis"
            | "vision.retry.max_backoff_millis"
            | "chat.enabled"
            | "chat.provider"
            | "chat.base_url"
            | "chat.model"
            | "chat.temperature"
            | "chat.timeout_seconds"
            | "chat.api_key"
            | "chat.max_concurrent_requests"
            | "chat.queue_timeout_seconds"
            | "chat.retry.max_retries"
            | "chat.retry.initial_backoff_millis"
            | "chat.retry.max_backoff_millis"
            | "chat.vision_attachments.enabled"
            | "chat.vision_attachments.model_supports_vision"
            | "chat.vision_attachments.max_images"
            | "chat.vision_attachments.max_total_bytes"
            | "chat.vision_attachments.detail"
            | "verifier.enabled"
            | "index_gc.retain_previous_generations"
            | "index_gc.stale_staging_seconds"
            | "cli.task_wait_timeout_seconds"
            | "collection_watcher.enabled"
            | "collection_watcher.debounce_millis"
            | "collection_watcher.max_depth"
            | "collection_watcher.max_queued_tasks"
            | "collection_watcher.ignore_collections"
            | "collection_watcher.ignore_paths"
    )
}

fn restart_required_reason(key: &str) -> &'static str {
    if key == "daemon.bind" {
        "daemon bind address is only read when the listener starts; restart verbatim-daemon to bind a new address"
    } else if key.starts_with("store.") {
        "store path selects persisted SQLite, vector, and lexical index data; restart with an explicit data migration or reindex plan"
    } else if key.starts_with("embedding.") {
        "embedding profile identity or vector semantics changed; rebuild vectors for a new profile before using this setting"
    } else if key.starts_with("parser.") {
        "parser and artifact settings affect persisted extracted data; reingest or restart with an explicit migration plan"
    } else if key.starts_with("graph.extraction.") {
        "graph extraction settings affect persisted graph data; reingest before treating the change as active"
    } else if key.starts_with("ocr.") {
        "OCR settings affect persisted OCR-derived text; reingest before treating the change as active"
    } else if key.starts_with("qdrant.") {
        "Qdrant settings affect external vector index integration; restart or reindex with the new target"
    } else if key.starts_with("vector_index.") {
        "vector index residency controls whether the daemon loads resident HNSW; restart verbatim-daemon to apply it"
    } else {
        "setting is not reload-safe in this version; restart verbatim-daemon to apply it"
    }
}

fn redact_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_secret_key(key) {
                    *child = serde_json::Value::String("<redacted>".to_string());
                } else {
                    redact_secrets(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_secrets(item);
            }
        }
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
    normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("authorization")
        || normalized.contains("bearer")
}

pub fn init_default_config() -> Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir: {}", parent.display()))?;
    }
    fs::write(&path, DEFAULT_CONFIG_TEMPLATE)
        .with_context(|| format!("failed to write config: {}", path.display()))?;
    Ok(path)
}

const DEFAULT_CONFIG_TEMPLATE: &str = r#"# Verbatim configuration
# See: https://github.com/RyderFreeman4Logos/verbatim

[store]
path = "~/.local/share/verbatim"

[parser]
default = "pdf_oxide"   # pdf_oxide or pdfplumber

[parser.image_artifacts]
max_images_per_source = 512
max_bytes_per_image = 16777216
max_total_bytes_per_source = 268435456
max_image_width = 10000
max_image_height = 10000
max_image_pixels = 100000000

[embedding]
profile_id = "default"
enabled = true
provider = "openai_compatible"
base_url = "http://127.0.0.1:8002/v1"
model = "Qwen/Qwen3-Embedding-8B"
dimension = 4096
normalize = true
query_instruction = "Given a user's question about a document, retrieve the exact passages that directly support a grounded answer with source-level citations."
document_instruction = ""
batch_size = 16
timeout_seconds = 120
api_key = ""
capability_cache_ttl_seconds = 60
max_concurrent_requests = 4
queue_timeout_seconds = 300

[embedding.retry]
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[retrieval]
dense_top_k = 80
bm25_top_k = 50
rrf_k = 60
default_limit = 12
default_page_size = 1

[vector_index]
# low_memory scans SQLite-stored vectors at query time and avoids resident HNSW.
# resident_hnsw loads the published local HNSW index into daemon memory.
residency = "low_memory"

[graph]
enabled = true
max_hops = 1
max_expanded_chunks = 30
max_neighbors_per_seed = 6
edge_types = ["parent", "previous", "next", "section_contains", "page_contains_image", "image_near_text", "markdown_links_to"]

[graph.extraction]
enabled = false
max_chunks = 8
max_chunk_chars = 3000
max_entities = 24
max_relationships = 32
max_claims = 32
max_retries = 1
max_output_tokens = 2048
max_response_chars = 32768
max_error_chars = 256

[graph.global_search]
enabled = false
max_communities = 128
max_report_claims = 12
max_report_chars = 4000
max_evidence_per_report = 12
max_search_results = 4

[graph.global_search.drift]
enabled = false
max_subqueries = 4

[rerank]
enabled = false
strategy = "endpoint"          # endpoint | llm
provider = "vllm"              # vllm | cohere | jina
base_url = "http://127.0.0.1:8003"
model = "Qwen/Qwen3-Reranker-4B"
top_n = 12
timeout_seconds = 120
api_key = ""
capability_cache_ttl_seconds = 60
max_concurrent_requests = 4
queue_timeout_seconds = 300

[rerank.retry]
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[context]
enabled = true

[vision]
enabled = false
provider = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "Qwen/Qwen3.6-27B"
temperature = 0.0
timeout_seconds = 180
api_key = ""
max_concurrent_requests = 4
queue_timeout_seconds = 300

[vision.retry]
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[ocr]
enabled = false
provider = "external_command"
engine = "external"
language = "eng"
profile = "default"
command = ""
args = []
timeout_seconds = 120
max_stdout_bytes = 4194304
max_stderr_bytes = 65536

[chat]
enabled = true
provider = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "Qwen/Qwen3.6-27B"
temperature = 0.0
timeout_seconds = 120
api_key = ""
max_concurrent_requests = 4
queue_timeout_seconds = 300

[chat.retry]
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[chat.vision_attachments]
enabled = false
model_supports_vision = false
max_images = 2
max_total_bytes = 8388608
detail = "auto"

[verifier]
enabled = true

[qdrant]
enabled = false
url = "http://rpi4b:6334"
collection = "verbatim"
prefer_for_search = false
timeout_seconds = 5

[index_gc]
retain_previous_generations = 2
stale_staging_seconds = 86400

[cli]
# Caps `verbatim task wait`. Model timeout_seconds values above bound provider
# calls and finite daemon HTTP requests; they do not cap task wait streams.
task_wait_timeout_seconds = 1500

[daemon]
bind = "127.0.0.1:7700"

[collection_watcher]
enabled = true
debounce_millis = 500
max_depth = 32
max_queued_tasks = 128
ignore_collections = []
ignore_paths = []
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_template() {
        let config: Config = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();
        assert_eq!(config.store.path, "~/.local/share/verbatim");
        assert_eq!(config.parser.default, "pdf_oxide");
        assert_eq!(config.parser.image_artifacts.max_images_per_source, 512);
        assert_eq!(
            config.parser.image_artifacts.max_bytes_per_image,
            16 * 1024 * 1024
        );
        assert_eq!(
            config.parser.image_artifacts.max_total_bytes_per_source,
            256 * 1024 * 1024
        );
        assert_eq!(config.parser.image_artifacts.max_image_width, 10_000);
        assert_eq!(config.parser.image_artifacts.max_image_height, 10_000);
        assert_eq!(config.parser.image_artifacts.max_image_pixels, 100_000_000);
        assert!(config.embedding.enabled);
        assert_eq!(config.embedding.provider, "openai_compatible");
        assert_eq!(config.embedding.base_url, "http://127.0.0.1:8002/v1");
        assert_eq!(config.embedding.dimension, 4096);
        assert!(config.embedding.normalize);
        assert_eq!(config.embedding.batch_size, 16);
        assert_eq!(
            config.embedding.capability_cache_ttl_seconds,
            DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS
        );
        assert_eq!(
            config.embedding.endpoint_runtime.max_concurrent_requests,
            DEFAULT_MODEL_ENDPOINT_MAX_CONCURRENT_REQUESTS
        );
        assert_eq!(
            config.embedding.endpoint_runtime.queue_timeout_seconds,
            DEFAULT_MODEL_ENDPOINT_QUEUE_TIMEOUT_SECONDS
        );
        assert_eq!(
            config.embedding.endpoint_runtime.retry.max_retries,
            DEFAULT_MODEL_RETRY_MAX_RETRIES
        );
        assert_eq!(config.retrieval.dense_top_k, 80);
        assert_eq!(config.retrieval.default_limit, 12);
        assert_eq!(config.retrieval.default_page_size, 1);
        assert_eq!(
            config.vector_index.residency,
            VectorIndexResidency::LowMemory
        );
        assert!(config.graph.enabled);
        assert_eq!(config.graph.max_hops, 1);
        assert_eq!(config.graph.max_expanded_chunks, 30);
        assert_eq!(config.graph.max_neighbors_per_seed, 6);
        assert_eq!(
            config.graph.edge_types,
            vec![
                EdgeType::Parent,
                EdgeType::Previous,
                EdgeType::Next,
                EdgeType::SectionContains,
                EdgeType::PageContainsImage,
                EdgeType::ImageNearText,
                EdgeType::MarkdownLinksTo,
            ]
        );
        assert!(!config.graph.extraction.enabled);
        assert_eq!(config.graph.extraction.max_chunks, 8);
        assert_eq!(config.graph.extraction.max_chunk_chars, 3_000);
        assert_eq!(config.graph.extraction.max_entities, 24);
        assert_eq!(config.graph.extraction.max_relationships, 32);
        assert_eq!(config.graph.extraction.max_claims, 32);
        assert_eq!(config.graph.extraction.max_retries, 1);
        assert_eq!(config.graph.extraction.max_output_tokens, 2_048);
        assert_eq!(config.graph.extraction.max_response_chars, 32 * 1024);
        assert_eq!(config.graph.extraction.max_error_chars, 256);
        assert!(!config.graph.global_search.enabled);
        assert_eq!(config.graph.global_search.max_communities, 128);
        assert_eq!(config.graph.global_search.max_report_claims, 12);
        assert_eq!(config.graph.global_search.max_report_chars, 4_000);
        assert_eq!(config.graph.global_search.max_evidence_per_report, 12);
        assert_eq!(config.graph.global_search.max_search_results, 4);
        assert!(!config.graph.global_search.drift.enabled);
        assert_eq!(config.graph.global_search.drift.max_subqueries, 4);
        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.strategy, RerankStrategy::Endpoint);
        assert_eq!(config.rerank.provider, "vllm");
        assert_eq!(config.rerank.base_url, "http://127.0.0.1:8003");
        assert_eq!(config.rerank.model, "Qwen/Qwen3-Reranker-4B");
        assert_eq!(config.rerank.top_n, 12);
        assert_eq!(
            config.rerank.capability_cache_ttl_seconds,
            DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS
        );
        assert_eq!(
            config.rerank.endpoint_runtime.max_concurrent_requests,
            DEFAULT_MODEL_ENDPOINT_MAX_CONCURRENT_REQUESTS
        );
        assert!(config.context.enabled);
        assert!(!config.vision.enabled);
        assert_eq!(config.vision.timeout_seconds, 180);
        assert_eq!(
            config.vision.endpoint_runtime.queue_timeout_seconds,
            DEFAULT_MODEL_ENDPOINT_QUEUE_TIMEOUT_SECONDS
        );
        assert!(!config.ocr.enabled);
        assert_eq!(config.ocr.provider, "external_command");
        assert_eq!(config.ocr.language, "eng");
        assert_eq!(config.ocr.timeout_seconds, 120);
        assert_eq!(config.ocr.max_stdout_bytes, 4 * 1024 * 1024);
        assert_eq!(config.ocr.max_stderr_bytes, 64 * 1024);
        assert!(config.chat.enabled);
        assert_eq!(config.chat.base_url, "http://127.0.0.1:8000/v1");
        assert_eq!(
            config.chat.endpoint_runtime.retry.initial_backoff_millis,
            DEFAULT_MODEL_RETRY_INITIAL_BACKOFF_MILLIS
        );
        assert!(!config.chat.vision_attachments.enabled);
        assert!(!config.chat.vision_attachments.model_supports_vision);
        assert_eq!(config.chat.vision_attachments.max_images, 2);
        assert_eq!(
            config.chat.vision_attachments.max_total_bytes,
            8 * 1024 * 1024
        );
        assert_eq!(config.chat.vision_attachments.detail, "auto");
        assert!(config.verifier.enabled);
        assert!(!config.qdrant.enabled);
        assert_eq!(config.qdrant.url, "http://rpi4b:6334");
        assert_eq!(config.qdrant.collection, "verbatim");
        assert!(!config.qdrant.prefer_for_search);
        assert_eq!(config.qdrant.timeout_seconds, 5);
        assert_eq!(config.index_gc.retain_previous_generations, 2);
        assert_eq!(config.index_gc.stale_staging_seconds, 86_400);
        assert!(config.collection_watcher.enabled);
        assert_eq!(config.collection_watcher.debounce_millis, 500);
        assert_eq!(config.collection_watcher.max_depth, 32);
        assert_eq!(config.collection_watcher.max_queued_tasks, 128);
        assert_eq!(config.cli.task_wait_timeout_seconds, 1500);
        assert_eq!(config.daemon.bind, "127.0.0.1:7700");
    }

    #[test]
    fn partial_cli_config_defaults_task_wait_timeout() {
        let config: Config = toml::from_str(
            r#"
[cli]
"#,
        )
        .unwrap();

        assert_eq!(config.cli.task_wait_timeout_seconds, 1500);
    }

    #[test]
    fn absent_cli_config_uses_task_wait_timeout_default() {
        let config: Config = toml::from_str(
            r#"
[daemon]
bind = "127.0.0.1:7701"
"#,
        )
        .unwrap();

        assert_eq!(config.cli.task_wait_timeout_seconds, 1500);
    }

    #[test]
    fn partial_qdrant_config_defaults_disabled_and_bounded() {
        let config: Config = toml::from_str(
            r#"
[qdrant]
enabled = true
collection = "custom"
"#,
        )
        .unwrap();

        assert!(config.qdrant.enabled);
        assert_eq!(config.qdrant.url, "http://rpi4b:6334");
        assert_eq!(config.qdrant.collection, "custom");
        assert!(!config.qdrant.prefer_for_search);
        assert_eq!(config.qdrant.timeout_seconds, 5);
    }

    #[test]
    fn partial_vision_config_defaults_disabled() {
        let config: Config = toml::from_str(
            r#"
[vision]
model = "Qwen/Qwen3.6-27B"
"#,
        )
        .unwrap();

        assert!(!config.vision.enabled);
        assert_eq!(config.vision.model, "Qwen/Qwen3.6-27B");
        assert!(!config.embedding.enabled);
        assert!(config.chat.enabled);
    }

    #[test]
    fn partial_ocr_config_defaults_disabled_and_explicit() {
        let config: Config = toml::from_str(
            r#"
[ocr]
enabled = true
command = "fixture-ocr"
"#,
        )
        .unwrap();

        assert!(config.ocr.enabled);
        assert_eq!(config.ocr.provider, "external_command");
        assert_eq!(config.ocr.engine, "external");
        assert_eq!(config.ocr.language, "eng");
        assert_eq!(config.ocr.profile, "default");
        assert_eq!(config.ocr.command, "fixture-ocr");
        assert_eq!(config.ocr.timeout_seconds, 120);
        assert_eq!(config.ocr.max_stdout_bytes, 4 * 1024 * 1024);
        assert_eq!(config.ocr.max_stderr_bytes, 64 * 1024);
    }

    #[test]
    fn partial_graph_config_defaults_to_bounded_expansion() {
        let config: Config = toml::from_str(
            r#"
[graph]
enabled = false
"#,
        )
        .unwrap();

        assert!(!config.graph.enabled);
        assert_eq!(config.graph.max_hops, 1);
        assert_eq!(config.graph.max_expanded_chunks, 30);
        assert_eq!(config.graph.max_neighbors_per_seed, 6);
        assert!(config.graph.edge_types.contains(&EdgeType::Parent));
        assert!(!config.graph.extraction.enabled);
        assert_eq!(config.graph.extraction.max_chunks, 8);
        assert!(!config.graph.global_search.enabled);
        assert_eq!(config.graph.global_search.max_search_results, 4);
    }

    #[test]
    fn partial_graph_extraction_config_defaults_disabled_and_bounded() {
        let config: Config = toml::from_str(
            r#"
[graph.extraction]
enabled = true
max_chunks = 2
"#,
        )
        .unwrap();

        assert!(config.graph.extraction.enabled);
        assert_eq!(config.graph.extraction.max_chunks, 2);
        assert_eq!(config.graph.extraction.max_chunk_chars, 3_000);
        assert_eq!(config.graph.extraction.max_entities, 24);
        assert_eq!(config.graph.extraction.max_relationships, 32);
        assert_eq!(config.graph.extraction.max_claims, 32);
        assert_eq!(config.graph.extraction.max_retries, 1);
        assert_eq!(config.graph.extraction.max_output_tokens, 2_048);
        assert_eq!(config.graph.extraction.max_response_chars, 32 * 1024);
        assert_eq!(config.graph.extraction.max_error_chars, 256);
    }

    #[test]
    fn partial_global_search_config_defaults_disabled_and_bounded() {
        let config: Config = toml::from_str(
            r#"
[graph.global_search]
enabled = true
max_search_results = 2
"#,
        )
        .unwrap();

        assert!(config.graph.global_search.enabled);
        assert_eq!(config.graph.global_search.max_search_results, 2);
        assert_eq!(config.graph.global_search.max_communities, 128);
        assert_eq!(config.graph.global_search.max_report_claims, 12);
        assert_eq!(config.graph.global_search.max_report_chars, 4_000);
        assert_eq!(config.graph.global_search.max_evidence_per_report, 12);
        assert!(!config.graph.global_search.drift.enabled);
        assert_eq!(config.graph.global_search.drift.max_subqueries, 4);
    }

    #[test]
    fn partial_rerank_endpoint_config_auto_enables_when_enabled_omitted() {
        let config: Config = toml::from_str(
            r#"
[rerank]
model = "custom-reranker"
"#,
        )
        .unwrap();

        assert!(config.rerank.enabled);
        assert_eq!(config.rerank.strategy, RerankStrategy::Endpoint);
        assert_eq!(config.rerank.provider, "vllm");
        assert_eq!(config.rerank.base_url, "http://127.0.0.1:8003");
        assert_eq!(config.rerank.model, "custom-reranker");
        assert_eq!(config.rerank.top_n, 12);
        assert_eq!(
            config.rerank.capability_cache_ttl_seconds,
            DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS
        );
    }

    #[test]
    fn absent_rerank_config_remains_disabled_despite_defaults() {
        let config: Config = toml::from_str(
            r#"
[retrieval]
dense_top_k = 4
bm25_top_k = 4
rrf_k = 60
"#,
        )
        .unwrap();

        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.base_url, "http://127.0.0.1:8003");
        assert_eq!(config.rerank.model, "Qwen/Qwen3-Reranker-4B");
    }

    #[test]
    fn chat_config_does_not_implicitly_select_llm_rerank() {
        let config: Config = toml::from_str(
            r#"
[chat]
base_url = "http://127.0.0.1:8000/v1"
model = "chat-model"
"#,
        )
        .unwrap();

        assert!(config.chat.enabled);
        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.strategy, RerankStrategy::Endpoint);
    }

    #[test]
    fn explicit_rerank_disabled_overrides_endpoint_fields() {
        let config: Config = toml::from_str(
            r#"
[rerank]
enabled = false
base_url = "http://127.0.0.1:9999"
model = "custom-reranker"
"#,
        )
        .unwrap();

        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.model, "custom-reranker");
    }

    #[test]
    fn absent_embedding_config_defaults_disabled() {
        let config: Config = toml::from_str(
            r#"
[retrieval]
dense_top_k = 4
bm25_top_k = 4
rrf_k = 60
"#,
        )
        .unwrap();

        assert!(!config.embedding.enabled);
    }

    #[test]
    fn configured_embedding_endpoint_auto_enables_when_enabled_omitted() {
        let config: Config = toml::from_str(
            r#"
[embedding]
base_url = "http://127.0.0.1:18002/v1"
model = "test-embedding"
"#,
        )
        .unwrap();

        assert!(config.embedding.enabled);
        assert_eq!(config.embedding.model, "test-embedding");
    }

    #[test]
    fn llm_rerank_requires_explicit_endpoint_and_model() {
        let error = toml::from_str::<Config>(
            r#"
[rerank]
strategy = "llm"
"#,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("requires explicit rerank.base_url and rerank.model"));
    }

    #[test]
    fn explicit_llm_rerank_config_auto_enables_when_enabled_omitted() {
        let config: Config = toml::from_str(
            r#"
[rerank]
strategy = "llm"
base_url = "http://127.0.0.1:8000/v1"
model = "chat-reranker"
"#,
        )
        .unwrap();

        assert!(config.rerank.enabled);
        assert_eq!(config.rerank.strategy, RerankStrategy::Llm);
        assert_eq!(config.rerank.model, "chat-reranker");
    }

    #[test]
    fn graph_config_rejects_unknown_edge_type() {
        let error = toml::from_str::<Config>(
            r#"
[graph]
edge_types = ["parent", "not_an_edge"]
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("not_an_edge"));
    }

    #[test]
    fn partial_chat_vision_attachment_config_defaults_disabled() {
        let config: Config = toml::from_str(
            r#"
[chat.vision_attachments]
model_supports_vision = true
"#,
        )
        .unwrap();

        assert!(!config.chat.vision_attachments.enabled);
        assert!(config.chat.vision_attachments.model_supports_vision);
        assert_eq!(config.chat.vision_attachments.max_images, 2);
        assert_eq!(
            config.chat.vision_attachments.max_total_bytes,
            8 * 1024 * 1024
        );
    }

    #[test]
    fn tilde_expansion() {
        let expanded = expand_tilde("~/.local/share/verbatim");
        assert!(expanded.to_str().unwrap().contains("share/verbatim"));
        assert!(!expanded.to_str().unwrap().starts_with("~"));
    }

    #[test]
    fn no_tilde_passthrough() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn config_roundtrip() {
        let config: Config = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();
        let serialized = config.show().unwrap();
        let _reparsed: Config = toml::from_str(&serialized).unwrap();
    }

    #[test]
    fn default_template_places_capability_cache_ttl_under_model_tables() {
        let template: toml::Value = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();

        assert_eq!(
            template
                .get("embedding")
                .and_then(|embedding| embedding.get("capability_cache_ttl_seconds"))
                .and_then(toml::Value::as_integer),
            Some(DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS as i64)
        );
        assert_eq!(
            template
                .get("rerank")
                .and_then(|rerank| rerank.get("capability_cache_ttl_seconds"))
                .and_then(toml::Value::as_integer),
            Some(DEFAULT_RERANK_CAPABILITY_CACHE_TTL_SECONDS as i64)
        );
    }

    #[test]
    fn redacted_json_masks_secret_like_fields() {
        let mut config: Config = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();
        config.chat.api_key = "***".into();

        let redacted = config.redacted_json();

        assert_eq!(redacted["chat"]["api_key"], "<redacted>");
        assert_eq!(redacted["chat"]["base_url"], "http://127.0.0.1:8000/v1");
    }

    #[test]
    fn redact_secrets_masks_tokens_and_authorization() {
        let mut value = serde_json::json!({
            "token": "secret",
            "headers": {
                "authorization": "Bearer secret",
                "x_request_id": "safe"
            }
        });

        redact_secrets(&mut value);

        assert_eq!(value["token"], "<redacted>");
        assert_eq!(value["headers"]["authorization"], "<redacted>");
        assert_eq!(value["headers"]["x_request_id"], "safe");
    }

    #[test]
    fn reload_plan_classifies_runtime_safe_changes() {
        let current: Config = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();
        let mut candidate = current.clone();
        candidate.retrieval.dense_top_k = 24;
        candidate.chat.timeout_seconds = 30;
        candidate.chat.endpoint_runtime.max_concurrent_requests = 2;
        candidate.chat.endpoint_runtime.retry.max_retries = 1;
        candidate.embedding.base_url = "http://127.0.0.1:18002/v1".into();
        candidate.embedding.capability_cache_ttl_seconds = 5;
        candidate.embedding.endpoint_runtime.queue_timeout_seconds = 60;
        candidate.index_gc.retain_previous_generations = 1;
        candidate.index_gc.stale_staging_seconds = 3_600;

        let plan = current.reload_plan(&candidate).unwrap();

        assert_eq!(
            plan.reload_safe_keys,
            vec![
                "chat.max_concurrent_requests",
                "chat.retry.max_retries",
                "chat.timeout_seconds",
                "embedding.base_url",
                "embedding.capability_cache_ttl_seconds",
                "embedding.queue_timeout_seconds",
                "index_gc.retain_previous_generations",
                "index_gc.stale_staging_seconds",
                "retrieval.dense_top_k"
            ]
        );
        assert!(plan.restart_required_keys.is_empty());

        let applied = current.apply_reload_safe_changes(&candidate);
        assert_eq!(applied.retrieval.dense_top_k, 24);
        assert_eq!(applied.chat.timeout_seconds, 30);
        assert_eq!(applied.chat.endpoint_runtime.max_concurrent_requests, 2);
        assert_eq!(applied.chat.endpoint_runtime.retry.max_retries, 1);
        assert_eq!(applied.embedding.base_url, "http://127.0.0.1:18002/v1");
        assert_eq!(applied.embedding.capability_cache_ttl_seconds, 5);
        assert_eq!(applied.embedding.endpoint_runtime.queue_timeout_seconds, 60);
        assert_eq!(applied.index_gc.retain_previous_generations, 1);
        assert_eq!(applied.index_gc.stale_staging_seconds, 3_600);
    }

    #[test]
    fn reload_plan_classifies_restart_required_changes() {
        let current: Config = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();
        let mut candidate = current.clone();
        candidate.daemon.bind = "127.0.0.1:9900".into();
        candidate.store.path = "/srv/verbatim".into();
        candidate.embedding.model = "other-embedding-model".into();
        candidate.vector_index.residency = VectorIndexResidency::ResidentHnsw;

        let plan = current.reload_plan(&candidate).unwrap();

        assert!(plan.reload_safe_keys.is_empty());
        let keys = plan
            .restart_required_keys
            .iter()
            .map(|change| change.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "daemon.bind",
                "embedding.model",
                "store.path",
                "vector_index.residency"
            ]
        );
        assert!(plan.restart_required_keys[0].reason.contains("listener"));
        assert!(plan.restart_required_keys[1].reason.contains("vectors"));
        assert!(plan.restart_required_keys[2].reason.contains("SQLite"));
        assert!(plan.restart_required_keys[3]
            .reason
            .contains("resident HNSW"));

        let applied = current.apply_reload_safe_changes(&candidate);
        assert_eq!(applied.daemon.bind, current.daemon.bind);
        assert_eq!(applied.store.path, current.store.path);
        assert_eq!(applied.embedding.model, current.embedding.model);
        assert_eq!(
            applied.vector_index.residency,
            current.vector_index.residency
        );
    }
}
