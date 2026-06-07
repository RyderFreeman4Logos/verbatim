use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub store: StoreConfig,
    #[serde(default)]
    pub parser: ParserConfig,
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub retrieval: RetrievalConfig,
    #[serde(default)]
    pub rerank: RerankConfig,
    #[serde(default)]
    pub context: ContextConfig,
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
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            default: "pdf_oxide".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub base_url: String,
    #[serde(default = "default_embedding_model")]
    pub model: String,
    #[serde(default = "default_dimension")]
    pub dimension: usize,
    #[serde(default = "default_query_instruction")]
    pub query_instruction: String,
    #[serde(default)]
    pub document_instruction: String,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

fn default_embedding_model() -> String {
    "Qwen/Qwen3-Embedding-8B".into()
}

fn default_dimension() -> usize {
    4096
}

fn default_query_instruction() -> String {
    "Given a user's question about a document, retrieve the exact passages that directly support a grounded answer with source-level citations.".into()
}

fn default_batch_size() -> usize {
    32
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
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_rerank_top_n")]
    pub top_n: usize,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            model: String::new(),
            top_n: 12,
        }
    }
}

fn default_rerank_top_n() -> usize {
    12
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
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub temperature: f32,
    #[serde(default)]
    pub api_key: String,
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

[embedding]
base_url = "http://127.0.0.1:8001/v1"
model = "Qwen/Qwen3-Embedding-8B"
dimension = 4096
query_instruction = "Given a user's question about a document, retrieve the exact passages that directly support a grounded answer with source-level citations."
document_instruction = ""
batch_size = 32

[retrieval]
dense_top_k = 80
bm25_top_k = 50
rrf_k = 60

[rerank]
enabled = false
# base_url = "http://127.0.0.1:8003/v1"
# model = "Qwen/Qwen3-Reranker-4B"
# top_n = 12

[context]
enabled = true

[chat]
base_url = "http://127.0.0.1:8002/v1"
model = "qwen3.6-27b"
temperature = 0.0
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
        assert_eq!(config.embedding.dimension, 4096);
        assert_eq!(config.retrieval.dense_top_k, 80);
        assert!(!config.rerank.enabled);
        assert!(config.context.enabled);
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
}
