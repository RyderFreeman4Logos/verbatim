use anyhow::Result;
use async_trait::async_trait;

use crate::config::EmbeddingConfig;
use crate::provider::openai_compatible::OpenAiCompatibleEmbeddingModel;
use crate::traits::EmbeddingClient;

pub struct OpenAiEmbeddingClient {
    model: OpenAiCompatibleEmbeddingModel,
}

impl OpenAiEmbeddingClient {
    pub fn new(config: &EmbeddingConfig) -> Self {
        Self {
            model: OpenAiCompatibleEmbeddingModel::from_config(config),
        }
    }

    pub fn prepare_query(&self, query: &str) -> String {
        self.model.prepare_query(query)
    }

    pub fn prepare_document(&self, text: &str, heading: &str) -> String {
        self.model.prepare_document(text, heading)
    }
}

#[async_trait]
impl EmbeddingClient for OpenAiEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut retries = 0;
        loop {
            match self.model.embed_prepared(texts.to_vec()).await {
                Ok(embeddings) => return Ok(embeddings),
                Err(e) if retries < 3 => {
                    retries += 1;
                    tracing::warn!(
                        retry = retries,
                        error = %e,
                        "embedding batch failed, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(
                        500 * 2u64.pow(retries - 1),
                    ))
                    .await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn dimension(&self) -> usize {
        self.model.dimension()
    }

    fn prepare_query(&self, query: &str) -> String {
        OpenAiEmbeddingClient::prepare_query(self, query)
    }

    fn prepare_document(&self, text: &str, heading: &str) -> String {
        OpenAiEmbeddingClient::prepare_document(self, text, heading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_query_with_instruction() {
        let config = EmbeddingConfig {
            base_url: "http://localhost:8002/v1".into(),
            model: "test".into(),
            dimension: 4096,
            query_instruction: "Retrieve relevant passages.".into(),
            document_instruction: String::new(),
            batch_size: 16,
            ..Default::default()
        };
        let client = OpenAiEmbeddingClient::new(&config);
        let q = client.prepare_query("What is freedom?");
        assert!(q.starts_with("Instruct:"));
        assert!(q.contains("What is freedom?"));
    }

    #[test]
    fn prepare_document_with_heading() {
        let config = EmbeddingConfig {
            base_url: "http://localhost:8002/v1".into(),
            model: "test".into(),
            dimension: 4096,
            query_instruction: String::new(),
            document_instruction: String::new(),
            batch_size: 16,
            ..Default::default()
        };
        let client = OpenAiEmbeddingClient::new(&config);
        let doc = client.prepare_document("Some text here.", "Chapter 1");
        assert_eq!(doc, "Chapter 1: Some text here.");
    }

    #[test]
    fn prepare_query_no_instruction() {
        let config = EmbeddingConfig {
            base_url: "http://localhost:8002/v1".into(),
            model: "test".into(),
            dimension: 4096,
            query_instruction: String::new(),
            document_instruction: String::new(),
            batch_size: 16,
            ..Default::default()
        };
        let client = OpenAiEmbeddingClient::new(&config);
        let q = client.prepare_query("plain query");
        assert_eq!(q, "plain query");
    }
}
