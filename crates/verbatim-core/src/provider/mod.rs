//! Model provider contracts for local OpenAI-compatible endpoints.
//!
//! The rest of Verbatim should depend on these traits instead of a vendor SDK.

use std::fmt;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::upstream::UpstreamFailureDiagnostic;

pub mod openai_compatible;

mod endpoint_capability;

/// Returns whether an endpoint URL names a loopback address or `localhost`.
pub(crate) fn endpoint_is_local(base_url: &str) -> bool {
    let Ok(endpoint) = url::Url::parse(base_url) else {
        return false;
    };

    match endpoint.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Builds an endpoint URL, rejecting non-local bases when local-only transport is required.
pub(crate) fn endpoint_url(
    base_url: &str,
    path: &str,
    local_only: bool,
    operation: &'static str,
) -> ProviderResult<String> {
    if base_url.is_empty() {
        return Err(ProviderError::configuration(operation, "base_url is empty"));
    }
    if local_only && !endpoint_is_local(base_url) {
        return Err(ProviderError::configuration(
            operation,
            "LocalOnly transport requires a loopback or localhost base_url",
        ));
    }

    Ok(format!("{base_url}/{path}"))
}

/// Result type used by model providers.
pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

/// Async stream of chat completion deltas.
pub type ChatStream = BoxStream<'static, ProviderResult<ChatStreamEvent>>;

/// Structured provider failure with request bodies intentionally omitted.
#[derive(Debug)]
pub enum ProviderError {
    /// The provider configuration is incomplete or invalid.
    Configuration {
        operation: &'static str,
        message: String,
    },
    /// The HTTP request failed before a provider response was available.
    Transport {
        operation: &'static str,
        source: reqwest::Error,
        diagnostic: Box<UpstreamFailureDiagnostic>,
    },
    /// The provider returned a non-success HTTP status.
    HttpStatus {
        operation: &'static str,
        status: reqwest::StatusCode,
        message: String,
        diagnostic: Box<UpstreamFailureDiagnostic>,
    },
    /// The provider response could not be decoded as the expected JSON shape.
    ResponseDecode {
        operation: &'static str,
        source: serde_json::Error,
        diagnostic: Box<UpstreamFailureDiagnostic>,
    },
    /// The application endpoint limiter did not admit the request in time.
    QueueTimeout {
        operation: &'static str,
        timeout_seconds: u64,
    },
    /// The application endpoint limiter rejected the request because its wait queue is full.
    QueueFull { operation: &'static str },
    /// A streaming event contained invalid JSON.
    StreamDecode {
        operation: &'static str,
        source: serde_json::Error,
    },
    /// The response JSON decoded but missed required semantic fields.
    MalformedResponse {
        operation: &'static str,
        message: String,
    },
}

impl ProviderError {
    pub(crate) fn configuration(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Configuration {
            operation,
            message: message.into(),
        }
    }

    pub(crate) fn malformed(operation: &'static str, message: impl Into<String>) -> Self {
        Self::MalformedResponse {
            operation,
            message: message.into(),
        }
    }

    pub fn diagnostic(&self) -> Option<&UpstreamFailureDiagnostic> {
        match self {
            Self::Transport { diagnostic, .. }
            | Self::HttpStatus { diagnostic, .. }
            | Self::ResponseDecode { diagnostic, .. } => Some(diagnostic.as_ref()),
            Self::Configuration { .. }
            | Self::QueueTimeout { .. }
            | Self::QueueFull { .. }
            | Self::StreamDecode { .. }
            | Self::MalformedResponse { .. } => None,
        }
    }

    pub fn is_retryable_provider_failure(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::HttpStatus { status, .. } => {
                matches!(
                    *status,
                    reqwest::StatusCode::REQUEST_TIMEOUT
                        | reqwest::StatusCode::TOO_MANY_REQUESTS
                        | reqwest::StatusCode::INTERNAL_SERVER_ERROR
                        | reqwest::StatusCode::BAD_GATEWAY
                        | reqwest::StatusCode::SERVICE_UNAVAILABLE
                        | reqwest::StatusCode::GATEWAY_TIMEOUT
                ) || status.is_server_error()
            }
            Self::Configuration { .. }
            | Self::ResponseDecode { .. }
            | Self::QueueTimeout { .. }
            | Self::QueueFull { .. }
            | Self::StreamDecode { .. }
            | Self::MalformedResponse { .. } => false,
        }
    }

    pub fn with_retry_count(mut self, retry_count: u32) -> Self {
        match &mut self {
            Self::Transport { diagnostic, .. }
            | Self::HttpStatus { diagnostic, .. }
            | Self::ResponseDecode { diagnostic, .. } => {
                diagnostic.retry_count = Some(retry_count);
            }
            Self::Configuration { .. }
            | Self::QueueTimeout { .. }
            | Self::QueueFull { .. }
            | Self::StreamDecode { .. }
            | Self::MalformedResponse { .. } => {}
        }
        self
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration { operation, message } => {
                write!(
                    f,
                    "{operation} provider configuration is invalid: {message}"
                )
            }
            Self::Transport {
                operation,
                source,
                diagnostic,
            } => {
                write!(
                    f,
                    "{operation} request failed: {source}; upstream diagnostic: {}",
                    diagnostic.summary()
                )
            }
            Self::HttpStatus {
                operation,
                status,
                message,
                diagnostic,
            } => write!(
                f,
                "{operation} provider returned HTTP {status}: {message}; upstream diagnostic: {}",
                diagnostic.summary()
            ),
            Self::ResponseDecode {
                operation,
                source,
                diagnostic,
            } => {
                write!(
                    f,
                    "failed to decode {operation} response: {source}; upstream diagnostic: {}",
                    diagnostic.summary()
                )
            }
            Self::QueueTimeout {
                operation,
                timeout_seconds,
            } => {
                write!(
                    f,
                    "{operation} request timed out after waiting {timeout_seconds}s for model endpoint capacity"
                )
            }
            Self::QueueFull { operation } => {
                write!(f, "{operation} model endpoint queue is full")
            }
            Self::StreamDecode { operation, source } => {
                write!(f, "failed to parse {operation} stream event: {source}")
            }
            Self::MalformedResponse { operation, message } => {
                write!(
                    f,
                    "{operation} provider returned a malformed response: {message}"
                )
            }
        }
    }
}

impl std::error::Error for ProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport { source, .. } => Some(source),
            Self::ResponseDecode { source, .. } => Some(source),
            Self::StreamDecode { source, .. } => Some(source),
            Self::Configuration { .. }
            | Self::HttpStatus { .. }
            | Self::QueueTimeout { .. }
            | Self::QueueFull { .. }
            | Self::MalformedResponse { .. } => None,
        }
    }
}

/// Chat/generation model boundary.
#[async_trait]
pub trait ChatModel: Send + Sync {
    /// Complete a chat request and return the final assistant message.
    async fn chat(&self, req: ChatRequest) -> ProviderResult<ChatResponse>;

    /// Stream chat completion deltas for daemon SSE or other incremental sinks.
    async fn stream_chat(&self, req: ChatRequest) -> ProviderResult<ChatStream>;
}

/// Vision-capable chat model boundary.
#[async_trait]
pub trait VisionModel: Send + Sync {
    /// Describe one image with an optional user prompt.
    async fn describe_image(&self, req: ImageDescribeRequest) -> ProviderResult<ImageDescription>;
}

/// Embedding model boundary with explicit query/document purpose.
#[async_trait]
pub trait EmbeddingModel: Send + Sync {
    /// Embed texts for the requested retrieval purpose.
    async fn embed(
        &self,
        texts: Vec<String>,
        purpose: EmbeddingPurpose,
    ) -> ProviderResult<Vec<Vec<f32>>>;
}

/// Reranker boundary for local OpenAI-compatible rerank endpoints.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Return ranked hits that refer to the input document indices.
    async fn rerank(
        &self,
        query: &str,
        docs: Vec<RerankDoc>,
        top_n: usize,
    ) -> ProviderResult<Vec<RerankHit>>;
}

/// Chat completion request independent of any provider SDK.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            temperature: None,
            max_tokens: None,
        }
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// One message in a chat request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatMessageContent,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: ChatMessageContent::Text(content.into()),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: ChatMessageContent::Text(content.into()),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: ChatMessageContent::Text(content.into()),
        }
    }

    pub fn user_parts(parts: Vec<ChatContentPart>) -> Self {
        Self {
            role: ChatRole::User,
            content: ChatMessageContent::Parts(parts),
        }
    }
}

/// Provider-neutral chat roles serialized in the OpenAI-compatible shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// Chat message content, including multimodal OpenAI-compatible parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatMessageContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

/// One text or image part in a multimodal chat message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

/// OpenAI-compatible image URL payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Final non-streaming chat completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
}

/// Incremental chat completion event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatStreamEvent {
    pub delta: String,
    pub finish_reason: Option<String>,
}

/// Token usage returned by OpenAI-compatible APIs when available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

/// Image description request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageDescribeRequest {
    pub image: ImageInput,
    pub prompt: String,
    pub detail: Option<String>,
    pub max_tokens: Option<u32>,
}

impl ImageDescribeRequest {
    pub fn new(image: ImageInput, prompt: impl Into<String>) -> Self {
        Self {
            image,
            prompt: prompt.into(),
            detail: None,
            max_tokens: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// Supported local image reference forms for OpenAI-compatible vision chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageInput {
    DataUri(String),
    File(PathBuf),
    Url(String),
}

impl ImageInput {
    pub fn data_uri(uri: impl Into<String>) -> Self {
        Self::DataUri(uri.into())
    }

    pub fn file(path: impl AsRef<Path>) -> Self {
        Self::File(path.as_ref().to_path_buf())
    }

    pub fn url(url: impl Into<String>) -> Self {
        Self::Url(url.into())
    }

    pub fn to_openai_url(&self) -> String {
        match self {
            Self::DataUri(uri) | Self::Url(uri) => uri.clone(),
            Self::File(path) => format!("file://{}", path.display()),
        }
    }
}

/// Image description result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDescription {
    pub text: String,
}

/// Embedding use case, used to apply query/document instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPurpose {
    Query,
    Document,
}

/// A document candidate passed to the reranker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerankDoc {
    pub text: String,
}

impl RerankDoc {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// A reranker hit referring to an input document index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankHit {
    pub index: usize,
    pub score: f32,
}
