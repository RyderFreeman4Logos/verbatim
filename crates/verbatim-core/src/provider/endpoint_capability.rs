//! Endpoint-advertised model limits for OpenAI-compatible providers.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use reqwest::{StatusCode, Url};

use super::ProviderError;

const MODELS_PATH: &str = "models";
const MODELS_V1_PATH: &str = "v1/models";
const SERVED_MODEL_FIELDS: &[&str] = &[
    "served_model",
    "served_model_name",
    "served_model_id",
    "base_model",
    "base_model_name",
    "root",
];
const DTYPE_FIELDS: &[&str] = &["dtype", "torch_dtype", "model_dtype", "weight_dtype"];
const QUANTIZATION_FIELDS: &[&str] = &[
    "quantization",
    "quantization_method",
    "quant_method",
    "load_format",
];
const WEIGHT_IDENTITY_FIELDS: &[&str] = &[
    "revision",
    "commit",
    "commit_hash",
    "sha",
    "model_sha",
    "digest",
    "weight_hash",
    "checkpoint",
];

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) enum EndpointCapabilityRole {
    Embedding,
    Rerank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EndpointCapabilityState {
    Cached,
    Refreshed,
    Unavailable,
    RefreshFailed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct EndpointCapability {
    pub max_context_tokens: Option<usize>,
    pub served_model: Option<String>,
    pub dtype: Option<String>,
    pub quantization: Option<String>,
    pub weight_identity: Option<String>,
    pub request_limits: EndpointRequestLimits,
}

impl EndpointCapability {
    fn is_empty(&self) -> bool {
        self.max_context_tokens.is_none()
            && self.served_model.is_none()
            && self.dtype.is_none()
            && self.quantization.is_none()
            && self.weight_identity.is_none()
            && self.request_limits.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct EndpointRequestLimits {
    pub max_batch_size: Option<usize>,
    pub max_inputs: Option<usize>,
    pub max_input_chars: Option<usize>,
    pub max_payload_chars: Option<usize>,
    pub max_candidates: Option<usize>,
    pub max_documents: Option<usize>,
    pub max_document_chars: Option<usize>,
}

impl EndpointRequestLimits {
    fn is_empty(&self) -> bool {
        self.max_batch_size.is_none()
            && self.max_inputs.is_none()
            && self.max_input_chars.is_none()
            && self.max_payload_chars.is_none()
            && self.max_candidates.is_none()
            && self.max_documents.is_none()
            && self.max_document_chars.is_none()
    }

    pub(super) fn embedding_batch_size(&self) -> Option<usize> {
        min_present([self.max_batch_size, self.max_inputs])
    }

    pub(super) fn rerank_candidate_count(&self) -> Option<usize> {
        min_present([self.max_candidates, self.max_documents])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EndpointCapabilityDiagnostics {
    pub state: EndpointCapabilityState,
    pub max_context_tokens: Option<usize>,
    pub served_model: Option<String>,
    pub dtype: Option<String>,
    pub quantization: Option<String>,
    pub weight_identity: Option<String>,
    pub max_batch_size: Option<usize>,
    pub max_inputs: Option<usize>,
    pub max_input_chars: Option<usize>,
    pub max_payload_chars: Option<usize>,
    pub max_candidates: Option<usize>,
    pub max_documents: Option<usize>,
    pub max_document_chars: Option<usize>,
    pub reason: Option<String>,
}

impl EndpointCapabilityDiagnostics {
    fn new(
        state: EndpointCapabilityState,
        value: Option<&EndpointCapability>,
        reason: Option<String>,
    ) -> Self {
        let limits = value.map(|capability| &capability.request_limits);
        Self {
            state,
            max_context_tokens: value.and_then(|capability| capability.max_context_tokens),
            served_model: value.and_then(|capability| capability.served_model.clone()),
            dtype: value.and_then(|capability| capability.dtype.clone()),
            quantization: value.and_then(|capability| capability.quantization.clone()),
            weight_identity: value.and_then(|capability| capability.weight_identity.clone()),
            max_batch_size: limits.and_then(|limits| limits.max_batch_size),
            max_inputs: limits.and_then(|limits| limits.max_inputs),
            max_input_chars: limits.and_then(|limits| limits.max_input_chars),
            max_payload_chars: limits.and_then(|limits| limits.max_payload_chars),
            max_candidates: limits.and_then(|limits| limits.max_candidates),
            max_documents: limits.and_then(|limits| limits.max_documents),
            max_document_chars: limits.and_then(|limits| limits.max_document_chars),
            reason,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EndpointCapabilityLookup {
    pub value: Option<EndpointCapability>,
    pub diagnostics: EndpointCapabilityDiagnostics,
}

impl EndpointCapabilityLookup {
    pub(super) fn cached(value: Option<EndpointCapability>, reason: Option<String>) -> Self {
        let state = if value.is_some() {
            EndpointCapabilityState::Cached
        } else {
            EndpointCapabilityState::Unavailable
        };
        Self {
            diagnostics: EndpointCapabilityDiagnostics::new(state, value.as_ref(), reason),
            value,
        }
    }

    pub(super) fn refreshed(value: EndpointCapability) -> Self {
        Self {
            diagnostics: EndpointCapabilityDiagnostics::new(
                EndpointCapabilityState::Refreshed,
                Some(&value),
                None,
            ),
            value: Some(value),
        }
    }

    pub(super) fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            value: None,
            diagnostics: EndpointCapabilityDiagnostics::new(
                EndpointCapabilityState::Unavailable,
                None,
                Some(reason.into()),
            ),
        }
    }

    pub(super) fn refresh_failed(reason: impl Into<String>) -> Self {
        Self {
            value: None,
            diagnostics: EndpointCapabilityDiagnostics::new(
                EndpointCapabilityState::RefreshFailed,
                None,
                Some(reason.into()),
            ),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct EndpointCapabilityCacheKey {
    base_url: String,
    role: EndpointCapabilityRole,
    provider_kind: String,
    model: String,
}

impl EndpointCapabilityCacheKey {
    pub(super) fn new(
        base_url: &str,
        role: EndpointCapabilityRole,
        provider_kind: &str,
        model: &str,
    ) -> Self {
        Self {
            base_url: normalized_endpoint_key(base_url),
            role,
            provider_kind: provider_kind.trim().to_ascii_lowercase(),
            model: model.to_string(),
        }
    }

    #[cfg(test)]
    fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[derive(Clone, Debug)]
struct EndpointCapabilityCacheEntry {
    lookup: EndpointCapabilityLookup,
    refreshed_at: Instant,
}

#[derive(Debug)]
pub(super) struct EndpointCapabilityCache {
    entries: Mutex<HashMap<EndpointCapabilityCacheKey, EndpointCapabilityCacheEntry>>,
}

impl EndpointCapabilityCache {
    pub(super) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn get_fresh(
        &self,
        key: &EndpointCapabilityCacheKey,
        ttl: Duration,
    ) -> Option<EndpointCapabilityLookup> {
        let entries = lock_unpoisoned(&self.entries);
        let entry = entries.get(key)?;
        if entry.refreshed_at.elapsed() > ttl {
            return None;
        }
        Some(EndpointCapabilityLookup::cached(
            entry.lookup.value.clone(),
            entry.lookup.diagnostics.reason.clone(),
        ))
    }

    pub(super) fn insert(&self, key: EndpointCapabilityCacheKey, lookup: EndpointCapabilityLookup) {
        let mut entries = lock_unpoisoned(&self.entries);
        entries.insert(
            key,
            EndpointCapabilityCacheEntry {
                lookup,
                refreshed_at: Instant::now(),
            },
        );
    }

    #[cfg(test)]
    pub(super) fn age_entry(&self, key: &EndpointCapabilityCacheKey, age: Duration) {
        let mut entries = lock_unpoisoned(&self.entries);
        entries
            .get_mut(key)
            .expect("cache entry exists")
            .refreshed_at = Instant::now() - age;
    }
}

pub(super) fn endpoint_capability_cache() -> &'static EndpointCapabilityCache {
    static CACHE: OnceLock<EndpointCapabilityCache> = OnceLock::new();
    CACHE.get_or_init(EndpointCapabilityCache::new)
}

pub(super) fn model_discovery_paths(base_url: &str) -> Vec<&'static str> {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") || trimmed.ends_with("/v2") {
        vec![MODELS_PATH]
    } else {
        vec![MODELS_V1_PATH, MODELS_PATH]
    }
}

pub(super) fn parse_endpoint_capability(
    value: &serde_json::Value,
    model: &str,
) -> Option<EndpointCapability> {
    let search_root = capability_search_root(value, model)?;
    let capability = EndpointCapability {
        max_context_tokens: find_context_limit(search_root),
        served_model: find_metadata_string(search_root, SERVED_MODEL_FIELDS),
        dtype: find_metadata_string(search_root, DTYPE_FIELDS),
        quantization: find_metadata_string(search_root, QUANTIZATION_FIELDS),
        weight_identity: find_metadata_string(search_root, WEIGHT_IDENTITY_FIELDS),
        request_limits: find_request_limits(search_root),
    };
    (!capability.is_empty()).then_some(capability)
}

pub(super) fn is_context_or_payload_limit_error(error: &ProviderError) -> bool {
    let ProviderError::HttpStatus {
        status,
        message,
        diagnostic,
        ..
    } = error
    else {
        return false;
    };
    if !matches!(
        *status,
        StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }

    let mut haystack = message.to_ascii_lowercase();
    if let Some(reason) = status.canonical_reason() {
        haystack.push(' ');
        haystack.push_str(&reason.to_ascii_lowercase());
    }
    if let Some(prefix) = &diagnostic.response_body_prefix {
        haystack.push(' ');
        haystack.push_str(&prefix.to_ascii_lowercase());
    }
    is_context_or_payload_limit_message(&haystack)
}

pub(super) fn is_discovery_unsupported(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::HttpStatus { status, .. }
            if matches!(
                *status,
                StatusCode::NOT_FOUND
                    | StatusCode::METHOD_NOT_ALLOWED
                    | StatusCode::NOT_IMPLEMENTED
            )
    )
}

pub(super) fn capability_failure_reason(error: &ProviderError) -> String {
    match error {
        ProviderError::Configuration { .. } => "invalid_configuration".to_string(),
        ProviderError::Transport { source, .. } if source.is_timeout() => {
            "discovery_timeout".to_string()
        }
        ProviderError::Transport { .. } => "discovery_request_failed".to_string(),
        ProviderError::HttpStatus { status, .. } => {
            format!("discovery_http_status_{}", status.as_u16())
        }
        ProviderError::ResponseDecode { .. } => "discovery_invalid_json".to_string(),
        ProviderError::QueueTimeout { .. } => "discovery_queue_timeout".to_string(),
        ProviderError::QueueFull { .. } => "discovery_queue_full".to_string(),
        ProviderError::StreamDecode { .. } | ProviderError::MalformedResponse { .. } => {
            "discovery_invalid_response".to_string()
        }
    }
}

pub(super) fn normalized_endpoint_key(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let Ok(mut url) = Url::parse(trimmed) else {
        return trimmed.to_ascii_lowercase();
    };
    url.set_query(None);
    url.set_fragment(None);
    let scheme = url.scheme().to_ascii_lowercase();
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = url.path().trim_end_matches('/');
    format!("{scheme}://{host}{port}{path}")
}

fn capability_search_root<'a>(
    value: &'a serde_json::Value,
    model: &str,
) -> Option<&'a serde_json::Value> {
    if let Some(data) = value.get("data").and_then(serde_json::Value::as_array) {
        if let Some(matched) = data
            .iter()
            .find(|candidate| model_entry_matches(candidate, model))
        {
            return Some(matched);
        }
        if data.len() == 1 {
            return data.first();
        }
        return None;
    }
    Some(value)
}

fn model_entry_matches(value: &serde_json::Value, model: &str) -> bool {
    ["id", "model", "name"]
        .iter()
        .filter_map(|field| value.get(*field).and_then(serde_json::Value::as_str))
        .any(|candidate| candidate == model)
}

fn find_context_limit(value: &serde_json::Value) -> Option<usize> {
    let mut limits = Vec::new();
    collect_context_limits(value, &mut limits);
    limits.into_iter().filter(|limit| *limit > 0).min()
}

fn find_metadata_string(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    let mut values = Vec::new();
    collect_metadata_strings(value, fields, &mut values);
    values.into_iter().next()
}

fn collect_metadata_strings(value: &serde_json::Value, fields: &[&str], values: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if fields.iter().any(|field| key.eq_ignore_ascii_case(field)) {
                    if let Some(value) = metadata_value_as_string(value) {
                        values.push(value);
                    }
                }
                collect_metadata_strings(value, fields, values);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_metadata_strings(item, fields, values);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

fn metadata_value_as_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => sanitized_metadata_value(value),
        serde_json::Value::Number(value) => sanitized_metadata_value(&value.to_string()),
        serde_json::Value::Bool(value) => {
            sanitized_metadata_value(if *value { "true" } else { "false" })
        }
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

fn sanitized_metadata_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(256).collect())
}

fn collect_context_limits(value: &serde_json::Value, limits: &mut Vec<usize>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if is_context_limit_field(key) {
                    if let Some(limit) = value_as_positive_usize(value) {
                        limits.push(limit);
                    }
                }
                collect_context_limits(value, limits);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_context_limits(value, limits);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

fn is_context_limit_field(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "max_model_len"
            | "max_context_length"
            | "context_length"
            | "max_sequence_length"
            | "max_seq_len"
            | "max_position_embeddings"
            | "model_max_length"
            | "n_ctx"
    )
}

fn find_request_limits(value: &serde_json::Value) -> EndpointRequestLimits {
    let mut limits = RequestLimitAccumulator::default();
    collect_request_limits(value, &mut limits);
    limits.into_limits()
}

fn collect_request_limits(value: &serde_json::Value, limits: &mut RequestLimitAccumulator) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if let Some(field) = request_limit_field(key) {
                    if let Some(limit) = value_as_positive_usize(value) {
                        limits.push(field, limit);
                    }
                }
                collect_request_limits(value, limits);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_request_limits(value, limits);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

fn request_limit_field(key: &str) -> Option<RequestLimitField> {
    match key.to_ascii_lowercase().as_str() {
        "max_batch_size" => Some(RequestLimitField::BatchSize),
        "max_inputs" | "max_input_count" => Some(RequestLimitField::Inputs),
        "max_input_chars" | "max_input_characters" => Some(RequestLimitField::InputChars),
        "max_payload_chars" | "max_payload_characters" => Some(RequestLimitField::PayloadChars),
        "max_candidates" | "max_candidate_count" => Some(RequestLimitField::Candidates),
        "max_documents" | "max_document_count" => Some(RequestLimitField::Documents),
        "max_document_chars" | "max_document_characters" => Some(RequestLimitField::DocumentChars),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
enum RequestLimitField {
    BatchSize,
    Inputs,
    InputChars,
    PayloadChars,
    Candidates,
    Documents,
    DocumentChars,
}

#[derive(Default)]
struct RequestLimitAccumulator {
    max_batch_size: Vec<usize>,
    max_inputs: Vec<usize>,
    max_input_chars: Vec<usize>,
    max_payload_chars: Vec<usize>,
    max_candidates: Vec<usize>,
    max_documents: Vec<usize>,
    max_document_chars: Vec<usize>,
}

impl RequestLimitAccumulator {
    fn push(&mut self, field: RequestLimitField, value: usize) {
        match field {
            RequestLimitField::BatchSize => self.max_batch_size.push(value),
            RequestLimitField::Inputs => self.max_inputs.push(value),
            RequestLimitField::InputChars => self.max_input_chars.push(value),
            RequestLimitField::PayloadChars => self.max_payload_chars.push(value),
            RequestLimitField::Candidates => self.max_candidates.push(value),
            RequestLimitField::Documents => self.max_documents.push(value),
            RequestLimitField::DocumentChars => self.max_document_chars.push(value),
        }
    }

    fn into_limits(self) -> EndpointRequestLimits {
        EndpointRequestLimits {
            max_batch_size: min_vec(self.max_batch_size),
            max_inputs: min_vec(self.max_inputs),
            max_input_chars: min_vec(self.max_input_chars),
            max_payload_chars: min_vec(self.max_payload_chars),
            max_candidates: min_vec(self.max_candidates),
            max_documents: min_vec(self.max_documents),
            max_document_chars: min_vec(self.max_document_chars),
        }
    }
}

fn value_as_positive_usize(value: &serde_json::Value) -> Option<usize> {
    if let Some(value) = value.as_u64() {
        return usize::try_from(value).ok().filter(|value| *value > 0);
    }
    if let Some(value) = value.as_str() {
        return value
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0);
    }
    None
}

fn is_context_or_payload_limit_message(message: &str) -> bool {
    [
        "context length",
        "context window",
        "max model len",
        "maximum context",
        "token limit",
        "prompt too long",
        "input too long",
        "payload too large",
        "request too large",
        "maximum input",
        "maximum sequence",
        "batch size",
        "too many inputs",
        "max_model_len",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

fn min_present<const N: usize>(values: [Option<usize>; N]) -> Option<usize> {
    values.into_iter().flatten().min()
}

fn min_vec(values: Vec<usize>) -> Option<usize> {
    values.into_iter().filter(|value| *value > 0).min()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_context_fields_from_common_metadata_shapes() {
        for field in [
            "max_model_len",
            "max_context_length",
            "context_length",
            "max_sequence_length",
            "max_seq_len",
            "max_position_embeddings",
            "model_max_length",
            "n_ctx",
        ] {
            let response = serde_json::json!({
                "data": [
                    {
                        "id": "target-model",
                        "model_extra": {
                            field: "4096"
                        }
                    }
                ]
            });

            let capability =
                parse_endpoint_capability(&response, "target-model").expect("capability parsed");

            assert_eq!(capability.max_context_tokens, Some(4096), "{field}");
        }
    }

    #[test]
    fn missing_or_invalid_capability_fields_return_unavailable() {
        for value in [
            serde_json::json!({"data": [{"id": "target-model"}]}),
            serde_json::json!({"data": [{"id": "target-model", "max_model_len": 0}]}),
            serde_json::json!({"data": [{"id": "target-model", "max_model_len": "invalid"}]}),
            serde_json::json!({"data": [{"id": "other-model", "max_model_len": 4096}, {"id": "second-model", "max_model_len": 2048}]}),
        ] {
            assert!(
                parse_endpoint_capability(&value, "target-model").is_none(),
                "{value}"
            );
        }
    }

    #[test]
    fn parses_safe_request_shaping_hints() {
        let response = serde_json::json!({
            "data": [{
                "id": "target-model",
                "limits": {
                    "max_batch_size": 8,
                    "max_inputs": "4",
                    "max_input_chars": 2000,
                    "max_payload_chars": 10000,
                    "max_candidates": 12,
                    "max_documents": 10,
                    "max_document_chars": 1500
                }
            }]
        });

        let capability =
            parse_endpoint_capability(&response, "target-model").expect("capability parsed");

        assert_eq!(capability.request_limits.embedding_batch_size(), Some(4));
        assert_eq!(capability.request_limits.rerank_candidate_count(), Some(10));
        assert_eq!(capability.request_limits.max_input_chars, Some(2000));
        assert_eq!(capability.request_limits.max_payload_chars, Some(10000));
        assert_eq!(capability.request_limits.max_document_chars, Some(1500));
    }

    #[test]
    fn cache_key_strips_query_tokens_and_distinguishes_role_provider_model() {
        let rerank_key = EndpointCapabilityCacheKey::new(
            "HTTP://LOCALHOST:8003/v1?token=fixture-secret#frag",
            EndpointCapabilityRole::Rerank,
            "VLLM",
            "model-a",
        );
        let embedding_key = EndpointCapabilityCacheKey::new(
            "http://localhost:8003/v1?token=other",
            EndpointCapabilityRole::Embedding,
            "openai_compatible",
            "model-a",
        );
        let other_model_key = EndpointCapabilityCacheKey::new(
            "http://localhost:8003/v1",
            EndpointCapabilityRole::Rerank,
            "vllm",
            "model-b",
        );

        assert_eq!(rerank_key.base_url(), "http://localhost:8003/v1");
        assert_ne!(rerank_key, embedding_key);
        assert_ne!(rerank_key, other_model_key);
    }

    #[test]
    fn cache_respects_ttl_and_forced_refresh_path() {
        let cache = EndpointCapabilityCache::new();
        let key = EndpointCapabilityCacheKey::new(
            "http://localhost:8003/v1",
            EndpointCapabilityRole::Rerank,
            "vllm",
            "cache-model",
        );
        cache.insert(
            key.clone(),
            EndpointCapabilityLookup::refreshed(EndpointCapability {
                max_context_tokens: Some(4096),
                served_model: None,
                dtype: None,
                quantization: None,
                weight_identity: None,
                request_limits: EndpointRequestLimits::default(),
            }),
        );

        let fresh = cache
            .get_fresh(&key, Duration::from_secs(60))
            .expect("fresh cache entry");
        assert_eq!(fresh.diagnostics.state, EndpointCapabilityState::Cached);
        assert_eq!(fresh.diagnostics.max_context_tokens, Some(4096));

        cache.age_entry(&key, Duration::from_secs(120));
        assert!(cache.get_fresh(&key, Duration::from_secs(60)).is_none());
    }

    #[test]
    fn model_discovery_paths_support_v1_and_bare_host_base_urls() {
        assert_eq!(
            model_discovery_paths("http://127.0.0.1:8003/v1"),
            vec!["models"]
        );
        assert_eq!(
            model_discovery_paths("http://127.0.0.1:8003"),
            vec!["v1/models", "models"]
        );
    }
}
