use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::EmbeddingConfig;
use crate::traits::EmbeddingClient;

pub struct OpenAiEmbeddingClient {
    client: Client,
    base_url: String,
    model: String,
    dimension: usize,
    batch_size: usize,
    query_instruction: String,
    document_instruction: String,
}

impl OpenAiEmbeddingClient {
    pub fn new(config: &EmbeddingConfig) -> Self {
        Self {
            client: Client::new(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            model: config.model.clone(),
            dimension: config.dimension,
            batch_size: config.batch_size,
            query_instruction: config.query_instruction.clone(),
            document_instruction: config.document_instruction.clone(),
        }
    }

    pub fn prepare_query(&self, query: &str) -> String {
        if self.query_instruction.is_empty() {
            return query.to_string();
        }
        format!("Instruct: {}\nQuery: {}", self.query_instruction, query)
    }

    pub fn prepare_document(&self, text: &str, heading: &str) -> String {
        let mut result = String::new();
        if !self.document_instruction.is_empty() {
            result.push_str(&self.document_instruction);
            result.push('\n');
        }
        if !heading.is_empty() {
            result.push_str(heading);
            result.push_str(": ");
        }
        result.push_str(text);
        result
    }

    async fn embed_batch_raw(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.base_url);
        let body = EmbeddingRequest {
            model: &self.model,
            input: texts,
            encoding_format: "float",
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("embedding request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("embedding API returned {status}: {text}");
        }

        let response: EmbeddingResponse = resp.json().await.context("parse embedding response")?;
        let mut embeddings: Vec<Vec<f32>> =
            response.data.into_iter().map(|d| d.embedding).collect();

        for emb in &embeddings {
            if emb.len() != self.dimension {
                bail!(
                    "dimension mismatch: expected {}, got {}",
                    self.dimension,
                    emb.len()
                );
            }
        }

        embeddings.truncate(texts.len());
        Ok(embeddings)
    }
}

#[async_trait]
impl EmbeddingClient for OpenAiEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut all_embeddings = Vec::with_capacity(texts.len());

        for batch in texts.chunks(self.batch_size) {
            let batch_vec: Vec<String> = batch.to_vec();
            let mut retries = 0;
            loop {
                match self.embed_batch_raw(&batch_vec).await {
                    Ok(embs) => {
                        all_embeddings.extend(embs);
                        break;
                    }
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
                    Err(e) => return Err(e),
                }
            }
        }

        Ok(all_embeddings)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn prepare_query(&self, query: &str) -> String {
        OpenAiEmbeddingClient::prepare_query(self, query)
    }

    fn prepare_document(&self, text: &str, heading: &str) -> String {
        OpenAiEmbeddingClient::prepare_document(self, text, heading)
    }
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    encoding_format: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_query_with_instruction() {
        let config = EmbeddingConfig {
            base_url: "http://localhost:8001/v1".into(),
            model: "test".into(),
            dimension: 4096,
            query_instruction: "Retrieve relevant passages.".into(),
            document_instruction: String::new(),
            batch_size: 32,
        };
        let client = OpenAiEmbeddingClient::new(&config);
        let q = client.prepare_query("What is freedom?");
        assert!(q.starts_with("Instruct:"));
        assert!(q.contains("What is freedom?"));
    }

    #[test]
    fn prepare_document_with_heading() {
        let config = EmbeddingConfig {
            base_url: "http://localhost:8001/v1".into(),
            model: "test".into(),
            dimension: 4096,
            query_instruction: String::new(),
            document_instruction: String::new(),
            batch_size: 32,
        };
        let client = OpenAiEmbeddingClient::new(&config);
        let doc = client.prepare_document("Some text here.", "Chapter 1");
        assert_eq!(doc, "Chapter 1: Some text here.");
    }

    #[test]
    fn prepare_query_no_instruction() {
        let config = EmbeddingConfig {
            base_url: "http://localhost:8001/v1".into(),
            model: "test".into(),
            dimension: 4096,
            query_instruction: String::new(),
            document_instruction: String::new(),
            batch_size: 32,
        };
        let client = OpenAiEmbeddingClient::new(&config);
        let q = client.prepare_query("plain query");
        assert_eq!(q, "plain query");
    }
}
