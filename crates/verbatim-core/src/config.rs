use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::image_limits::ImageArtifactLimits;
use crate::types::{EdgeType, EmbeddingProfileId, OcrProfile};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub daemon: DaemonConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            profile_id: EmbeddingProfileId::default_profile(),
            enabled: true,
            provider: default_openai_provider(),
            base_url: default_embedding_base_url(),
            model: default_embedding_model(),
            dimension: default_dimension(),
            normalize: true,
            query_instruction: default_query_instruction(),
            document_instruction: String::new(),
            batch_size: default_batch_size(),
            timeout_seconds: default_embedding_timeout_seconds(),
            api_key: String::new(),
        }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    #[serde(default)]
    pub enabled: bool,
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
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_rerank_provider(),
            base_url: default_rerank_base_url(),
            model: default_rerank_model(),
            top_n: 12,
            timeout_seconds: default_rerank_timeout_seconds(),
            api_key: String::new(),
        }
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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7700".into(),
        }
    }
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
default = "pdf_oxide"   # pdf_oxide | pdfplumber

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

[retrieval]
dense_top_k = 80
bm25_top_k = 50
rrf_k = 60
default_limit = 12
default_page_size = 1

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
provider = "vllm"              # vllm | cohere | jina
base_url = "http://127.0.0.1:8003"
model = "Qwen/Qwen3-Reranker-4B"
top_n = 12
timeout_seconds = 120
api_key = ""

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

[ocr]
enabled = false
provider = "external_command"
engine = "external"
language = "eng"
profile = "default"
command = ""
args = []

[chat]
enabled = true
provider = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "Qwen/Qwen3.6-27B"
temperature = 0.0
timeout_seconds = 120
api_key = ""

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

[daemon]
bind = "127.0.0.1:7700"
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
        assert_eq!(config.retrieval.dense_top_k, 80);
        assert_eq!(config.retrieval.default_limit, 12);
        assert_eq!(config.retrieval.default_page_size, 1);
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
        assert_eq!(config.rerank.provider, "vllm");
        assert_eq!(config.rerank.base_url, "http://127.0.0.1:8003");
        assert_eq!(config.rerank.model, "Qwen/Qwen3-Reranker-4B");
        assert_eq!(config.rerank.top_n, 12);
        assert!(config.context.enabled);
        assert!(!config.vision.enabled);
        assert_eq!(config.vision.timeout_seconds, 180);
        assert!(!config.ocr.enabled);
        assert_eq!(config.ocr.provider, "external_command");
        assert_eq!(config.ocr.language, "eng");
        assert!(config.chat.enabled);
        assert_eq!(config.chat.base_url, "http://127.0.0.1:8000/v1");
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
        assert_eq!(config.daemon.bind, "127.0.0.1:7700");
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
        assert!(config.embedding.enabled);
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
    fn partial_rerank_config_defaults_disabled() {
        let config: Config = toml::from_str(
            r#"
[rerank]
model = "custom-reranker"
"#,
        )
        .unwrap();

        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.provider, "vllm");
        assert_eq!(config.rerank.base_url, "http://127.0.0.1:8003");
        assert_eq!(config.rerank.model, "custom-reranker");
        assert_eq!(config.rerank.top_n, 12);
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
}
