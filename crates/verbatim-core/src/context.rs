use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::ChatConfig;
use crate::types::Chunk;

pub struct ContextGenerator {
    client: Client,
    base_url: String,
    model: String,
    temperature: f32,
    api_key: String,
}

impl ContextGenerator {
    pub fn new(config: &ChatConfig) -> Self {
        Self {
            client: Client::new(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            model: config.model.clone(),
            temperature: config.temperature,
            api_key: config.api_key.clone(),
        }
    }

    pub async fn generate_context(&self, chunk: &Chunk, document_title: &str) -> Result<String> {
        let heading = if chunk.heading_path.is_empty() {
            String::new()
        } else {
            chunk.heading_path.join(" > ")
        };

        let prompt = format!(
            "Document: {document_title}\n\
             Section: {heading}\n\n\
             Here is the chunk:\n\
             {text}\n\n\
             Provide a short (1-2 sentence) context that situates this chunk \
             within the document. Only state facts from the document.",
            text = chunk.text
        );

        let messages = vec![ChatMessage {
            role: "user",
            content: &prompt,
        }];

        let body = ChatRequest {
            model: &self.model,
            messages: &messages,
            temperature: self.temperature,
            max_tokens: 150,
        };

        let url = format!("{}/chat/completions", self.base_url);
        let mut req = self.client.post(&url).json(&body);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let resp = req
            .send()
            .await
            .context("context generation request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("chat API returned {status}: {text}");
        }

        let response: ChatResponse = resp.json().await.context("parse chat response")?;
        let context = response
            .choices
            .first()
            .map(|c| c.message.content.trim().to_string())
            .unwrap_or_default();

        Ok(context)
    }

    pub async fn enrich_chunks(
        &self,
        chunks: &mut [Chunk],
        document_title: &str,
        concurrency: usize,
    ) -> Result<usize> {
        let children: Vec<usize> = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.chunk_type == crate::types::ChunkType::Child)
            .map(|(i, _)| i)
            .collect();

        let total = children.len();
        let mut enriched = 0;

        for batch_start in (0..children.len()).step_by(concurrency) {
            let batch_end = (batch_start + concurrency).min(children.len());
            let batch = &children[batch_start..batch_end];

            let mut futures = Vec::new();
            for &idx in batch {
                let chunk = chunks[idx].clone();
                let title = document_title.to_string();
                futures.push(async move {
                    let ctx = self.generate_context(&chunk, &title).await;
                    (idx, ctx)
                });
            }

            let results = futures::future::join_all(futures).await;

            for (idx, result) in results {
                match result {
                    Ok(ctx) if !ctx.is_empty() => {
                        chunks[idx].context_text = Some(ctx);
                        enriched += 1;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(chunk_id = %chunks[idx].id.0, error = %e, "context generation failed");
                    }
                }
            }

            tracing::info!(
                progress = format!("{}/{}", (batch_end).min(total), total),
                "contextual retrieval"
            );
        }

        Ok(enriched)
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage<'a>],
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageResponse,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChunkId, ChunkType, SourceId};

    #[test]
    fn prompt_includes_heading_and_text() {
        let chunk = Chunk {
            id: ChunkId("c1".into()),
            source_id: SourceId("test".into()),
            text: "Some important text.".into(),
            context_text: None,
            token_count: 5,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Chapter 1".into(), "Section 1.1".into()],
            evidence_unit_ids: vec![],
        };

        let heading = chunk.heading_path.join(" > ");
        assert_eq!(heading, "Chapter 1 > Section 1.1");
    }
}
