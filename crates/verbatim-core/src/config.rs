use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::image_limits::ImageArtifactLimits;

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
    pub rerank: RerankConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub vision: VisionConfig,
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
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            dense_top_k: 80,
            bm25_top_k: 50,
            rrf_k: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    pub enabled: bool,
    #[serde(default = "default_openai_provider")]
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
            provider: default_openai_provider(),
            base_url: default_rerank_base_url(),
            model: default_rerank_model(),
            top_n: 12,
            timeout_seconds: default_rerank_timeout_seconds(),
            api_key: String::new(),
        }
    }
}

fn default_rerank_base_url() -> String {
    "http://127.0.0.1:8003/v1".into()
}

fn default_rerank_model() -> String {
    "Qwen/Qwen3-Reranker-8B".into()
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
pub struct VisionConfig {
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
    #[serde(default = "default_vision_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub api_key: String,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
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
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_collection")]
    pub collection: String,
    #[serde(default)]
    pub prefer_for_search: bool,
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            collection: "verbatim".into(),
            prefer_for_search: false,
        }
    }
}

fn default_collection() -> String {
    "verbatim".into()
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

[rerank]
enabled = false
provider = "openai_compatible"
base_url = "http://127.0.0.1:8003/v1"
model = "Qwen/Qwen3-Reranker-8B"
top_n = 12
timeout_seconds = 120
api_key = ""

[context]
enabled = true

[vision]
enabled = true
provider = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "Qwen/Qwen3.6-27B"
temperature = 0.0
timeout_seconds = 180
api_key = ""

[chat]
enabled = true
provider = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "Qwen/Qwen3.6-27B"
temperature = 0.0
timeout_seconds = 120
api_key = ""

[verifier]
enabled = true

[qdrant]
enabled = false
# url = "http://rpi4b:6334"
# collection = "verbatim"
# prefer_for_search = false

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
        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.model, "Qwen/Qwen3-Reranker-8B");
        assert!(config.context.enabled);
        assert!(config.vision.enabled);
        assert_eq!(config.vision.timeout_seconds, 180);
        assert!(config.chat.enabled);
        assert_eq!(config.chat.base_url, "http://127.0.0.1:8000/v1");
        assert!(config.verifier.enabled);
        assert!(!config.qdrant.enabled);
        assert_eq!(config.daemon.bind, "127.0.0.1:7700");
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
