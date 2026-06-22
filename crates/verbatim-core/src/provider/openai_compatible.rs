//! OpenAI-compatible provider adapters for local model servers.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};

use crate::config::{ChatConfig, EmbeddingConfig, RerankConfig, VisionConfig};

use super::{
    ChatContentPart, ChatMessage, ChatModel, ChatRequest, ChatResponse, ChatStream,
    ChatStreamEvent, EmbeddingModel, EmbeddingPurpose, ImageDescribeRequest, ImageDescription,
    ImageUrl, ProviderError, ProviderResult, RerankDoc, RerankHit, Reranker as ProviderReranker,
    TokenUsage, VisionModel,
};

const CHAT_COMPLETIONS_PATH: &str = "chat/completions";
const EMBEDDINGS_PATH: &str = "embeddings";
const RERANK_PATH: &str = "rerank";

/// OpenAI-compatible chat completion adapter.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleChatModel {
    endpoint: OpenAiEndpoint,
    temperature: f32,
}

impl OpenAiCompatibleChatModel {
    pub fn from_config(config: &ChatConfig) -> Self {
        Self {
            endpoint: OpenAiEndpoint::new(
                &config.base_url,
                &config.model,
                &config.api_key,
                config.timeout_seconds,
            ),
            temperature: config.temperature,
        }
    }
}

/// OpenAI-compatible vision chat adapter.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleVisionModel {
    chat: OpenAiCompatibleChatModel,
}

impl OpenAiCompatibleVisionModel {
    pub fn from_config(config: &VisionConfig) -> Self {
        Self {
            chat: OpenAiCompatibleChatModel {
                endpoint: OpenAiEndpoint::new(
                    &config.base_url,
                    &config.model,
                    &config.api_key,
                    config.timeout_seconds,
                ),
                temperature: config.temperature,
            },
        }
    }
}

/// OpenAI-compatible embedding adapter.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleEmbeddingModel {
    endpoint: OpenAiEndpoint,
    dimension: usize,
    normalize: bool,
    batch_size: usize,
    query_instruction: String,
    document_instruction: String,
}

impl OpenAiCompatibleEmbeddingModel {
    pub fn from_config(config: &EmbeddingConfig) -> Self {
        Self {
            endpoint: OpenAiEndpoint::new(
                &config.base_url,
                &config.model,
                &config.api_key,
                config.timeout_seconds,
            ),
            dimension: config.dimension,
            normalize: config.normalize,
            batch_size: config.batch_size.max(1),
            query_instruction: config.query_instruction.clone(),
            document_instruction: config.document_instruction.clone(),
        }
    }

    pub fn dimension(&self) -> usize {
        self.dimension
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

    pub async fn embed_prepared(&self, texts: Vec<String>) -> ProviderResult<Vec<Vec<f32>>> {
        let mut all_embeddings = Vec::with_capacity(texts.len());

        for batch in texts.chunks(self.batch_size) {
            let body = OpenAiEmbeddingRequest {
                model: self.endpoint.model.clone(),
                input: batch.to_vec(),
                encoding_format: "float",
            };

            let response: OpenAiEmbeddingResponse = self
                .endpoint
                .post_json(EMBEDDINGS_PATH, &body, "embedding")
                .await?;

            if response.data.len() != batch.len() {
                return Err(ProviderError::malformed(
                    "embedding",
                    format!(
                        "expected {} embeddings, got {}",
                        batch.len(),
                        response.data.len()
                    ),
                ));
            }

            for item in response.data {
                let mut embedding = item.embedding;
                if self.dimension > 0 && embedding.len() != self.dimension {
                    return Err(ProviderError::malformed(
                        "embedding",
                        format!(
                            "dimension mismatch: expected {}, got {}",
                            self.dimension,
                            embedding.len()
                        ),
                    ));
                }
                if self.normalize {
                    normalize_vector(&mut embedding);
                }
                all_embeddings.push(embedding);
            }
        }

        Ok(all_embeddings)
    }

    fn prepare_for_purpose(&self, text: &str, purpose: EmbeddingPurpose) -> String {
        match purpose {
            EmbeddingPurpose::Query => self.prepare_query(text),
            EmbeddingPurpose::Document => self.prepare_document(text, ""),
        }
    }
}

/// OpenAI-compatible rerank adapter for `/v1/rerank`-style endpoints.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleReranker {
    endpoint: OpenAiEndpoint,
    default_top_n: usize,
}

impl OpenAiCompatibleReranker {
    pub fn from_config(config: &RerankConfig) -> Self {
        Self {
            endpoint: OpenAiEndpoint::new(
                &config.base_url,
                &config.model,
                &config.api_key,
                config.timeout_seconds,
            ),
            default_top_n: config.top_n,
        }
    }
}

#[async_trait]
impl ChatModel for OpenAiCompatibleChatModel {
    async fn chat(&self, req: ChatRequest) -> ProviderResult<ChatResponse> {
        let body = self.chat_body(req, false);
        let response: OpenAiChatResponse = self
            .endpoint
            .post_json(CHAT_COMPLETIONS_PATH, &body, "chat")
            .await?;
        chat_response_from_openai(response)
    }

    async fn stream_chat(&self, req: ChatRequest) -> ProviderResult<ChatStream> {
        let body = self.chat_body(req, true);
        let response = self
            .endpoint
            .post(CHAT_COMPLETIONS_PATH, &body, "streaming chat")
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error("streaming chat", status, response).await);
        }

        let chunks = response
            .bytes_stream()
            .map(|result| match result {
                Ok(bytes) => Ok(bytes.to_vec()),
                Err(source) => Err(ProviderError::Transport {
                    operation: "streaming chat",
                    source,
                }),
            })
            .boxed();

        Ok(sse_chat_stream(chunks))
    }
}

impl OpenAiCompatibleChatModel {
    fn chat_body(&self, req: ChatRequest, stream: bool) -> OpenAiChatRequest {
        OpenAiChatRequest {
            model: self.endpoint.model.clone(),
            messages: req.messages,
            temperature: req.temperature.unwrap_or(self.temperature),
            max_tokens: req.max_tokens,
            stream,
        }
    }
}

#[async_trait]
impl VisionModel for OpenAiCompatibleVisionModel {
    async fn describe_image(&self, req: ImageDescribeRequest) -> ProviderResult<ImageDescription> {
        let prompt = if req.prompt.trim().is_empty() {
            "Describe this image.".to_string()
        } else {
            req.prompt
        };
        let message = ChatMessage::user_parts(vec![
            ChatContentPart::Text { text: prompt },
            ChatContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: req.image.to_openai_url(),
                    detail: req.detail,
                },
            },
        ]);

        let mut chat_req = ChatRequest::new(vec![message]);
        chat_req.max_tokens = req.max_tokens;
        let response = self.chat.chat(chat_req).await?;
        Ok(ImageDescription {
            text: response.content,
        })
    }
}

#[async_trait]
impl EmbeddingModel for OpenAiCompatibleEmbeddingModel {
    async fn embed(
        &self,
        texts: Vec<String>,
        purpose: EmbeddingPurpose,
    ) -> ProviderResult<Vec<Vec<f32>>> {
        let prepared = texts
            .iter()
            .map(|text| self.prepare_for_purpose(text, purpose))
            .collect();
        self.embed_prepared(prepared).await
    }
}

#[async_trait]
impl ProviderReranker for OpenAiCompatibleReranker {
    async fn rerank(
        &self,
        query: &str,
        docs: Vec<RerankDoc>,
        top_n: usize,
    ) -> ProviderResult<Vec<RerankHit>> {
        let top_n = if top_n == 0 {
            self.default_top_n
        } else {
            top_n
        };
        let body = OpenAiRerankRequest {
            model: self.endpoint.model.clone(),
            query: query.to_string(),
            documents: docs.into_iter().map(|doc| doc.text).collect(),
            top_n,
        };

        let response: OpenAiRerankResponse = self
            .endpoint
            .post_json(RERANK_PATH, &body, "rerank")
            .await?;
        let results = response.into_hits();
        if results.is_empty() && !body.documents.is_empty() {
            return Err(ProviderError::malformed(
                "rerank",
                "response did not include rerank results",
            ));
        }
        Ok(results)
    }
}

#[async_trait]
impl crate::traits::Reranker for OpenAiCompatibleReranker {
    async fn rerank(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> anyhow::Result<Vec<(usize, f32)>> {
        let docs = docs.iter().cloned().map(RerankDoc::new).collect();
        let hits = ProviderReranker::rerank(self, query, docs, top_n).await?;
        Ok(hits.into_iter().map(|hit| (hit.index, hit.score)).collect())
    }
}

#[derive(Clone, Debug)]
struct OpenAiEndpoint {
    client: Client,
    base_url: String,
    model: String,
    api_key: String,
    timeout: Duration,
}

impl OpenAiEndpoint {
    fn new(base_url: &str, model: &str, api_key: &str, timeout_seconds: u64) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
            timeout: Duration::from_secs(timeout_seconds.max(1)),
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        operation: &'static str,
    ) -> ProviderResult<T> {
        let response = self.post(path, body, operation).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(operation, status, response).await);
        }
        response
            .json::<T>()
            .await
            .map_err(|source| ProviderError::ResponseDecode { operation, source })
    }

    async fn post<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        operation: &'static str,
    ) -> ProviderResult<reqwest::Response> {
        if self.base_url.is_empty() {
            return Err(ProviderError::configuration(operation, "base_url is empty"));
        }
        if self.model.is_empty() {
            return Err(ProviderError::configuration(operation, "model is empty"));
        }

        let url = format!("{}/{}", self.base_url, path);
        self.auth(self.client.post(url).timeout(self.timeout).json(body))
            .send()
            .await
            .map_err(|source| ProviderError::Transport { operation, source })
    }

    fn auth(&self, req: RequestBuilder) -> RequestBuilder {
        if self.api_key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.api_key)
        }
    }
}

#[derive(Debug, Serialize)]
struct OpenAiChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChatChoice>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatChoice {
    message: Option<OpenAiChatMessage>,
    delta: Option<OpenAiChatDelta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatDelta {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatChunk {
    choices: Vec<OpenAiChatChoice>,
}

#[derive(Debug, Serialize)]
struct OpenAiEmbeddingRequest {
    model: String,
    input: Vec<String>,
    encoding_format: &'static str,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingData {
    embedding: Vec<f32>,
}

#[derive(Debug, Serialize)]
struct OpenAiRerankRequest {
    model: String,
    query: String,
    documents: Vec<String>,
    top_n: usize,
}

#[derive(Debug, Deserialize)]
struct OpenAiRerankResponse {
    #[serde(default)]
    results: Vec<OpenAiRerankResult>,
    #[serde(default)]
    data: Vec<OpenAiRerankResult>,
}

impl OpenAiRerankResponse {
    fn into_hits(self) -> Vec<RerankHit> {
        let results = if self.results.is_empty() {
            self.data
        } else {
            self.results
        };
        results
            .into_iter()
            .map(|result| RerankHit {
                index: result.index,
                score: result.score,
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiRerankResult {
    index: usize,
    #[serde(alias = "relevance_score")]
    score: f32,
}

fn chat_response_from_openai(response: OpenAiChatResponse) -> ProviderResult<ChatResponse> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::malformed("chat", "response contained no choices"))?;
    let content = choice
        .message
        .and_then(|message| message.content)
        .unwrap_or_default();
    Ok(ChatResponse {
        content,
        finish_reason: choice.finish_reason,
        usage: response.usage,
    })
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

async fn http_status_error(
    operation: &'static str,
    status: StatusCode,
    response: reqwest::Response,
) -> ProviderError {
    let message = match response.text().await {
        Ok(body) => openai_error_message(&body)
            .unwrap_or_else(|| "provider returned a non-success response".to_string()),
        Err(_) => "provider returned a non-success response".to_string(),
    };
    ProviderError::HttpStatus {
        operation,
        status,
        message,
    }
}

fn openai_error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error = value.get("error")?;
    let raw_message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provider error");
    Some(truncate_message(raw_message, 240))
}

fn truncate_message(message: &str, max_chars: usize) -> String {
    let mut chars = message.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn sse_chat_stream(chunks: BoxStream<'static, ProviderResult<Vec<u8>>>) -> ChatStream {
    struct State {
        chunks: BoxStream<'static, ProviderResult<Vec<u8>>>,
        pending_utf8: Vec<u8>,
        buffer: String,
        pending: VecDeque<ProviderResult<ChatStreamEvent>>,
        done: bool,
    }

    let state = State {
        chunks,
        pending_utf8: Vec::new(),
        buffer: String::new(),
        pending: VecDeque::new(),
        done: false,
    };

    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Some((event, state));
            }
            if state.done {
                return None;
            }

            match state.chunks.next().await {
                Some(Ok(chunk)) => {
                    if let Err(error) =
                        push_utf8_chunk(&chunk, &mut state.pending_utf8, &mut state.buffer)
                    {
                        return Some((Err(error), state));
                    }
                    drain_sse_buffer(&mut state.buffer, &mut state.pending);
                }
                Some(Err(err)) => return Some((Err(err), state)),
                None => {
                    if !state.pending_utf8.is_empty() {
                        state.pending_utf8.clear();
                        state.done = true;
                        return Some((
                            Err(ProviderError::malformed(
                                "streaming chat",
                                "provider stream ended with partial UTF-8",
                            )),
                            state,
                        ));
                    }
                    if !state.buffer.trim().is_empty() {
                        parse_sse_frame(&state.buffer, &mut state.pending);
                        state.buffer.clear();
                    }
                    state.done = true;
                }
            }
        }
    })
    .boxed()
}

fn push_utf8_chunk(
    chunk: &[u8],
    pending_utf8: &mut Vec<u8>,
    buffer: &mut String,
) -> ProviderResult<()> {
    pending_utf8.extend_from_slice(chunk);

    loop {
        match std::str::from_utf8(pending_utf8) {
            Ok(text) => {
                buffer.push_str(text);
                pending_utf8.clear();
                return Ok(());
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if valid_up_to > 0 {
                    let valid =
                        std::str::from_utf8(&pending_utf8[..valid_up_to]).map_err(|_| {
                            ProviderError::malformed(
                                "streaming chat",
                                "provider stream contained invalid UTF-8",
                            )
                        })?;
                    buffer.push_str(valid);
                    pending_utf8.drain(..valid_up_to);
                    continue;
                }

                if error.error_len().is_none() {
                    return Ok(());
                }

                return Err(ProviderError::malformed(
                    "streaming chat",
                    "provider stream contained invalid UTF-8",
                ));
            }
        }
    }
}

fn drain_sse_buffer(buffer: &mut String, pending: &mut VecDeque<ProviderResult<ChatStreamEvent>>) {
    while let Some((idx, separator_len)) = find_sse_frame_separator(buffer) {
        let frame = buffer[..idx].to_string();
        buffer.drain(..idx + separator_len);
        parse_sse_frame(&frame, pending);
    }
}

fn find_sse_frame_separator(buffer: &str) -> Option<(usize, usize)> {
    let lf = buffer.find("\n\n").map(|idx| (idx, 2));
    let crlf = buffer.find("\r\n\r\n").map(|idx| (idx, 4));

    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn parse_sse_frame(frame: &str, pending: &mut VecDeque<ProviderResult<ChatStreamEvent>>) {
    let data = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");

    if data.is_empty() {
        return;
    }
    if data == "[DONE]" {
        return;
    }

    match serde_json::from_str::<OpenAiChatChunk>(&data) {
        Ok(chunk) => {
            for choice in chunk.choices {
                let delta = choice
                    .delta
                    .and_then(|delta| delta.content)
                    .unwrap_or_default();
                if !delta.is_empty() || choice.finish_reason.is_some() {
                    pending.push_back(Ok(ChatStreamEvent {
                        delta,
                        finish_reason: choice.finish_reason,
                    }));
                }
            }
        }
        Err(source) => pending.push_back(Err(ProviderError::StreamDecode {
            operation: "streaming chat",
            source,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessageContent, ImageInput};
    use futures::stream;

    #[test]
    fn serializes_text_chat_request_shape() {
        let model = OpenAiCompatibleChatModel {
            endpoint: OpenAiEndpoint::new("http://127.0.0.1:8000/v1", "model", "", 120),
            temperature: 0.2,
        };
        let body = model.chat_body(
            ChatRequest::new(vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user("user prompt"),
            ])
            .with_max_tokens(42),
            false,
        );

        let value = serde_json::to_value(body).expect("serialize chat request");

        assert_eq!(value["model"], "model");
        assert_eq!(value["stream"], false);
        assert_eq!(value["max_tokens"], 42);
        assert_eq!(value["messages"][0]["role"], "system");
        assert_eq!(value["messages"][1]["content"], "user prompt");
    }

    #[test]
    fn serializes_vision_message_with_data_uri() {
        let message = ChatMessage::user_parts(vec![
            ChatContentPart::Text {
                text: "Describe".into(),
            },
            ChatContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: ImageInput::data_uri("data:image/png;base64,abc").to_openai_url(),
                    detail: Some("high".into()),
                },
            },
        ]);

        let value = serde_json::to_value(message).expect("serialize vision message");

        assert_eq!(value["role"], "user");
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][1]["type"], "image_url");
        assert_eq!(
            value["content"][1]["image_url"]["url"],
            "data:image/png;base64,abc"
        );
        assert_eq!(value["content"][1]["image_url"]["detail"], "high");
    }

    #[test]
    fn file_image_input_uses_file_url() {
        assert_eq!(
            ImageInput::file("/tmp/crop.png").to_openai_url(),
            "file:///tmp/crop.png"
        );
    }

    #[test]
    fn embedding_purpose_applies_query_instruction() {
        let model = OpenAiCompatibleEmbeddingModel {
            endpoint: OpenAiEndpoint::new("http://127.0.0.1:8002/v1", "model", "", 120),
            dimension: 3,
            normalize: false,
            batch_size: 16,
            query_instruction: "Find evidence.".into(),
            document_instruction: String::new(),
        };

        let prepared = model.prepare_for_purpose("What happened?", EmbeddingPurpose::Query);

        assert_eq!(prepared, "Instruct: Find evidence.\nQuery: What happened?");
    }

    #[test]
    fn normalizes_embedding_vector() {
        let mut vector = vec![3.0, 4.0];
        normalize_vector(&mut vector);

        assert!((vector[0] - 0.6).abs() < f32::EPSILON);
        assert!((vector[1] - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn parses_rerank_results_and_data_shapes() {
        let response: OpenAiRerankResponse = serde_json::from_str(
            r#"{"results":[{"index":2,"relevance_score":0.9},{"index":0,"score":0.7}]}"#,
        )
        .expect("parse rerank results");

        assert_eq!(
            response.into_hits(),
            vec![
                RerankHit {
                    index: 2,
                    score: 0.9
                },
                RerankHit {
                    index: 0,
                    score: 0.7
                }
            ]
        );

        let response: OpenAiRerankResponse =
            serde_json::from_str(r#"{"data":[{"index":1,"score":0.8}]}"#)
                .expect("parse rerank data");
        assert_eq!(
            response.into_hits(),
            vec![RerankHit {
                index: 1,
                score: 0.8
            }]
        );
    }

    #[tokio::test]
    async fn parses_streaming_chat_chunks() {
        let chunks = stream::iter(vec![
            Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n".to_vec()),
            Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n".to_vec()),
            Ok(b"data: [DONE]\n\n".to_vec()),
        ])
        .boxed();

        let events = sse_chat_stream(chunks)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<ProviderResult<Vec<_>>>()
            .expect("parse stream");

        assert_eq!(
            events,
            vec![
                ChatStreamEvent {
                    delta: "hel".into(),
                    finish_reason: None,
                },
                ChatStreamEvent {
                    delta: "lo".into(),
                    finish_reason: Some("stop".into()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn parses_streaming_chat_chunk_split_inside_utf8_scalar() {
        let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"streamed 你好 π\"}}]}\n\n";
        let split = frame
            .find("你")
            .expect("test frame contains multibyte text")
            + 1;
        let chunks = stream::iter(vec![
            Ok(frame.as_bytes()[..split].to_vec()),
            Ok(frame.as_bytes()[split..].to_vec()),
            Ok(b"data: [DONE]\n\n".to_vec()),
        ])
        .boxed();

        let events = sse_chat_stream(chunks)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<ProviderResult<Vec<_>>>()
            .expect("parse stream");

        assert_eq!(
            events,
            vec![ChatStreamEvent {
                delta: "streamed 你好 π".into(),
                finish_reason: None,
            }]
        );
        assert!(!events[0].delta.contains('\u{fffd}'));
    }

    #[test]
    fn parses_openai_error_message_without_plain_body() {
        let json = r#"{"error":{"message":"model not found","type":"invalid_request"}}"#;
        assert_eq!(openai_error_message(json), Some("model not found".into()));
        assert_eq!(openai_error_message("raw stack trace"), None);
    }

    #[test]
    fn text_message_content_serializes_as_string() {
        let value = serde_json::to_value(ChatMessageContent::Text("hello".into()))
            .expect("serialize content");
        assert_eq!(value, serde_json::Value::String("hello".into()));
    }
}
