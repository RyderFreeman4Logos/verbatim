//! OpenAI-compatible provider adapters for local model servers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};

use crate::config::{
    ChatConfig, EmbeddingConfig, ModelEndpointRuntimeConfig, ModelRetryConfig, RerankConfig,
    VisionConfig,
};
use crate::resource::{
    global_resource_registry, ObservableResource, ResourceLimitConfig, ResourcePermit,
    ResourceQueueError, ResourceQueueSnapshot,
};
use crate::traits::{
    EmbeddingEndpointCapabilities, RerankCapabilityDiagnostics, RerankCapabilityState,
    RerankDiagnostics, RerankError, RerankRequestDiagnostics, RerankResponse,
};
use crate::types::hex_sha256;
use crate::upstream::{
    capture_full_response, capture_response_prefix, sanitize_text, UpstreamRequestContext,
    DEFAULT_BODY_PREFIX_MAX_BYTES,
};

use super::endpoint_capability::{
    capability_failure_reason, endpoint_capability_cache, is_context_or_payload_limit_error,
    is_discovery_unsupported, model_discovery_paths, normalized_endpoint_key,
    parse_endpoint_capability, EndpointCapability, EndpointCapabilityCacheKey,
    EndpointCapabilityLookup, EndpointCapabilityRole, EndpointCapabilityState,
};
use super::{
    ChatContentPart, ChatMessage, ChatModel, ChatRequest, ChatResponse, ChatStream,
    ChatStreamEvent, EmbeddingModel, EmbeddingPurpose, ImageDescribeRequest, ImageDescription,
    ImageUrl, ProviderError, ProviderResult, RerankDoc, RerankHit, Reranker as ProviderReranker,
    TokenUsage, VisionModel,
};

const CHAT_COMPLETIONS_PATH: &str = "chat/completions";
const EMBEDDINGS_PATH: &str = "embeddings";
const RERANK_PATH: &str = "rerank";
const RERANK_V1_PATH: &str = "v1/rerank";
const RERANK_V2_PATH: &str = "v2/rerank";
const RERANK_DOCUMENT_CHARS_PER_CONTEXT_TOKEN: usize = 3;
const RERANK_DOCUMENT_CONTEXT_BUDGET_NUMERATOR: usize = 3;
const RERANK_DOCUMENT_CONTEXT_BUDGET_DENOMINATOR: usize = 4;
const RERANK_RETRY_CONTEXT_BUDGET_NUMERATOR: usize = 1;
const RERANK_RETRY_CONTEXT_BUDGET_DENOMINATOR: usize = 2;
const RERANK_MIN_CHARS_PER_CANDIDATE: usize = 512;
const RERANK_MAX_DOCUMENT_CHARS: usize = 8_000;
const RERANK_RETRY_MAX_DOCUMENT_CHARS: usize = 4_000;
const LLM_RERANK_MAX_CANDIDATES: usize = 20;
const LLM_RERANK_MAX_DOCUMENT_CHARS: usize = 2_000;
const LLM_RERANK_MAX_OUTPUT_TOKENS: u32 = 1_024;

/// OpenAI-compatible chat completion adapter.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleChatModel {
    endpoint: OpenAiEndpoint,
    temperature: f32,
}

impl OpenAiCompatibleChatModel {
    pub fn from_config(config: &ChatConfig) -> Self {
        Self {
            endpoint: OpenAiEndpoint::new_with_options(
                &config.base_url,
                &config.model,
                &config.api_key,
                config.timeout_seconds,
                &config.endpoint_runtime,
                "chat",
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
                endpoint: OpenAiEndpoint::new_with_options(
                    &config.base_url,
                    &config.model,
                    &config.api_key,
                    config.timeout_seconds,
                    &config.endpoint_runtime,
                    "vision",
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
    provider_kind: String,
    dimension: usize,
    normalize: bool,
    batch_size: usize,
    capability_cache_ttl: Duration,
    query_instruction: String,
    document_instruction: String,
}

impl OpenAiCompatibleEmbeddingModel {
    pub fn from_config(config: &EmbeddingConfig) -> Self {
        Self {
            endpoint: OpenAiEndpoint::new_with_options(
                &config.base_url,
                &config.model,
                &config.api_key,
                config.timeout_seconds,
                &config.endpoint_runtime,
                "embedding",
            ),
            provider_kind: config.provider.clone(),
            dimension: config.dimension,
            normalize: config.normalize,
            batch_size: config.batch_size.max(1),
            capability_cache_ttl: Duration::from_secs(config.capability_cache_ttl_seconds.max(1)),
            query_instruction: config.query_instruction.clone(),
            document_instruction: config.document_instruction.clone(),
        }
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub async fn endpoint_capabilities(&self) -> ProviderResult<EmbeddingEndpointCapabilities> {
        let lookup = self.load_endpoint_capability(false).await;
        let capability = lookup.value.as_ref();
        Ok(EmbeddingEndpointCapabilities {
            endpoint_identity: Some(normalized_endpoint_key(&self.endpoint.base_url)),
            requested_model: Some(self.endpoint.model.clone()),
            served_model: capability.and_then(|capability| capability.served_model.clone()),
            max_context_tokens: capability.and_then(|capability| capability.max_context_tokens),
            dtype: capability.and_then(|capability| capability.dtype.clone()),
            quantization: capability.and_then(|capability| capability.quantization.clone()),
            weight_identity: capability.and_then(|capability| capability.weight_identity.clone()),
        })
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
        let capability = self.cached_endpoint_capability();
        let mut batch_size = self.embedding_batch_size(capability.value.as_ref());
        let mut offset = 0;
        let mut refreshed_after_limit = false;

        while offset < texts.len() {
            let end = offset.saturating_add(batch_size).min(texts.len());
            let batch = &texts[offset..end];

            match self.post_embedding_batch(batch).await {
                Ok(mut embeddings) => {
                    all_embeddings.append(&mut embeddings);
                    offset = end;
                }
                Err(error)
                    if is_context_or_payload_limit_error(&error) && !refreshed_after_limit =>
                {
                    refreshed_after_limit = true;
                    let refresh = self.load_endpoint_capability(true).await;
                    let refreshed_batch_size = self.embedding_batch_size(refresh.value.as_ref());
                    if refreshed_batch_size < batch.len() {
                        batch_size = refreshed_batch_size;
                        continue;
                    }
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }

        Ok(all_embeddings)
    }

    async fn post_embedding_batch(&self, batch: &[String]) -> ProviderResult<Vec<Vec<f32>>> {
        let body = OpenAiEmbeddingRequest {
            model: self.endpoint.model.clone(),
            input: batch.to_vec(),
            encoding_format: "float",
        };

        let response: OpenAiEmbeddingResponse = self
            .endpoint
            .post_json(EMBEDDINGS_PATH, &body, "embedding", "embedding")
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

        response
            .data
            .into_iter()
            .map(|item| {
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
                Ok(embedding)
            })
            .collect()
    }

    async fn load_endpoint_capability(&self, force_refresh: bool) -> EndpointCapabilityLookup {
        let key = self.capability_cache_key();
        if !force_refresh {
            if let Some(cached) =
                endpoint_capability_cache().get_fresh(&key, self.capability_cache_ttl)
            {
                return cached;
            }
        }

        let refreshed = self.refresh_endpoint_capability().await;
        endpoint_capability_cache().insert(key, refreshed.clone());
        refreshed
    }

    fn cached_endpoint_capability(&self) -> EndpointCapabilityLookup {
        let key = self.capability_cache_key();
        endpoint_capability_cache()
            .get_fresh(&key, self.capability_cache_ttl)
            .unwrap_or_else(|| EndpointCapabilityLookup::unavailable("capability_not_loaded"))
    }

    fn capability_cache_key(&self) -> EndpointCapabilityCacheKey {
        EndpointCapabilityCacheKey::new(
            &self.endpoint.base_url,
            EndpointCapabilityRole::Embedding,
            &self.provider_kind,
            &self.endpoint.model,
        )
    }

    async fn refresh_endpoint_capability(&self) -> EndpointCapabilityLookup {
        let paths = model_discovery_paths(&self.endpoint.base_url);
        let response = self
            .endpoint
            .get_json_paths::<serde_json::Value>(
                &paths,
                "embedding capability discovery",
                "embedding",
            )
            .await;
        match response {
            Ok(value) => match parse_endpoint_capability(&value, &self.endpoint.model) {
                Some(capability) => EndpointCapabilityLookup::refreshed(capability),
                None => EndpointCapabilityLookup::unavailable("capability_absent"),
            },
            Err(error) if is_discovery_unsupported(&error) => {
                EndpointCapabilityLookup::unavailable("discovery_unsupported")
            }
            Err(error) => {
                EndpointCapabilityLookup::refresh_failed(capability_failure_reason(&error))
            }
        }
    }

    fn embedding_batch_size(&self, capability: Option<&EndpointCapability>) -> usize {
        capability
            .and_then(|capability| capability.request_limits.embedding_batch_size())
            .map_or(self.batch_size, |limit| self.batch_size.min(limit.max(1)))
            .max(1)
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
    provider: RerankProviderKind,
    default_top_n: usize,
    capability_cache_ttl: Duration,
}

/// OpenAI-compatible chat adapter used only for explicitly configured LLM rerank.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleLlmReranker {
    chat: OpenAiCompatibleChatModel,
}

impl OpenAiCompatibleLlmReranker {
    pub fn from_config(config: &RerankConfig) -> Self {
        Self {
            chat: OpenAiCompatibleChatModel {
                endpoint: OpenAiEndpoint::new_with_options(
                    &config.base_url,
                    &config.model,
                    &config.api_key,
                    config.timeout_seconds,
                    &config.endpoint_runtime,
                    "rerank",
                ),
                temperature: 0.0,
            },
        }
    }
}

impl OpenAiCompatibleReranker {
    pub fn from_config(config: &RerankConfig) -> Self {
        Self {
            endpoint: OpenAiEndpoint::new_with_options(
                &config.base_url,
                &config.model,
                &config.api_key,
                config.timeout_seconds,
                &config.endpoint_runtime,
                "rerank",
            ),
            provider: RerankProviderKind::from_provider(&config.provider),
            default_top_n: config.top_n,
            capability_cache_ttl: Duration::from_secs(config.capability_cache_ttl_seconds.max(1)),
        }
    }

    async fn rerank_with_outcome(
        &self,
        query: &str,
        docs: Vec<RerankDoc>,
        top_n: usize,
    ) -> Result<ProviderRerankOutcome, ProviderRerankFailure> {
        let mut diagnostics = RerankDiagnostics::default();
        let capability = self.load_rerank_capability(false).await;
        diagnostics.capability = Some(rerank_capability_diagnostics(&capability));

        let first_shape = RerankRequestShape::new(
            &docs,
            top_n,
            self.default_top_n,
            capability.value.as_ref(),
            false,
        );
        diagnostics.request = Some(first_shape.diagnostics());

        match self.post_rerank(query, &docs, &first_shape).await {
            Ok(hits) => Ok(ProviderRerankOutcome { hits, diagnostics }),
            Err(error) if is_context_or_payload_limit_error(&error) => {
                let refresh = self.load_rerank_capability(true).await;
                diagnostics.capability = Some(rerank_capability_diagnostics(&refresh));

                let Some(refreshed_capability) = refresh.value else {
                    return Err(ProviderRerankFailure { error, diagnostics });
                };

                let retry_shape = RerankRequestShape::new(
                    &docs,
                    top_n,
                    self.default_top_n,
                    Some(&refreshed_capability),
                    true,
                );
                diagnostics.request = Some(retry_shape.diagnostics());
                diagnostics.retried_after_context_limit = true;

                match self.post_rerank(query, &docs, &retry_shape).await {
                    Ok(hits) => Ok(ProviderRerankOutcome { hits, diagnostics }),
                    Err(retry_error) => Err(ProviderRerankFailure {
                        error: retry_error,
                        diagnostics,
                    }),
                }
            }
            Err(error) => Err(ProviderRerankFailure { error, diagnostics }),
        }
    }

    async fn post_rerank(
        &self,
        query: &str,
        docs: &[RerankDoc],
        shape: &RerankRequestShape,
    ) -> ProviderResult<Vec<RerankHit>> {
        let body = OpenAiRerankRequest {
            model: self.endpoint.model.clone(),
            query: query.to_string(),
            documents: shape
                .document_texts(docs)
                .into_iter()
                .map(|text| text.to_string())
                .collect(),
            top_n: shape.top_n,
        };

        let paths = self.provider.rerank_paths(&self.endpoint.base_url);
        let response: OpenAiRerankResponse = self
            .endpoint
            .post_json_paths(&paths, &body, "rerank", "rerank")
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

    async fn load_rerank_capability(&self, force_refresh: bool) -> EndpointCapabilityLookup {
        let key = EndpointCapabilityCacheKey::new(
            &self.endpoint.base_url,
            EndpointCapabilityRole::Rerank,
            self.provider.capability_provider_kind(),
            &self.endpoint.model,
        );
        if !force_refresh {
            if let Some(cached) =
                endpoint_capability_cache().get_fresh(&key, self.capability_cache_ttl)
            {
                return cached;
            }
        }

        let refreshed = self.refresh_rerank_capability().await;
        endpoint_capability_cache().insert(key, refreshed.clone());
        refreshed
    }

    async fn refresh_rerank_capability(&self) -> EndpointCapabilityLookup {
        let paths = model_discovery_paths(&self.endpoint.base_url);
        let response = self
            .endpoint
            .get_json_paths::<serde_json::Value>(&paths, "rerank capability discovery", "rerank")
            .await;
        match response {
            Ok(value) => match parse_endpoint_capability(&value, &self.endpoint.model) {
                Some(capability) => EndpointCapabilityLookup::refreshed(capability),
                None => EndpointCapabilityLookup::unavailable("capability_absent"),
            },
            Err(error) if is_discovery_unsupported(&error) => {
                EndpointCapabilityLookup::unavailable("discovery_unsupported")
            }
            Err(error) => {
                EndpointCapabilityLookup::refresh_failed(capability_failure_reason(&error))
            }
        }
    }
}

#[async_trait]
impl ChatModel for OpenAiCompatibleChatModel {
    async fn chat(&self, req: ChatRequest) -> ProviderResult<ChatResponse> {
        self.chat_with_operation(req, "chat", "chat").await
    }

    async fn stream_chat(&self, req: ChatRequest) -> ProviderResult<ChatStream> {
        self.stream_chat_with_operation(req, "streaming chat", "chat")
            .await
    }
}

impl OpenAiCompatibleChatModel {
    async fn chat_with_operation(
        &self,
        req: ChatRequest,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<ChatResponse> {
        let body = self.chat_body(req, false);
        let response: OpenAiChatResponse = self
            .endpoint
            .post_json(CHAT_COMPLETIONS_PATH, &body, operation, client_kind)
            .await?;
        chat_response_from_openai(response)
    }

    async fn stream_chat_with_operation(
        &self,
        req: ChatRequest,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<ChatStream> {
        let body = self.chat_body(req, true);
        let (response, context, permit) = self
            .endpoint
            .post_stream(CHAT_COMPLETIONS_PATH, &body, operation, client_kind)
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(operation, &context, status, response).await);
        }

        let chunks = response
            .bytes_stream()
            .map(move |result| match result {
                Ok(bytes) => Ok(bytes.to_vec()),
                Err(source) => {
                    let diagnostic = context.transport_failure(&source);
                    Err(ProviderError::Transport {
                        operation,
                        source,
                        diagnostic: Box::new(diagnostic),
                    })
                }
            })
            .boxed();

        Ok(sse_chat_stream(chunks, permit))
    }

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
        let response = self
            .chat
            .chat_with_operation(chat_req, "vision", "vision")
            .await?;
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
        self.rerank_with_outcome(query, docs, top_n)
            .await
            .map(|outcome| outcome.hits)
            .map_err(|failure| failure.error)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum RerankProviderKind {
    Vllm,
    Cohere,
    Jina,
    OpenAiCompatible,
}

impl RerankProviderKind {
    fn from_provider(provider: &str) -> Self {
        match provider.to_ascii_lowercase().as_str() {
            "cohere" => Self::Cohere,
            "jina" => Self::Jina,
            "openai_compatible" => Self::OpenAiCompatible,
            _ => Self::Vllm,
        }
    }

    fn rerank_paths(self, base_url: &str) -> Vec<&'static str> {
        let trimmed = base_url.trim_end_matches('/');
        if trimmed.ends_with("/v1") || trimmed.ends_with("/v2") {
            return vec![RERANK_PATH];
        }

        match self {
            Self::Cohere => vec![RERANK_V2_PATH, RERANK_V1_PATH],
            Self::Vllm | Self::Jina | Self::OpenAiCompatible => {
                vec![RERANK_V1_PATH, RERANK_V2_PATH]
            }
        }
    }

    fn capability_provider_kind(self) -> &'static str {
        match self {
            Self::Vllm => "vllm",
            Self::Cohere => "cohere",
            Self::Jina => "jina",
            Self::OpenAiCompatible => "openai_compatible",
        }
    }
}

#[derive(Debug)]
struct ProviderRerankOutcome {
    hits: Vec<RerankHit>,
    diagnostics: RerankDiagnostics,
}

#[derive(Debug)]
struct ProviderRerankFailure {
    error: ProviderError,
    diagnostics: RerankDiagnostics,
}

fn rerank_capability_diagnostics(lookup: &EndpointCapabilityLookup) -> RerankCapabilityDiagnostics {
    RerankCapabilityDiagnostics {
        state: rerank_capability_state(lookup.diagnostics.state),
        max_context_tokens: lookup.diagnostics.max_context_tokens,
        max_candidates: lookup.diagnostics.max_candidates,
        max_documents: lookup.diagnostics.max_documents,
        max_document_chars: lookup.diagnostics.max_document_chars,
        max_payload_chars: lookup.diagnostics.max_payload_chars,
        reason: lookup.diagnostics.reason.clone(),
    }
}

fn rerank_capability_state(state: EndpointCapabilityState) -> RerankCapabilityState {
    match state {
        EndpointCapabilityState::Cached => RerankCapabilityState::Cached,
        EndpointCapabilityState::Refreshed => RerankCapabilityState::Refreshed,
        EndpointCapabilityState::Unavailable => RerankCapabilityState::Unavailable,
        EndpointCapabilityState::RefreshFailed => RerankCapabilityState::RefreshFailed,
    }
}

#[derive(Debug)]
struct RerankRequestShape {
    candidate_count: usize,
    document_char_limit: usize,
    top_n: usize,
}

impl RerankRequestShape {
    fn new(
        docs: &[RerankDoc],
        requested_top_n: usize,
        default_top_n: usize,
        capability: Option<&EndpointCapability>,
        force_smaller: bool,
    ) -> Self {
        let base_candidate_count = docs.len();
        let requested_top_n = if requested_top_n == 0 {
            default_top_n
        } else {
            requested_top_n
        };

        let (candidate_count, document_char_limit) = capability
            .map(|capability| {
                capability_request_limits(capability, base_candidate_count, force_smaller)
            })
            .unwrap_or((base_candidate_count, RERANK_MAX_DOCUMENT_CHARS));
        let candidate_count = candidate_count.min(base_candidate_count);
        let top_n = if candidate_count == 0 {
            0
        } else {
            requested_top_n.max(1).min(candidate_count)
        };

        Self {
            candidate_count,
            document_char_limit,
            top_n,
        }
    }

    fn document_texts<'a>(&self, docs: &'a [RerankDoc]) -> Vec<&'a str> {
        docs.iter()
            .take(self.candidate_count)
            .map(|doc| bounded_doc_text(&doc.text, self.document_char_limit))
            .collect()
    }

    fn diagnostics(&self) -> RerankRequestDiagnostics {
        RerankRequestDiagnostics {
            candidate_count: self.candidate_count,
            document_char_limit: self.document_char_limit,
            top_n: self.top_n,
        }
    }
}

fn capability_request_limits(
    capability: &EndpointCapability,
    docs_len: usize,
    force_smaller: bool,
) -> (usize, usize) {
    if docs_len == 0 {
        return (0, RERANK_MAX_DOCUMENT_CHARS);
    }
    let explicit_candidate_count = capability.request_limits.rerank_candidate_count();
    let explicit_document_char_limit = capability.request_limits.max_document_chars;

    let (mut candidate_count, mut document_char_limit) =
        if let Some(max_context_tokens) = capability.max_context_tokens {
            context_budget_request_limits(max_context_tokens, docs_len, force_smaller)
        } else {
            (docs_len, RERANK_MAX_DOCUMENT_CHARS)
        };

    if let Some(limit) = explicit_candidate_count {
        candidate_count = candidate_count.min(limit.max(1));
    }
    candidate_count = candidate_count.min(docs_len).max(1);

    if let Some(limit) = explicit_document_char_limit {
        document_char_limit = document_char_limit.min(limit.max(1));
    }
    document_char_limit = document_char_limit.clamp(1, RERANK_MAX_DOCUMENT_CHARS);
    if force_smaller {
        document_char_limit = document_char_limit.min(RERANK_RETRY_MAX_DOCUMENT_CHARS);
    }

    (candidate_count, document_char_limit)
}

fn context_budget_request_limits(
    max_context_tokens: usize,
    docs_len: usize,
    force_smaller: bool,
) -> (usize, usize) {
    let (numerator, denominator) = if force_smaller {
        (
            RERANK_RETRY_CONTEXT_BUDGET_NUMERATOR,
            RERANK_RETRY_CONTEXT_BUDGET_DENOMINATOR,
        )
    } else {
        (
            RERANK_DOCUMENT_CONTEXT_BUDGET_NUMERATOR,
            RERANK_DOCUMENT_CONTEXT_BUDGET_DENOMINATOR,
        )
    };
    let token_budget = max_context_tokens
        .saturating_mul(numerator)
        .checked_div(denominator)
        .unwrap_or(0)
        .max(1);
    let aggregate_chars = token_budget
        .saturating_mul(RERANK_DOCUMENT_CHARS_PER_CONTEXT_TOKEN)
        .max(1);
    let budget_candidate_count = (aggregate_chars / RERANK_MIN_CHARS_PER_CANDIDATE).max(1);
    let mut candidate_count = docs_len.min(budget_candidate_count);
    if force_smaller && docs_len > 1 {
        candidate_count = candidate_count.min(docs_len.div_ceil(2)).max(1);
    }

    let mut document_char_limit =
        (aggregate_chars / candidate_count.max(1)).clamp(1, RERANK_MAX_DOCUMENT_CHARS);
    if force_smaller {
        document_char_limit = document_char_limit.min(RERANK_RETRY_MAX_DOCUMENT_CHARS);
    }

    (candidate_count, document_char_limit)
}

fn bounded_doc_text(text: &str, max_chars: usize) -> &str {
    if text.chars().count() <= max_chars {
        return text;
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    &text[..end]
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

    async fn rerank_with_diagnostics(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> anyhow::Result<RerankResponse> {
        let docs = docs.iter().cloned().map(RerankDoc::new).collect();
        match self.rerank_with_outcome(query, docs, top_n).await {
            Ok(outcome) => Ok(RerankResponse {
                hits: outcome
                    .hits
                    .into_iter()
                    .map(|hit| (hit.index, hit.score))
                    .collect(),
                diagnostics: outcome.diagnostics,
            }),
            Err(failure) => Err(RerankError::new(failure.error.into(), failure.diagnostics).into()),
        }
    }
}

#[async_trait]
impl crate::traits::Reranker for OpenAiCompatibleLlmReranker {
    async fn rerank(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> anyhow::Result<Vec<(usize, f32)>> {
        Ok(self.rerank_with_diagnostics(query, docs, top_n).await?.hits)
    }

    async fn rerank_with_diagnostics(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> anyhow::Result<RerankResponse> {
        let shape = LlmRerankRequestShape::new(docs, top_n);
        let diagnostics = RerankDiagnostics {
            request: Some(shape.diagnostics()),
            ..RerankDiagnostics::default()
        };
        if shape.candidate_count == 0 {
            return Ok(RerankResponse {
                hits: Vec::new(),
                diagnostics,
            });
        }

        let request = ChatRequest::new(vec![
            ChatMessage::system(LLM_RERANK_SYSTEM_PROMPT),
            ChatMessage::user(llm_rerank_user_prompt(query, docs, &shape)?),
        ])
        .with_temperature(0.0)
        .with_max_tokens(LLM_RERANK_MAX_OUTPUT_TOKENS);

        let response = self
            .chat
            .chat_with_operation(request, "llm rerank", "rerank")
            .await
            .map_err(|error| RerankError::new(error.into(), diagnostics.clone()))?;
        let hits = parse_llm_rerank_response(&response.content, shape.candidate_count)
            .map_err(|error| RerankError::new(error.into(), diagnostics.clone()))?;

        Ok(RerankResponse { hits, diagnostics })
    }
}

const LLM_RERANK_SYSTEM_PROMPT: &str = "\
You are a reranking system. Output only strict JSON. \
Return {\"rankings\":[{\"index\":0,\"score\":0.0}]} using only submitted candidate indexes. \
Scores must be finite numbers where larger means more relevant.";

#[derive(Debug)]
struct LlmRerankRequestShape {
    candidate_count: usize,
    document_char_limit: usize,
    top_n: usize,
}

impl LlmRerankRequestShape {
    fn new(docs: &[String], requested_top_n: usize) -> Self {
        let candidate_count = docs.len().min(LLM_RERANK_MAX_CANDIDATES);
        let top_n = if candidate_count == 0 {
            0
        } else {
            requested_top_n.max(1).min(candidate_count)
        };
        Self {
            candidate_count,
            document_char_limit: LLM_RERANK_MAX_DOCUMENT_CHARS,
            top_n,
        }
    }

    fn diagnostics(&self) -> RerankRequestDiagnostics {
        RerankRequestDiagnostics {
            candidate_count: self.candidate_count,
            document_char_limit: self.document_char_limit,
            top_n: self.top_n,
        }
    }
}

fn llm_rerank_user_prompt(
    query: &str,
    docs: &[String],
    shape: &LlmRerankRequestShape,
) -> ProviderResult<String> {
    let candidates = docs
        .iter()
        .take(shape.candidate_count)
        .enumerate()
        .map(|(index, text)| {
            serde_json::json!({
                "index": index,
                "text": bounded_doc_text(text, shape.document_char_limit),
            })
        })
        .collect::<Vec<_>>();
    let candidates = serde_json::to_string(&candidates)
        .map_err(|error| ProviderError::malformed("llm rerank", error.to_string()))?;
    Ok(format!(
        "Query:\n{query}\n\nReturn the top {top_n} candidates ranked by relevance.\nCandidates JSON:\n{candidates}",
        top_n = shape.top_n
    ))
}

#[derive(Debug, Deserialize)]
struct LlmRerankJsonResponse {
    #[serde(alias = "results", alias = "data")]
    rankings: Vec<LlmRerankJsonItem>,
}

#[derive(Debug, Deserialize)]
struct LlmRerankJsonItem {
    #[serde(alias = "document_index")]
    index: usize,
    #[serde(alias = "relevance_score", alias = "rerank_score")]
    score: f32,
}

fn parse_llm_rerank_response(
    content: &str,
    submitted_candidate_count: usize,
) -> ProviderResult<Vec<(usize, f32)>> {
    let cleaned = strip_json_fence(content);
    let response: LlmRerankJsonResponse =
        serde_json::from_str(cleaned).map_err(|source| ProviderError::MalformedResponse {
            operation: "llm rerank",
            message: format!("model returned invalid JSON: {source}"),
        })?;
    if response.rankings.is_empty() {
        return Err(ProviderError::malformed(
            "llm rerank",
            "response did not include rankings",
        ));
    }
    let mut seen = HashSet::new();
    let mut hits = Vec::with_capacity(response.rankings.len());
    for item in response.rankings {
        if item.index >= submitted_candidate_count {
            return Err(ProviderError::malformed(
                "llm rerank",
                format!(
                    "response included index {} outside submitted candidate count {}",
                    item.index, submitted_candidate_count
                ),
            ));
        }
        if !seen.insert(item.index) {
            return Err(ProviderError::malformed(
                "llm rerank",
                format!("response included duplicate index {}", item.index),
            ));
        }
        if !item.score.is_finite() {
            return Err(ProviderError::malformed(
                "llm rerank",
                "response included a non-finite score",
            ));
        }
        hits.push((item.index, item.score));
    }
    Ok(hits)
}

fn strip_json_fence(content: &str) -> &str {
    let trimmed = content.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim();
    trimmed.strip_suffix("```").unwrap_or(trimmed).trim()
}

#[derive(Clone, Debug)]
struct OpenAiEndpoint {
    runtime: Arc<ModelEndpointRuntime>,
    base_url: String,
    model: String,
    api_key: String,
    timeout: Duration,
    retry: ModelRetryPolicy,
}

impl OpenAiEndpoint {
    #[cfg(test)]
    fn new(base_url: &str, model: &str, api_key: &str, timeout_seconds: u64) -> Self {
        let config = ModelEndpointRuntimeConfig::default();
        Self::new_with_options(base_url, model, api_key, timeout_seconds, &config, "test")
    }

    fn new_with_options(
        base_url: &str,
        model: &str,
        api_key: &str,
        timeout_seconds: u64,
        config: &ModelEndpointRuntimeConfig,
        capability: &'static str,
    ) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let config = config.bounded();
        let runtime =
            endpoint_runtime_registry().runtime_for(&base_url, model, capability, &config);
        Self {
            runtime,
            base_url,
            model: model.to_string(),
            api_key: api_key.to_string(),
            timeout: Duration::from_secs(timeout_seconds.max(1)),
            retry: ModelRetryPolicy::from_config(&config.retry),
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<T> {
        let mut retry_count = 0;
        loop {
            match self
                .post_json_once(path, body, operation, client_kind)
                .await
            {
                Ok(value) => return Ok(value),
                Err(error) if self.retry.should_retry(&error, retry_count) => {
                    retry_count += 1;
                    tracing::warn!(
                        operation,
                        client_kind,
                        retry_count,
                        error = %error,
                        "provider request failed, retrying"
                    );
                    tokio::time::sleep(self.retry.backoff(retry_count)).await;
                }
                Err(error) => return Err(error.with_retry_count(retry_count)),
            }
        }
    }

    async fn post_json_paths<T: serde::de::DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        paths: &[&str],
        body: &B,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<T> {
        let mut last_variant_error = None;
        for (idx, path) in paths.iter().enumerate() {
            match self.post_json(path, body, operation, client_kind).await {
                Ok(value) => return Ok(value),
                Err(error @ ProviderError::HttpStatus { status, .. })
                    if idx + 1 < paths.len()
                        && matches!(
                            status,
                            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                        ) =>
                {
                    last_variant_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_variant_error.unwrap_or_else(|| {
            ProviderError::configuration(operation, "no provider endpoint paths configured")
        }))
    }

    async fn get_json_paths<T: serde::de::DeserializeOwned>(
        &self,
        paths: &[&str],
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<T> {
        let mut last_variant_error = None;
        for (idx, path) in paths.iter().enumerate() {
            match self.get_json_once(path, operation, client_kind).await {
                Ok(value) => return Ok(value),
                Err(error @ ProviderError::HttpStatus { status, .. })
                    if idx + 1 < paths.len()
                        && matches!(
                            status,
                            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                        ) =>
                {
                    last_variant_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_variant_error.unwrap_or_else(|| {
            ProviderError::configuration(operation, "no provider endpoint paths configured")
        }))
    }

    async fn post_json_once<T: serde::de::DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<T> {
        let (response, context, _permit) =
            self.post_once(path, body, operation, client_kind).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(operation, &context, status, response).await);
        }
        decode_json_response(response, operation, &context).await
    }

    async fn get_json_once<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<T> {
        let (response, context, _permit) = self.get_once(path, operation, client_kind).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(operation, &context, status, response).await);
        }
        decode_json_response(response, operation, &context).await
    }

    async fn post_stream<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<(reqwest::Response, UpstreamRequestContext, EndpointPermit)> {
        let mut retry_count = 0;
        loop {
            match self.post_once(path, body, operation, client_kind).await {
                Ok((response, context, permit)) if response.status().is_success() => {
                    return Ok((response, context, permit));
                }
                Ok((response, context, permit)) => {
                    let status = response.status();
                    let error = http_status_error(operation, &context, status, response).await;
                    drop(permit);
                    if self.retry.should_retry(&error, retry_count) {
                        retry_count += 1;
                        tracing::warn!(
                            operation,
                            client_kind,
                            retry_count,
                            error = %error,
                            "provider streaming request failed before body stream, retrying"
                        );
                        tokio::time::sleep(self.retry.backoff(retry_count)).await;
                    } else {
                        return Err(error.with_retry_count(retry_count));
                    }
                }
                Err(error) if self.retry.should_retry(&error, retry_count) => {
                    retry_count += 1;
                    tracing::warn!(
                        operation,
                        client_kind,
                        retry_count,
                        error = %error,
                        "provider streaming request failed, retrying"
                    );
                    tokio::time::sleep(self.retry.backoff(retry_count)).await;
                }
                Err(error) => return Err(error.with_retry_count(retry_count)),
            }
        }
    }

    async fn post_once<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<(reqwest::Response, UpstreamRequestContext, EndpointPermit)> {
        if self.base_url.is_empty() {
            return Err(ProviderError::configuration(operation, "base_url is empty"));
        }
        if self.model.is_empty() {
            return Err(ProviderError::configuration(operation, "model is empty"));
        }

        let url = format!("{}/{}", self.base_url, path);
        let context = UpstreamRequestContext::new(
            operation,
            client_kind,
            Some("openai_compatible".into()),
            Some(self.model.clone()),
            &Method::POST,
            &url,
        );
        let permit = self.runtime.acquire(operation).await?;
        self.auth(
            self.runtime
                .client
                .post(url)
                .timeout(self.timeout)
                .json(body),
        )
        .send()
        .await
        .map(|response| (response, context.clone(), permit))
        .map_err(|source| {
            let diagnostic = context.transport_failure(&source);
            ProviderError::Transport {
                operation,
                source,
                diagnostic: Box::new(diagnostic),
            }
        })
    }

    async fn get_once(
        &self,
        path: &str,
        operation: &'static str,
        client_kind: &'static str,
    ) -> ProviderResult<(reqwest::Response, UpstreamRequestContext, EndpointPermit)> {
        if self.base_url.is_empty() {
            return Err(ProviderError::configuration(operation, "base_url is empty"));
        }

        let url = format!("{}/{}", self.base_url, path);
        let context = UpstreamRequestContext::new(
            operation,
            client_kind,
            Some("openai_compatible".into()),
            Some(self.model.clone()),
            &Method::GET,
            &url,
        );
        let permit = self.runtime.acquire(operation).await?;
        self.auth(self.runtime.client.get(url).timeout(self.timeout))
            .send()
            .await
            .map(|response| (response, context.clone(), permit))
            .map_err(|source| {
                let diagnostic = context.transport_failure(&source);
                ProviderError::Transport {
                    operation,
                    source,
                    diagnostic: Box::new(diagnostic),
                }
            })
    }

    fn auth(&self, req: RequestBuilder) -> RequestBuilder {
        if self.api_key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.api_key)
        }
    }
}

#[derive(Clone, Debug)]
struct ModelEndpointRuntime {
    client: Client,
    resource: Arc<ObservableResource>,
}

impl ModelEndpointRuntime {
    async fn acquire(&self, operation: &'static str) -> ProviderResult<EndpointPermit> {
        self.resource
            .acquire()
            .await
            .map(|permit| EndpointPermit { _permit: permit })
            .map_err(|error| provider_queue_error(operation, error))
    }

    fn configure(&self, config: &ModelEndpointRuntimeConfig) {
        self.resource.configure(endpoint_resource_config(config));
    }
}

#[derive(Debug)]
struct EndpointResourceRegistry {
    runtimes: Mutex<HashMap<EndpointKey, Arc<ModelEndpointRuntime>>>,
}

impl EndpointResourceRegistry {
    fn runtime_for(
        &self,
        base_url: &str,
        model: &str,
        capability: &'static str,
        config: &ModelEndpointRuntimeConfig,
    ) -> Arc<ModelEndpointRuntime> {
        let key = EndpointKey::new(base_url, model, capability);
        let mut runtimes = lock_unpoisoned(&self.runtimes);
        let runtime = runtimes
            .entry(key)
            .or_insert_with(|| {
                let resource = global_resource_registry().resource(
                    endpoint_resource_name(base_url, model, capability),
                    "model_endpoint",
                    endpoint_resource_config(config),
                );
                Arc::new(ModelEndpointRuntime {
                    client: Client::new(),
                    resource,
                })
            })
            .clone();
        runtime.configure(config);
        runtime
    }
}

fn endpoint_runtime_registry() -> &'static EndpointResourceRegistry {
    static REGISTRY: OnceLock<EndpointResourceRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| EndpointResourceRegistry {
        runtimes: Mutex::new(HashMap::new()),
    })
}

#[derive(Clone, Debug, Eq)]
struct EndpointKey {
    endpoint: String,
    model: String,
    capability: &'static str,
}

impl EndpointKey {
    fn new(base_url: &str, model: &str, capability: &'static str) -> Self {
        Self {
            endpoint: normalized_endpoint_key(base_url),
            model: model.to_ascii_lowercase(),
            capability,
        }
    }
}

impl PartialEq for EndpointKey {
    fn eq(&self, other: &Self) -> bool {
        self.endpoint == other.endpoint
            && self.model == other.model
            && self.capability == other.capability
    }
}

impl Hash for EndpointKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.endpoint.hash(state);
        self.model.hash(state);
        self.capability.hash(state);
    }
}

#[derive(Debug)]
struct EndpointPermit {
    _permit: ResourcePermit,
}

fn endpoint_resource_config(config: &ModelEndpointRuntimeConfig) -> ResourceLimitConfig {
    let config = config.bounded();
    ResourceLimitConfig {
        capacity: config.max_concurrent_requests,
        queue_capacity: config.queue_capacity,
        queue_timeout: Duration::from_secs(config.queue_timeout_seconds),
    }
}

fn endpoint_resource_name(base_url: &str, model: &str, capability: &'static str) -> String {
    let endpoint = normalized_endpoint_key(base_url);
    let model = model.to_ascii_lowercase();
    let fingerprint = hex_sha256(format!("{endpoint}\0{model}\0{capability}").as_bytes());
    format!("model_endpoint:{capability}:{}", &fingerprint[..16])
}

fn provider_queue_error(operation: &'static str, error: ResourceQueueError) -> ProviderError {
    match error {
        ResourceQueueError::Timeout { timeout, .. } => ProviderError::QueueTimeout {
            operation,
            timeout_seconds: timeout.as_secs(),
        },
        ResourceQueueError::Full { .. } => ProviderError::QueueFull { operation },
    }
}

pub fn model_endpoint_resource_snapshots() -> Vec<ResourceQueueSnapshot> {
    global_resource_registry()
        .snapshots()
        .into_iter()
        .filter(|snapshot| snapshot.kind == "model_endpoint")
        .collect()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Clone, Debug)]
struct ModelRetryPolicy {
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl ModelRetryPolicy {
    fn from_config(config: &ModelRetryConfig) -> Self {
        let config = config.bounded();
        Self {
            max_retries: config.max_retries,
            initial_backoff: Duration::from_millis(config.initial_backoff_millis),
            max_backoff: Duration::from_millis(config.max_backoff_millis),
        }
    }

    fn should_retry(&self, error: &ProviderError, retry_count: u32) -> bool {
        retry_count < self.max_retries && error.is_retryable_provider_failure()
    }

    fn backoff(&self, retry_count: u32) -> Duration {
        let exponent = retry_count.saturating_sub(1).min(16);
        let multiplier = 1_u128 << exponent;
        let base_millis = self
            .initial_backoff
            .as_millis()
            .saturating_mul(multiplier)
            .min(self.max_backoff.as_millis());
        let jitter_window = (base_millis / 4).max(1);
        let jitter = current_time_nanos() % jitter_window;
        duration_from_millis(base_millis.saturating_add(jitter))
    }
}

fn duration_from_millis(value: u128) -> Duration {
    Duration::from_millis(value.min(u128::from(u64::MAX)) as u64)
}

fn current_time_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
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
    #[serde(default)]
    rankings: Vec<OpenAiRerankResult>,
}

impl OpenAiRerankResponse {
    fn into_hits(self) -> Vec<RerankHit> {
        let results = if !self.results.is_empty() {
            self.results
        } else if !self.data.is_empty() {
            self.data
        } else {
            self.rankings
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
    #[serde(alias = "document_index")]
    index: usize,
    #[serde(alias = "relevance_score", alias = "rerank_score")]
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
    context: &UpstreamRequestContext,
    status: StatusCode,
    response: reqwest::Response,
) -> ProviderError {
    let captured = capture_response_prefix(response, DEFAULT_BODY_PREFIX_MAX_BYTES).await;
    let body = String::from_utf8_lossy(&captured.body);
    let message = openai_error_message(&body)
        .unwrap_or_else(|| "provider returned a non-success response".to_string());
    let diagnostic = captured.diagnostic_for_status(context);
    ProviderError::HttpStatus {
        operation,
        status,
        message,
        diagnostic: Box::new(diagnostic),
    }
}

async fn decode_json_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    operation: &'static str,
    context: &UpstreamRequestContext,
) -> ProviderResult<T> {
    let status = response.status();
    let headers = response.headers().clone();
    let captured = capture_full_response(response).await.map_err(|source| {
        let diagnostic = context.body_read_failure(Some(status), &headers, &source);
        ProviderError::Transport {
            operation,
            source,
            diagnostic: Box::new(diagnostic),
        }
    })?;
    serde_json::from_slice::<T>(&captured.body).map_err(|source| {
        let diagnostic = captured.diagnostic_for_decode(context, &source);
        ProviderError::ResponseDecode {
            operation,
            source,
            diagnostic: Box::new(diagnostic),
        }
    })
}

fn openai_error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error = value.get("error")?;
    let raw_message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provider error");
    Some(truncate_message(&sanitize_text(raw_message), 240))
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

fn sse_chat_stream(
    chunks: BoxStream<'static, ProviderResult<Vec<u8>>>,
    permit: EndpointPermit,
) -> ChatStream {
    struct State {
        chunks: BoxStream<'static, ProviderResult<Vec<u8>>>,
        pending_utf8: Vec<u8>,
        buffer: String,
        pending: VecDeque<ProviderResult<ChatStreamEvent>>,
        done: bool,
        _permit: EndpointPermit,
    }

    let state = State {
        chunks,
        pending_utf8: Vec::new(),
        buffer: String::new(),
        pending: VecDeque::new(),
        done: false,
        _permit: permit,
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
    use crate::config::RerankStrategy;
    use crate::provider::{
        ChatMessageContent, ImageInput, RerankDoc, Reranker as ProviderReranker,
    };
    use futures::stream;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    #[derive(Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: String,
    }

    fn spawn_json_server(
        status: &'static str,
        response_body: &'static str,
    ) -> (String, thread::JoinHandle<RecordedRequest>) {
        spawn_response_server(status, "application/json", response_body)
    }

    fn spawn_response_server(
        status: &'static str,
        content_type: &'static str,
        response_body: &'static str,
    ) -> (String, thread::JoinHandle<RecordedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!("http://{}", listener.local_addr().expect("server addr"));
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let request = read_http_request(&mut stream);
            write_http_response(&mut stream, status, content_type, response_body);
            request
        });
        (base_url, handle)
    }

    fn spawn_response_sequence_server(
        responses: Vec<(&'static str, &'static str, &'static str)>,
    ) -> (
        String,
        Arc<Mutex<mpsc::Receiver<RecordedRequest>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!("http://{}", listener.local_addr().expect("server addr"));
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            for (status, content_type, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept request");
                let request = read_http_request(&mut stream);
                request_tx.send(request).expect("record request");
                write_http_response(&mut stream, status, content_type, body);
            }
        });
        (base_url, Arc::new(Mutex::new(request_rx)), handle)
    }

    struct BlockingFirstEmbeddingServer {
        base_url: String,
        requests: Arc<Mutex<mpsc::Receiver<RecordedRequest>>>,
        release_first_response: mpsc::Sender<()>,
        handle: thread::JoinHandle<()>,
    }

    fn spawn_blocking_first_embedding_server() -> BlockingFirstEmbeddingServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!("http://{}", listener.local_addr().expect("server addr"));
        let (request_tx, request_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept first request");
            let request = read_http_request(&mut stream);
            request_tx.send(request).expect("record first request");
            release_rx.recv().expect("release first response");
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json",
                embedding_response_body(),
            );

            let (mut stream, _) = listener.accept().expect("accept second request");
            let request = read_http_request(&mut stream);
            request_tx.send(request).expect("record second request");
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json",
                embedding_response_body(),
            );
        });
        BlockingFirstEmbeddingServer {
            base_url,
            requests: Arc::new(Mutex::new(request_rx)),
            release_first_response: release_tx,
            handle,
        }
    }

    fn spawn_embedding_sequence_server(
        statuses: Vec<&'static str>,
    ) -> (
        String,
        Arc<Mutex<mpsc::Receiver<RecordedRequest>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!("http://{}", listener.local_addr().expect("server addr"));
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().expect("accept request");
                let request = read_http_request(&mut stream);
                request_tx.send(request).expect("record request");
                let body = if status.starts_with('2') {
                    embedding_response_body()
                } else {
                    r#"{"error":{"message":"fixture provider failure"}}"#
                };
                write_http_response(&mut stream, status, "application/json", body);
            }
        });
        (base_url, Arc::new(Mutex::new(request_rx)), handle)
    }

    fn spawn_chat_stream_sequence_server(
        statuses: Vec<&'static str>,
    ) -> (
        String,
        Arc<Mutex<mpsc::Receiver<RecordedRequest>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!("http://{}", listener.local_addr().expect("server addr"));
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().expect("accept request");
                let request = read_http_request(&mut stream);
                request_tx.send(request).expect("record request");
                let (content_type, body) = if status.starts_with('2') {
                    ("text/event-stream", chat_stream_response_body())
                } else {
                    (
                        "application/json",
                        r#"{"error":{"message":"fixture provider failure"}}"#,
                    )
                };
                write_http_response(&mut stream, status, content_type, body);
            }
        });
        (base_url, Arc::new(Mutex::new(request_rx)), handle)
    }

    async fn recv_recorded_request(
        rx: Arc<Mutex<mpsc::Receiver<RecordedRequest>>>,
        timeout: Duration,
    ) -> Option<RecordedRequest> {
        tokio::task::spawn_blocking(move || {
            let rx = rx.lock().expect("request receiver lock");
            rx.recv_timeout(timeout).ok()
        })
        .await
        .expect("request receiver task joins")
    }

    async fn collect_recorded_requests(
        rx: Arc<Mutex<mpsc::Receiver<RecordedRequest>>>,
        count: usize,
    ) -> Vec<RecordedRequest> {
        let mut requests = Vec::with_capacity(count);
        for _ in 0..count {
            requests.push(
                recv_recorded_request(Arc::clone(&rx), Duration::from_secs(1))
                    .await
                    .expect("request recorded"),
            );
        }
        assert!(
            recv_recorded_request(rx, Duration::from_millis(75))
                .await
                .is_none(),
            "unexpected extra request recorded"
        );
        requests
    }

    fn embedding_response_body() -> &'static str {
        r#"{"data":[{"embedding":[1.0,0.0,0.0]}]}"#
    }

    fn chat_stream_response_body() -> &'static str {
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
    }

    fn test_runtime_config(
        max_concurrent_requests: usize,
        max_retries: u32,
        initial_backoff_millis: u64,
    ) -> ModelEndpointRuntimeConfig {
        ModelEndpointRuntimeConfig {
            max_concurrent_requests,
            queue_capacity: 128,
            queue_timeout_seconds: 1,
            retry: ModelRetryConfig {
                max_retries,
                initial_backoff_millis,
                max_backoff_millis: initial_backoff_millis,
            },
        }
    }

    #[test]
    fn endpoint_key_normalizes_endpoint_model_and_separates_capability() {
        let left = EndpointKey::new("HTTP://LOCALHOST:8080/v1/", "Embedding-Model", "embedding");
        let same = EndpointKey::new("http://localhost:8080/v1", "embedding-model", "embedding");
        let other_model = EndpointKey::new("http://localhost:8080/v1", "chat-model", "embedding");
        let other_capability =
            EndpointKey::new("http://localhost:8080/v1", "embedding-model", "rerank");

        assert_eq!(left, same);
        assert_ne!(left, other_model);
        assert_ne!(left, other_capability);
    }

    #[test]
    fn endpoint_resource_name_uses_stable_redacted_fingerprint() {
        let name = endpoint_resource_name(
            "HTTP://LOCALHOST:8080/v1/",
            "Secret-Embedding-Model",
            "embedding",
        );
        let same = endpoint_resource_name(
            "http://localhost:8080/v1",
            "secret-embedding-model",
            "embedding",
        );
        let other_model =
            endpoint_resource_name("http://localhost:8080/v1", "other-model", "embedding");

        assert_eq!(name, same);
        assert_ne!(name, other_model);
        assert!(name.starts_with("model_endpoint:embedding:"));
        assert!(!name.contains("localhost"));
        assert!(!name.contains("8080"));
        assert!(!name.contains("Secret-Embedding-Model"));
        assert!(!name.contains("secret-embedding-model"));
    }

    fn embedding_model_with_runtime(
        base_url: &str,
        runtime: &ModelEndpointRuntimeConfig,
    ) -> OpenAiCompatibleEmbeddingModel {
        OpenAiCompatibleEmbeddingModel {
            endpoint: OpenAiEndpoint::new_with_options(
                base_url,
                "embedding-model",
                "",
                5,
                runtime,
                "embedding",
            ),
            provider_kind: "openai_compatible".into(),
            dimension: 3,
            normalize: false,
            batch_size: 16,
            capability_cache_ttl: Duration::from_secs(60),
            query_instruction: String::new(),
            document_instruction: String::new(),
        }
    }

    fn chat_model_with_runtime(
        base_url: &str,
        runtime: &ModelEndpointRuntimeConfig,
    ) -> OpenAiCompatibleChatModel {
        OpenAiCompatibleChatModel {
            endpoint: OpenAiEndpoint::new_with_options(
                base_url,
                "chat-model",
                "",
                5,
                runtime,
                "chat",
            ),
            temperature: 0.0,
        }
    }

    async fn test_stream_permit() -> EndpointPermit {
        let resource = global_resource_registry().resource(
            "model_endpoint:test_stream:chat",
            "model_endpoint",
            endpoint_resource_config(&test_runtime_config(1, 0, 1)),
        );
        EndpointPermit {
            _permit: resource.acquire().await.expect("test stream permit"),
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> RecordedRequest {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 512];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("read request");
            assert!(read > 0, "request closed before headers");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = find_header_end(&buffer) {
                break position;
            }
        };

        let headers = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let body_start = header_end + 4;
        while buffer.len().saturating_sub(body_start) < content_length {
            let read = stream.read(&mut chunk).expect("read body");
            assert!(read > 0, "request closed before body");
            buffer.extend_from_slice(&chunk[..read]);
        }

        let mut request_line = headers
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace();
        let method = request_line.next().unwrap_or_default().to_string();
        let path = request_line.next().unwrap_or_default().to_string();
        let body = String::from_utf8(buffer[body_start..body_start + content_length].to_vec())
            .expect("request body utf8");

        RecordedRequest { method, path, body }
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn write_http_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nX-Request-Id: req-fixture-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    }

    #[tokio::test]
    async fn endpoints_with_same_normalized_base_url_share_capacity() {
        let server = spawn_blocking_first_embedding_server();
        let runtime = test_runtime_config(1, 0, 1);
        let first_model = embedding_model_with_runtime(&server.base_url, &runtime);
        let second_model = embedding_model_with_runtime(&format!("{}/", server.base_url), &runtime);

        let first =
            tokio::spawn(async move { first_model.embed_prepared(vec!["first".into()]).await });
        let first_request =
            recv_recorded_request(Arc::clone(&server.requests), Duration::from_secs(1))
                .await
                .expect("first request reaches server");
        assert!(first_request.body.contains("first"));

        let second =
            tokio::spawn(async move { second_model.embed_prepared(vec!["second".into()]).await });
        assert!(
            recv_recorded_request(Arc::clone(&server.requests), Duration::from_millis(75))
                .await
                .is_none(),
            "second request must wait for shared endpoint capacity"
        );

        server
            .release_first_response
            .send(())
            .expect("release first response");
        let second_request =
            recv_recorded_request(Arc::clone(&server.requests), Duration::from_secs(1))
                .await
                .expect("second request reaches server after release");
        assert!(second_request.body.contains("second"));
        first
            .await
            .expect("first task joins")
            .expect("first succeeds");
        second
            .await
            .expect("second task joins")
            .expect("second succeeds");
        server.handle.join().expect("server thread joins");
    }

    #[tokio::test]
    async fn retry_releases_endpoint_capacity_before_backoff_sleep() {
        let (base_url, request_rx, handle) =
            spawn_embedding_sequence_server(vec!["500 Internal Server Error", "200 OK", "200 OK"]);
        let runtime = test_runtime_config(1, 1, 250);
        let first_model = embedding_model_with_runtime(&base_url, &runtime);
        let competing_model = embedding_model_with_runtime(&base_url, &runtime);

        let first =
            tokio::spawn(async move { first_model.embed_prepared(vec!["first".into()]).await });
        let first_request = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("first request reaches server");
        assert!(first_request.body.contains("first"));

        let competing = tokio::spawn(async move {
            competing_model
                .embed_prepared(vec!["competing".into()])
                .await
        });
        let competing_request =
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_millis(120))
                .await
                .expect("competing request acquires during first retry backoff");
        assert!(competing_request.body.contains("competing"));

        let retry_request = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("first retry reaches server");
        assert!(retry_request.body.contains("first"));
        competing
            .await
            .expect("competing task joins")
            .expect("competing request succeeds");
        first
            .await
            .expect("first task joins")
            .expect("first retry succeeds");
        handle.join().expect("server thread joins");
    }

    #[tokio::test]
    async fn streaming_retry_releases_endpoint_capacity_before_backoff_sleep() {
        let (base_url, request_rx, handle) = spawn_chat_stream_sequence_server(vec![
            "500 Internal Server Error",
            "200 OK",
            "200 OK",
        ]);
        let runtime = test_runtime_config(1, 1, 250);
        let first_model = chat_model_with_runtime(&base_url, &runtime);
        let competing_model = chat_model_with_runtime(&base_url, &runtime);

        let first = tokio::spawn(async move {
            let stream = first_model
                .stream_chat(ChatRequest::new(vec![ChatMessage::user("first")]))
                .await?;
            stream
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<ProviderResult<Vec<_>>>()
        });
        let first_request = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("first streaming request reaches server");
        assert!(first_request.body.contains("first"));

        let competing = tokio::spawn(async move {
            let stream = competing_model
                .stream_chat(ChatRequest::new(vec![ChatMessage::user("competing")]))
                .await?;
            stream
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<ProviderResult<Vec<_>>>()
        });
        let competing_request =
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_millis(120))
                .await
                .expect("competing streaming request acquires during first retry backoff");
        assert!(competing_request.body.contains("competing"));

        let retry_request = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("first streaming retry reaches server");
        assert!(retry_request.body.contains("first"));
        competing
            .await
            .expect("competing task joins")
            .expect("competing request succeeds");
        first
            .await
            .expect("first task joins")
            .expect("first retry succeeds");
        handle.join().expect("server thread joins");
    }

    #[tokio::test]
    async fn retryable_status_codes_are_retried() {
        let (base_url, request_rx, handle) =
            spawn_embedding_sequence_server(vec!["429 Too Many Requests", "200 OK"]);
        let runtime = test_runtime_config(1, 1, 1);
        let model = embedding_model_with_runtime(&base_url, &runtime);

        let embeddings = model
            .embed_prepared(vec!["document".into()])
            .await
            .expect("retry succeeds");

        assert_eq!(embeddings, vec![vec![1.0, 0.0, 0.0]]);
        assert!(
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
                .await
                .is_some()
        );
        assert!(
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
                .await
                .is_some()
        );
        handle.join().expect("server thread joins");
    }

    #[tokio::test]
    async fn non_retryable_client_status_is_not_retried() {
        let (base_url, request_rx, handle) =
            spawn_embedding_sequence_server(vec!["400 Bad Request"]);
        let runtime = test_runtime_config(1, 3, 1);
        let model = embedding_model_with_runtime(&base_url, &runtime);

        let error = model
            .embed_prepared(vec!["document".into()])
            .await
            .expect_err("400 response fails without retry");

        let ProviderError::HttpStatus {
            status, diagnostic, ..
        } = error
        else {
            panic!("expected HTTP status error");
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(diagnostic.retry_count, Some(0));
        assert!(
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
                .await
                .is_some()
        );
        assert!(
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_millis(75))
                .await
                .is_none(),
            "400 response must not be retried"
        );
        handle.join().expect("server thread joins");
    }

    #[tokio::test]
    async fn embedding_payload_limit_forces_capability_refresh_and_splits_batch() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "413 Payload Too Large",
                "application/json",
                r#"{"error":{"message":"payload too large for embedding batch"}}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"embedding-model","max_batch_size":1}]}"#,
            ),
            ("200 OK", "application/json", embedding_response_body()),
            ("200 OK", "application/json", embedding_response_body()),
        ]);
        let runtime = test_runtime_config(1, 0, 1);
        let model = embedding_model_with_runtime(&base_url, &runtime);

        let embeddings = model
            .embed_prepared(vec!["first".into(), "second".into()])
            .await
            .expect("payload-limit refresh splits embedding batch");

        assert_eq!(embeddings, vec![vec![1.0, 0.0, 0.0], vec![1.0, 0.0, 0.0]]);
        let requests = collect_recorded_requests(request_rx, 4).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/embeddings", "/v1/models", "/embeddings", "/embeddings"]
        );
        let first_body: serde_json::Value =
            serde_json::from_str(&requests[0].body).expect("first embedding body");
        let retry_body: serde_json::Value =
            serde_json::from_str(&requests[2].body).expect("retry embedding body");
        assert_eq!(first_body["input"].as_array().unwrap().len(), 2);
        assert_eq!(retry_body["input"].as_array().unwrap().len(), 1);
        assert_eq!(retry_body["input"][0], "first");
    }

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
            provider_kind: "openai_compatible".into(),
            dimension: 3,
            normalize: false,
            batch_size: 16,
            capability_cache_ttl: Duration::from_secs(60),
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

        let response: OpenAiRerankResponse =
            serde_json::from_str(r#"{"rankings":[{"document_index":3,"rerank_score":0.6}]}"#)
                .expect("parse jina-style rerank ranking");
        assert_eq!(
            response.into_hits(),
            vec![RerankHit {
                index: 3,
                score: 0.6
            }]
        );
    }

    #[tokio::test]
    async fn reranker_posts_vllm_request_to_v1_rerank() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"rerank-model","max_model_len":8192}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":1,"relevance_score":0.93}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "rerank-model".into(),
            top_n: 2,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);

        let hits = ProviderReranker::rerank(
            &reranker,
            "alpha?",
            vec![RerankDoc::new("first"), RerankDoc::new("second")],
            1,
        )
        .await
        .expect("rerank request succeeds");
        let discovery_request =
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
                .await
                .expect("discovery request recorded");
        let request = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("rerank request recorded");
        handle.join().expect("server thread joins");
        let body: serde_json::Value = serde_json::from_str(&request.body).expect("request json");

        assert_eq!(discovery_request.method, "GET");
        assert_eq!(discovery_request.path, "/v1/models");
        assert_eq!(request.path, "/v1/rerank");
        assert_eq!(body["model"], "rerank-model");
        assert_eq!(body["query"], "alpha?");
        assert_eq!(body["documents"][0], "first");
        assert_eq!(body["documents"][1], "second");
        assert_eq!(body["top_n"], 1);
        assert_eq!(
            hits,
            vec![RerankHit {
                index: 1,
                score: 0.93
            }]
        );
    }

    #[tokio::test]
    async fn reranker_posts_cohere_request_to_v2_rerank() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"cohere-rerank","max_context_length":4096}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.88}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "cohere".into(),
            base_url,
            model: "cohere-rerank".into(),
            top_n: 1,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);

        let hits = ProviderReranker::rerank(
            &reranker,
            "alpha?",
            vec![RerankDoc::new("first"), RerankDoc::new("second")],
            1,
        )
        .await
        .expect("rerank request succeeds");
        let discovery_request =
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
                .await
                .expect("discovery request recorded");
        let request = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("rerank request recorded");
        handle.join().expect("server thread joins");

        assert_eq!(discovery_request.path, "/v1/models");
        assert_eq!(request.path, "/v2/rerank");
        assert_eq!(
            hits,
            vec![RerankHit {
                index: 0,
                score: 0.88
            }]
        );
    }

    #[tokio::test]
    async fn reranker_uses_explicit_request_shaping_hints() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"hint-model","max_candidates":2,"max_document_chars":5}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.99}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "hint-model".into(),
            top_n: 4,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);
        let docs = vec![
            "alpha-long".to_string(),
            "bravo-long".to_string(),
            "charlie-long".to_string(),
            "delta-long".to_string(),
        ];

        let response =
            <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
                &reranker, "query", &docs, 4,
            )
            .await
            .expect("rerank succeeds with explicit shaping hints");

        assert_eq!(response.hits, vec![(0, 0.99)]);
        let capability = response
            .diagnostics
            .capability
            .as_ref()
            .expect("capability diagnostics");
        assert_eq!(capability.max_candidates, Some(2));
        assert_eq!(capability.max_document_chars, Some(5));
        let request = response
            .diagnostics
            .request
            .as_ref()
            .expect("request diagnostics");
        assert_eq!(request.candidate_count, 2);
        assert_eq!(request.document_char_limit, 5);
        assert_eq!(request.top_n, 2);

        let requests = collect_recorded_requests(request_rx, 2).await;
        handle.join().expect("server thread joins");
        let body: serde_json::Value = serde_json::from_str(&requests[1].body).expect("rerank body");
        assert_eq!(body["documents"].as_array().unwrap().len(), 2);
        assert_eq!(body["documents"][0], "alpha");
        assert_eq!(body["documents"][1], "bravo");
        assert_eq!(body["top_n"], 2);
    }

    #[tokio::test]
    async fn reranker_uses_cached_capability_until_explicit_refresh() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"cache-model","max_model_len":4096}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.91}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.92}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"cache-model","max_model_len":2048}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "cache-model".into(),
            top_n: 1,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);

        ProviderReranker::rerank(&reranker, "alpha?", vec![RerankDoc::new("first")], 1)
            .await
            .expect("first rerank succeeds");
        ProviderReranker::rerank(&reranker, "alpha?", vec![RerankDoc::new("first")], 1)
            .await
            .expect("second rerank succeeds with cached capability");
        let refreshed = reranker.load_rerank_capability(true).await;

        assert_eq!(
            refreshed.diagnostics.state,
            EndpointCapabilityState::Refreshed
        );
        assert_eq!(refreshed.diagnostics.max_context_tokens, Some(2048));
        let first = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("first discovery recorded");
        let second = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("first rerank recorded");
        let third = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("second rerank recorded");
        let fourth = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("explicit refresh recorded");
        handle.join().expect("server thread joins");

        assert_eq!(first.path, "/v1/models");
        assert_eq!(second.path, "/v1/rerank");
        assert_eq!(third.path, "/v1/rerank");
        assert_eq!(fourth.path, "/v1/models");
    }

    #[tokio::test]
    async fn rerank_context_limit_400_refreshes_once_and_retries_bounded() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"ctx-model","max_model_len":8192}]}"#,
            ),
            (
                "400 Bad Request",
                "application/json",
                r#"{"error":{"message":"context length exceeds max_model_len"}}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"ctx-model","max_model_len":512}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.97}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "ctx-model".into(),
            top_n: 6,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);
        let docs = vec!["x".repeat(8_000); 6];

        let response =
            <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
                &reranker,
                "secret query token=fixture-query",
                &docs,
                6,
            )
            .await
            .expect("context-limit retry succeeds");

        assert_eq!(response.hits, vec![(0, 0.97)]);
        assert!(response.diagnostics.retried_after_context_limit);
        let capability = response
            .diagnostics
            .capability
            .as_ref()
            .expect("capability diagnostics");
        assert_eq!(capability.state, RerankCapabilityState::Refreshed);
        assert_eq!(capability.max_context_tokens, Some(512));
        let retry_request = response
            .diagnostics
            .request
            .as_ref()
            .expect("request diagnostics");
        assert_eq!(retry_request.candidate_count, 1);
        assert_eq!(retry_request.document_char_limit, 768);
        assert_eq!(retry_request.top_n, 1);

        let requests = collect_recorded_requests(request_rx, 4).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/v1/models", "/v1/rerank", "/v1/models", "/v1/rerank"]
        );
        let first_body: serde_json::Value =
            serde_json::from_str(&requests[1].body).expect("first rerank body");
        let retry_body: serde_json::Value =
            serde_json::from_str(&requests[3].body).expect("retry rerank body");
        assert_eq!(first_body["documents"].as_array().unwrap().len(), 6);
        assert_eq!(retry_body["documents"].as_array().unwrap().len(), 1);
        assert!(
            first_body["documents"][0].as_str().unwrap().len()
                > retry_body["documents"][0].as_str().unwrap().len()
        );
    }

    #[tokio::test]
    async fn rerank_payload_limit_413_refreshes_once_and_retries_bounded() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"payload-model","max_model_len":4096}]}"#,
            ),
            (
                "413 Payload Too Large",
                "application/json",
                r#"{"error":{"message":"payload too large for maximum context"}}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"payload-model","max_sequence_length":512}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.87}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "payload-model".into(),
            top_n: 4,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);
        let docs = vec!["payload".repeat(1_000); 4];

        let response =
            <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
                &reranker,
                "payload query",
                &docs,
                4,
            )
            .await
            .expect("payload-limit retry succeeds");

        assert_eq!(response.hits, vec![(0, 0.87)]);
        assert!(response.diagnostics.retried_after_context_limit);
        assert_eq!(
            response
                .diagnostics
                .capability
                .as_ref()
                .unwrap()
                .max_context_tokens,
            Some(512)
        );
        let requests = collect_recorded_requests(request_rx, 4).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/v1/models", "/v1/rerank", "/v1/models", "/v1/rerank"]
        );
    }

    #[tokio::test]
    async fn rerank_context_limit_retry_is_exhausted_after_one_retry() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"exhaust-model","max_model_len":8192}]}"#,
            ),
            (
                "422 Unprocessable Entity",
                "application/json",
                r#"{"error":{"message":"input too long for context window"}}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"exhaust-model","max_model_len":512}]}"#,
            ),
            (
                "422 Unprocessable Entity",
                "application/json",
                r#"{"error":{"message":"input too long for context window"}}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "exhaust-model".into(),
            top_n: 2,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);
        let docs = vec!["doc".repeat(2_000); 2];

        let error = <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
            &reranker, "query", &docs, 2,
        )
        .await
        .expect_err("retry context-limit failure is returned");
        let rerank_error = error
            .downcast_ref::<RerankError>()
            .expect("error carries rerank diagnostics");

        assert!(rerank_error.diagnostics().retried_after_context_limit);
        assert_eq!(
            rerank_error
                .diagnostics()
                .capability
                .as_ref()
                .and_then(|capability| capability.max_context_tokens),
            Some(512)
        );
        let requests = collect_recorded_requests(request_rx, 4).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/v1/models", "/v1/rerank", "/v1/models", "/v1/rerank"]
        );
    }

    #[tokio::test]
    async fn rerank_context_limit_refresh_failure_does_not_retry_rerank() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"refresh-fail-model","max_model_len":8192}]}"#,
            ),
            (
                "400 Bad Request",
                "application/json",
                r#"{"error":{"message":"prompt too long for maximum context"}}"#,
            ),
            (
                "500 Internal Server Error",
                "application/json",
                r#"{"error":{"message":"metadata service unavailable"}}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "refresh-fail-model".into(),
            top_n: 2,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);
        let docs = vec!["doc".repeat(2_000); 2];

        let error = <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
            &reranker, "query", &docs, 2,
        )
        .await
        .expect_err("refresh failure preserves original context error");
        let rerank_error = error
            .downcast_ref::<RerankError>()
            .expect("error carries rerank diagnostics");
        let capability = rerank_error
            .diagnostics()
            .capability
            .as_ref()
            .expect("capability diagnostics");

        assert_eq!(capability.state, RerankCapabilityState::RefreshFailed);
        assert_eq!(
            capability.reason.as_deref(),
            Some("discovery_http_status_500")
        );
        assert!(!rerank_error.diagnostics().retried_after_context_limit);
        let requests = collect_recorded_requests(request_rx, 3).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/v1/models", "/v1/rerank", "/v1/models"]
        );
    }

    #[tokio::test]
    async fn rerank_non_context_400_is_not_refreshed_or_retried() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"bad-request-model","max_model_len":8192}]}"#,
            ),
            (
                "400 Bad Request",
                "application/json",
                r#"{"error":{"message":"unknown parameter top_n_extra"}}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "bad-request-model".into(),
            top_n: 1,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);

        let error = ProviderReranker::rerank(&reranker, "query", vec![RerankDoc::new("doc")], 1)
            .await
            .expect_err("non-context 400 fails");

        let ProviderError::HttpStatus {
            status, diagnostic, ..
        } = error
        else {
            panic!("expected HTTP status error");
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(diagnostic.retry_count, Some(0));
        let requests = collect_recorded_requests(request_rx, 2).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/v1/models", "/v1/rerank"]
        );
    }

    #[tokio::test]
    async fn rerank_discovery_unsupported_fails_soft_and_still_reranks() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "404 Not Found",
                "application/json",
                r#"{"error":{"message":"not found"}}"#,
            ),
            (
                "404 Not Found",
                "application/json",
                r#"{"error":{"message":"not found"}}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.7}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "unsupported-models".into(),
            top_n: 1,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);
        let docs = vec!["doc".to_string()];

        let response =
            <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
                &reranker, "query", &docs, 1,
            )
            .await
            .expect("rerank succeeds without discovery");

        assert_eq!(response.hits, vec![(0, 0.7)]);
        let capability = response.diagnostics.capability.as_ref().unwrap();
        assert_eq!(capability.state, RerankCapabilityState::Unavailable);
        assert_eq!(capability.reason.as_deref(), Some("discovery_unsupported"));
        let requests = collect_recorded_requests(request_rx, 3).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/v1/models", "/models", "/v1/rerank"]
        );
    }

    #[tokio::test]
    async fn rerank_discovery_without_capability_fields_fails_soft() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"fieldless-model","object":"model"}]}"#,
            ),
            (
                "200 OK",
                "application/json",
                r#"{"results":[{"index":0,"relevance_score":0.72}]}"#,
            ),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "fieldless-model".into(),
            top_n: 1,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);
        let docs = vec!["doc".to_string()];

        let response =
            <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
                &reranker, "query", &docs, 1,
            )
            .await
            .expect("rerank succeeds with unparseable discovery");

        assert_eq!(response.hits, vec![(0, 0.72)]);
        let capability = response.diagnostics.capability.as_ref().unwrap();
        assert_eq!(capability.state, RerankCapabilityState::Unavailable);
        assert_eq!(capability.reason.as_deref(), Some("capability_absent"));
        let requests = collect_recorded_requests(request_rx, 2).await;
        handle.join().expect("server thread joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/v1/models", "/v1/rerank"]
        );
    }

    #[tokio::test]
    async fn llm_rerank_malformed_json_returns_diagnostic_rerank_error() {
        let body = r#"{"choices":[{"message":{"content":"not json"},"finish_reason":"stop"}]}"#;
        let (base_url, handle) = spawn_response_server("200 OK", "application/json", body);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Llm,
            provider: "openai_compatible".into(),
            base_url,
            model: "llm-reranker".into(),
            top_n: 1,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleLlmReranker::from_config(&config);
        let docs = vec!["alpha document".to_string()];

        let error =
            <OpenAiCompatibleLlmReranker as crate::traits::Reranker>::rerank_with_diagnostics(
                &reranker, "alpha?", &docs, 1,
            )
            .await
            .expect_err("malformed LLM rerank output fails");
        let request = handle.join().expect("server thread joins");
        let rerank_error = error
            .downcast_ref::<RerankError>()
            .expect("error carries rerank diagnostics");

        assert_eq!(request.path, "/chat/completions");
        assert!(request.body.contains("alpha document"));
        assert_eq!(
            rerank_error.diagnostics().request.as_ref(),
            Some(&RerankRequestDiagnostics {
                candidate_count: 1,
                document_char_limit: LLM_RERANK_MAX_DOCUMENT_CHARS,
                top_n: 1,
            })
        );
    }

    #[tokio::test]
    async fn chat_invalid_json_error_includes_sanitized_response_diagnostic() {
        let body = concat!(
            "Authorization: Bearer fixturebearertoken\n",
            "not json token=fixture12345 ",
            "OPENAI_API_KEY=providerfixture12345 ",
            "https://user:pass@example.test/path?token=fixture12345"
        );
        let (base_url, handle) = spawn_response_server("200 OK", "text/html", body);
        let model = OpenAiCompatibleChatModel {
            endpoint: OpenAiEndpoint::new(&base_url, "chat-model", "", 5),
            temperature: 0.2,
        };

        let error = model
            .chat(ChatRequest::new(vec![ChatMessage::user("question")]))
            .await
            .expect_err("invalid json response fails");
        let request = handle.join().expect("server thread joins");

        assert_eq!(request.path, "/chat/completions");
        let ProviderError::ResponseDecode { diagnostic, .. } = error else {
            panic!("expected response decode error");
        };
        assert_eq!(diagnostic.client_kind, "chat");
        assert_eq!(diagnostic.phase, "chat");
        assert_eq!(diagnostic.status_code, Some(200));
        assert_eq!(
            diagnostic.response_content_type.as_deref(),
            Some("text/html")
        );
        assert_eq!(diagnostic.response_body_bytes, Some(body.len() as u64));
        assert_eq!(
            diagnostic.upstream_request_id.as_deref(),
            Some("req-fixture-1")
        );
        let prefix = diagnostic
            .response_body_prefix
            .as_deref()
            .expect("body prefix recorded");
        assert!(prefix.contains("Authorization: <redacted>"));
        assert_diagnostic_has_no_fixture_secrets(&diagnostic);
    }

    #[tokio::test]
    async fn chat_empty_json_error_records_empty_body_metadata() {
        let (base_url, handle) = spawn_response_server("200 OK", "application/json", "");
        let model = OpenAiCompatibleChatModel {
            endpoint: OpenAiEndpoint::new(&base_url, "chat-model", "", 5),
            temperature: 0.2,
        };

        let error = model
            .chat(ChatRequest::new(vec![ChatMessage::user("question")]))
            .await
            .expect_err("empty json response fails");
        let _request = handle.join().expect("server thread joins");

        let ProviderError::ResponseDecode { diagnostic, .. } = error else {
            panic!("expected response decode error");
        };
        assert_eq!(diagnostic.status_code, Some(200));
        assert_eq!(diagnostic.response_body_bytes, Some(0));
        assert_eq!(diagnostic.response_body_prefix.as_deref(), Some(""));
        assert_eq!(diagnostic.transport_error_kind.as_deref(), Some("eof"));
        assert!(diagnostic.response_body_available);
    }

    #[tokio::test]
    async fn embedding_invalid_json_error_includes_embedding_diagnostic() {
        let (base_url, handle) = spawn_json_server("200 OK", "not-json");
        let model = OpenAiCompatibleEmbeddingModel {
            endpoint: OpenAiEndpoint::new(&base_url, "embedding-model", "", 5),
            provider_kind: "openai_compatible".into(),
            dimension: 3,
            normalize: false,
            batch_size: 16,
            capability_cache_ttl: Duration::from_secs(60),
            query_instruction: String::new(),
            document_instruction: String::new(),
        };

        let error = model
            .embed(vec!["document".into()], EmbeddingPurpose::Document)
            .await
            .expect_err("invalid embedding json fails");
        let request = handle.join().expect("server thread joins");

        assert_eq!(request.path, "/embeddings");
        let ProviderError::ResponseDecode { diagnostic, .. } = error else {
            panic!("expected response decode error");
        };
        assert_eq!(diagnostic.client_kind, "embedding");
        assert_eq!(diagnostic.phase, "embedding");
        assert_eq!(diagnostic.model.as_deref(), Some("embedding-model"));
        assert_eq!(diagnostic.status_code, Some(200));
        assert_eq!(diagnostic.response_body_prefix.as_deref(), Some("not-json"));
    }

    #[tokio::test]
    async fn rerank_invalid_json_error_includes_rerank_diagnostic() {
        let (base_url, request_rx, handle) = spawn_response_sequence_server(vec![
            (
                "200 OK",
                "application/json",
                r#"{"data":[{"id":"rerank-model","max_model_len":8192}]}"#,
            ),
            ("200 OK", "application/json", "not-json"),
        ]);
        let config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            base_url,
            model: "rerank-model".into(),
            top_n: 1,
            timeout_seconds: 5,
            capability_cache_ttl_seconds: 60,
            ..Default::default()
        };
        let reranker = OpenAiCompatibleReranker::from_config(&config);

        let error = ProviderReranker::rerank(&reranker, "query", vec![RerankDoc::new("doc")], 1)
            .await
            .expect_err("invalid rerank json fails");
        let _discovery_request =
            recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
                .await
                .expect("discovery request recorded");
        let request = recv_recorded_request(Arc::clone(&request_rx), Duration::from_secs(1))
            .await
            .expect("rerank request recorded");
        handle.join().expect("server thread joins");

        assert_eq!(request.path, "/v1/rerank");
        let ProviderError::ResponseDecode { diagnostic, .. } = error else {
            panic!("expected response decode error");
        };
        assert_eq!(diagnostic.client_kind, "rerank");
        assert_eq!(diagnostic.phase, "rerank");
        assert_eq!(diagnostic.model.as_deref(), Some("rerank-model"));
        assert_eq!(diagnostic.endpoint_path, "/v1/rerank");
        assert_eq!(diagnostic.status_code, Some(200));
    }

    #[tokio::test]
    async fn parses_streaming_chat_chunks() {
        let chunks = stream::iter(vec![
            Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n".to_vec()),
            Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n".to_vec()),
            Ok(b"data: [DONE]\n\n".to_vec()),
        ])
        .boxed();

        let events = sse_chat_stream(chunks, test_stream_permit().await)
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

        let events = sse_chat_stream(chunks, test_stream_permit().await)
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

        let json =
            r#"{"error":{"message":"OPENAI_API_KEY=providerfixture12345 token=fixture12345"}}"#;
        let message = openai_error_message(json).expect("message parsed");
        assert!(!message.contains("providerfixture12345"));
        assert!(!message.contains("fixture12345"));
        assert!(message.contains("<redacted>"));
    }

    #[test]
    fn text_message_content_serializes_as_string() {
        let value = serde_json::to_value(ChatMessageContent::Text("hello".into()))
            .expect("serialize content");
        assert_eq!(value, serde_json::Value::String("hello".into()));
    }

    fn assert_diagnostic_has_no_fixture_secrets(
        diagnostic: &crate::upstream::UpstreamFailureDiagnostic,
    ) {
        let encoded = serde_json::to_string(diagnostic).expect("serialize diagnostic");
        assert!(!encoded.contains("fixturebearertoken"));
        assert!(!encoded.contains("fixture12345"));
        assert!(!encoded.contains("providerfixture12345"));
        assert!(!encoded.contains("user:pass"));
        assert!(encoded.contains("<redacted>"));
    }
}
