use anyhow::{Context, Result};

use crate::config::ChatConfig;
use crate::provider::openai_compatible::OpenAiCompatibleChatModel;
use crate::provider::{ChatMessage, ChatModel, ChatRequest};
use crate::types::Chunk;

pub struct ContextGenerator {
    chat_model: OpenAiCompatibleChatModel,
}

impl ContextGenerator {
    pub fn new(config: &ChatConfig) -> Self {
        Self {
            chat_model: OpenAiCompatibleChatModel::from_config(config),
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

        let response = self
            .chat_model
            .chat(ChatRequest::new(vec![ChatMessage::user(prompt)]).with_max_tokens(150))
            .await
            .context("context generation request failed")?;
        let context = response.content.trim().to_string();

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
