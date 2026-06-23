//! Bounded diagnostics for failures from external HTTP APIs.

use std::error::Error;
use std::fmt;
use std::time::Instant;

use reqwest::header::{HeaderMap, CONTENT_TYPE};
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_BODY_PREFIX_MAX_BYTES: usize = 4096;

const REDACTED: &str = "<redacted>";
const SENSITIVE_KEYWORDS: &[&str] = &[
    "authorization",
    "access_token",
    "refresh_token",
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "key",
];
const REQUEST_ID_HEADERS: &[&str] = &[
    "x-request-id",
    "x-request_id",
    "request-id",
    "openai-request-id",
    "x-openai-request-id",
    "x-qdrant-request-id",
    "cf-ray",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamFailureDiagnostic {
    pub phase: String,
    pub client_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_host: Option<String>,
    pub http_method: String,
    pub endpoint_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_content_type: Option<String>,
    pub response_body_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub response_body_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_request_id: Option<String>,
}

impl UpstreamFailureDiagnostic {
    pub fn summary(&self) -> String {
        let mut parts = vec![
            format!("client={}", self.client_kind),
            format!("phase={}", self.phase),
            format!("method={}", self.http_method),
            format!("path={}", self.endpoint_path),
        ];
        if let Some(host) = &self.base_url_host {
            parts.push(format!("host={host}"));
        }
        if let Some(model) = &self.model {
            parts.push(format!("model={model}"));
        }
        if let Some(status) = self.status_code {
            parts.push(format!("status={status}"));
        }
        if let Some(kind) = &self.transport_error_kind {
            parts.push(format!("error_kind={kind}"));
        }
        if let Some(content_type) = &self.response_content_type {
            parts.push(format!("content_type={content_type}"));
        }
        if let Some(bytes) = self.response_body_bytes {
            parts.push(format!("body_bytes={bytes}"));
        }
        if let Some(prefix) = &self.response_body_prefix {
            let preview = bounded_chars(prefix, 160);
            parts.push(format!("body_prefix={preview:?}"));
        } else if !self.response_body_available {
            parts.push("body=unavailable".into());
        }
        if let Some(retry_count) = self.retry_count {
            parts.push(format!("retries={retry_count}"));
        }
        bounded_chars(&parts.join(" "), 768)
    }

    pub fn with_retry_count(mut self, retry_count: u32) -> Self {
        self.retry_count = Some(retry_count);
        self
    }

    fn with_transport_error(mut self, kind: String) -> Self {
        self.transport_error_kind = Some(kind);
        self
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamFailureError {
    message: String,
    diagnostic: UpstreamFailureDiagnostic,
}

impl UpstreamFailureError {
    pub fn new(message: impl Into<String>, diagnostic: UpstreamFailureDiagnostic) -> Self {
        Self {
            message: message.into(),
            diagnostic,
        }
    }

    pub fn diagnostic(&self) -> &UpstreamFailureDiagnostic {
        &self.diagnostic
    }
}

impl fmt::Display for UpstreamFailureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.message, self.diagnostic.summary())
    }
}

impl Error for UpstreamFailureError {}

#[derive(Debug, Clone)]
pub struct UpstreamRequestContext {
    phase: String,
    client_kind: String,
    provider: Option<String>,
    model: Option<String>,
    base_url_host: Option<String>,
    http_method: String,
    endpoint_path: String,
    started_at: Instant,
}

impl UpstreamRequestContext {
    pub fn new(
        phase: impl Into<String>,
        client_kind: impl Into<String>,
        provider: Option<String>,
        model: Option<String>,
        method: &Method,
        url: &str,
    ) -> Self {
        let parsed = Url::parse(url).ok();
        let base_url_host = parsed
            .as_ref()
            .and_then(|url| url.host_str())
            .map(str::to_string);
        let endpoint_path = parsed
            .as_ref()
            .map(sanitized_endpoint_path)
            .unwrap_or_else(|| sanitize_text(url));

        Self {
            phase: phase.into(),
            client_kind: client_kind.into(),
            provider,
            model: model.map(|value| bounded_chars(&sanitize_text(&value), 256)),
            base_url_host,
            http_method: method.as_str().to_string(),
            endpoint_path,
            started_at: Instant::now(),
        }
    }

    pub fn transport_failure(&self, source: &reqwest::Error) -> UpstreamFailureDiagnostic {
        self.base_diagnostic()
            .with_transport_error(classify_reqwest_error(source))
    }

    pub fn status_failure(
        &self,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
        body_truncated: bool,
        body_bytes: Option<u64>,
    ) -> UpstreamFailureDiagnostic {
        self.response_failure(
            Some(status),
            headers,
            body,
            body_truncated,
            body_bytes,
            Some(format!("http_status_{}", status.as_u16())),
        )
    }

    pub fn decode_failure(
        &self,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
        body_truncated: bool,
        body_bytes: Option<u64>,
        source: &serde_json::Error,
    ) -> UpstreamFailureDiagnostic {
        self.response_failure(
            Some(status),
            headers,
            body,
            body_truncated,
            body_bytes,
            Some(classify_json_error(source)),
        )
    }

    pub fn body_read_failure(
        &self,
        status: Option<StatusCode>,
        headers: &HeaderMap,
        source: &reqwest::Error,
    ) -> UpstreamFailureDiagnostic {
        let mut diagnostic = self.response_failure(
            status,
            headers,
            &[],
            false,
            None,
            Some(classify_reqwest_error(source)),
        );
        diagnostic.response_body_available = false;
        diagnostic
    }

    fn base_diagnostic(&self) -> UpstreamFailureDiagnostic {
        UpstreamFailureDiagnostic {
            phase: self.phase.clone(),
            client_kind: self.client_kind.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            base_url_host: self.base_url_host.clone(),
            http_method: self.http_method.clone(),
            endpoint_path: self.endpoint_path.clone(),
            status_code: None,
            response_content_type: None,
            response_body_available: false,
            response_body_prefix: None,
            response_body_truncated: false,
            response_body_bytes: None,
            transport_error_kind: None,
            request_duration_ms: Some(elapsed_ms(self.started_at)),
            retry_count: None,
            upstream_request_id: None,
        }
    }

    fn response_failure(
        &self,
        status: Option<StatusCode>,
        headers: &HeaderMap,
        body: &[u8],
        body_truncated: bool,
        body_bytes: Option<u64>,
        transport_error_kind: Option<String>,
    ) -> UpstreamFailureDiagnostic {
        let mut diagnostic = self.base_diagnostic();
        diagnostic.status_code = status.map(|status| status.as_u16());
        diagnostic.response_content_type = content_type(headers);
        diagnostic.response_body_available = true;
        diagnostic.response_body_prefix =
            Some(sanitized_body_prefix(body, DEFAULT_BODY_PREFIX_MAX_BYTES));
        diagnostic.response_body_truncated = body_truncated;
        diagnostic.response_body_bytes = body_bytes;
        diagnostic.transport_error_kind = transport_error_kind;
        diagnostic.upstream_request_id = request_id(headers);
        diagnostic
    }
}

pub struct CapturedResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub body_truncated: bool,
    pub body_bytes: Option<u64>,
}

impl CapturedResponse {
    pub fn diagnostic_for_status(
        &self,
        context: &UpstreamRequestContext,
    ) -> UpstreamFailureDiagnostic {
        context.status_failure(
            self.status,
            &self.headers,
            &self.body,
            self.body_truncated,
            self.body_bytes,
        )
    }

    pub fn diagnostic_for_decode(
        &self,
        context: &UpstreamRequestContext,
        source: &serde_json::Error,
    ) -> UpstreamFailureDiagnostic {
        context.decode_failure(
            self.status,
            &self.headers,
            &self.body,
            self.body_truncated,
            self.body_bytes,
            source,
        )
    }
}

pub async fn capture_full_response(
    response: reqwest::Response,
) -> Result<CapturedResponse, reqwest::Error> {
    let status = response.status();
    let headers = response.headers().clone();
    let declared_len = response.content_length();
    let body = response.bytes().await?.to_vec();
    let body_bytes = declared_len.or(Some(body.len() as u64));
    Ok(CapturedResponse {
        status,
        headers,
        body,
        body_truncated: false,
        body_bytes,
    })
}

pub async fn capture_response_prefix(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> CapturedResponse {
    let status = response.status();
    let headers = response.headers().clone();
    let declared_len = response.content_length();
    let mut body = Vec::new();
    let mut body_truncated = false;

    while body.len() < max_bytes {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) | Err(_) => break,
        };
        let remaining = max_bytes - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if chunk.len() > remaining {
            body_truncated = true;
            break;
        }
    }

    let body_bytes = declared_len.or_else(|| (!body_truncated).then_some(body.len() as u64));
    CapturedResponse {
        status,
        headers,
        body,
        body_truncated,
        body_bytes,
    }
}

pub fn sanitize_text(input: &str) -> String {
    if let Some(redacted) = sanitize_json_document(input) {
        return redacted;
    }
    sanitize_plain_text(input)
}

fn sanitize_plain_text(input: &str) -> String {
    let header_redacted = redact_authorization_headers(input);
    let urls_redacted = redact_urls(&header_redacted);
    let json_fields_redacted = redact_json_like_fields(&urls_redacted);
    redact_sensitive_assignments(&json_fields_redacted)
}

fn sanitize_json_document(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let mut value = serde_json::from_str::<Value>(input).ok()?;
    redact_json_value(&mut value);
    serde_json::to_string(&value).ok()
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_sensitive_key(key) {
                    *value = Value::String(REDACTED.into());
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_json_value(value);
            }
        }
        Value::String(text) => {
            *text = sanitize_plain_text(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn sanitized_body_prefix(body: &[u8], max_bytes: usize) -> String {
    let end = body.len().min(max_bytes);
    sanitize_text(&String::from_utf8_lossy(&body[..end]))
}

fn content_type(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(sanitize_text)
        .map(|value| bounded_chars(&value, 256))
}

fn request_id(headers: &HeaderMap) -> Option<String> {
    REQUEST_ID_HEADERS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(sanitize_text)
            .map(|value| bounded_chars(&value, 256))
    })
}

fn sanitized_endpoint_path(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{}", url.path(), sanitized_query(query)),
        None => url.path().to_string(),
    }
}

fn sanitized_query(query: &str) -> String {
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| {
            let value = if is_sensitive_key(&key) {
                REDACTED.into()
            } else {
                sanitize_text(&value)
            };
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn redact_authorization_headers(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let Some((name, _value)) = line.split_once(':') else {
                return line.to_string();
            };
            if is_sensitive_key(name.trim()) {
                format!("{}: {REDACTED}", name.trim())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_urls(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(index) = rest.find("://") {
        let (before_scheme, after_scheme_marker) = rest.split_at(index);
        let scheme_start = before_scheme
            .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')))
            .map_or(0, |position| position + 1);
        output.push_str(&before_scheme[..scheme_start]);

        let url_start_in_rest = scheme_start;
        let candidate_start = &rest[url_start_in_rest..];
        let candidate_len = candidate_start
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ')' | ']'))
            .unwrap_or(candidate_start.len());
        let candidate = &candidate_start[..candidate_len];
        output.push_str(&sanitize_url(candidate));
        rest = &candidate_start[candidate_len..];

        if after_scheme_marker.is_empty() {
            break;
        }
    }
    output.push_str(rest);
    output
}

fn sanitize_url(candidate: &str) -> String {
    let Ok(mut url) = Url::parse(candidate) else {
        return candidate.to_string();
    };
    if !url.username().is_empty() {
        let _ = url.set_username(REDACTED);
    }
    if url.password().is_some() {
        let _ = url.set_password(Some(REDACTED));
    }
    if url.query().is_some() {
        let pairs = url
            .query_pairs()
            .map(|(key, value)| {
                let value = if is_sensitive_key(&key) {
                    REDACTED.into()
                } else {
                    sanitize_text(&value)
                };
                (key.into_owned(), value)
            })
            .collect::<Vec<_>>();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.set_fragment(None);
    url.to_string()
}

fn redact_sensitive_assignments(input: &str) -> String {
    let mut output = input.to_string();
    for key in SENSITIVE_KEYWORDS {
        output = redact_assignment_key(&output, key);
    }
    output
}

fn redact_json_like_fields(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while let Some((key_start, key_end, key)) = find_json_like_string(input, index) {
        let after_key = skip_ascii_whitespace(input, key_end);
        if !input[after_key..].starts_with(':') {
            output.push_str(&input[index..key_end]);
            index = key_end;
            continue;
        }
        let value_start = skip_ascii_whitespace(input, after_key + 1);
        if is_sensitive_key(&key) {
            output.push_str(&input[index..value_start]);
            output.push('"');
            output.push_str(REDACTED);
            output.push('"');
            index = json_like_value_end(input, value_start);
        } else {
            output.push_str(&input[index..key_start]);
            output.push_str(&input[key_start..key_end]);
            index = key_end;
        }
    }
    output.push_str(&input[index..]);
    output
}

fn find_json_like_string(input: &str, start: usize) -> Option<(usize, usize, String)> {
    let quote_start = start + input[start..].find('"')?;
    let quote_end = string_literal_end(input, quote_start)?;
    let key = serde_json::from_str::<String>(&input[quote_start..quote_end])
        .unwrap_or_else(|_| input[quote_start + 1..quote_end - 1].to_string());
    Some((quote_start, quote_end, key))
}

fn string_literal_end(input: &str, quote_start: usize) -> Option<usize> {
    quoted_literal_end(input, quote_start, '"')
}

fn quoted_literal_end(input: &str, quote_start: usize, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (offset, ch) in input[quote_start + quote.len_utf8()..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            ch if ch == quote => {
                return Some(quote_start + quote.len_utf8() + offset + ch.len_utf8());
            }
            _ => {}
        }
    }
    None
}

fn skip_ascii_whitespace(input: &str, start: usize) -> usize {
    let mut index = start;
    for ch in input[start..].chars() {
        if !ch.is_ascii_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn json_like_value_end(input: &str, start: usize) -> usize {
    let Some(first) = input[start..].chars().next() else {
        return start;
    };
    match first {
        '"' => string_literal_end(input, start).unwrap_or(input.len()),
        '{' => balanced_json_like_end(input, start, '{', '}'),
        '[' => balanced_json_like_end(input, start, '[', ']'),
        _ => input[start..]
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | '}' | ']'))
            .map_or(input.len(), |relative| start + relative),
    }
}

fn balanced_json_like_end(input: &str, start: usize, open: char, close: char) -> usize {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in input[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == open {
            depth += 1;
        } else if ch == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return start + offset + ch.len_utf8();
            }
        }
    }
    input.len()
}

fn redact_assignment_key(input: &str, key: &str) -> String {
    let mut output = String::new();
    let mut index = 0;
    let lower = input.to_ascii_lowercase();
    let key_lower = key.to_ascii_lowercase();
    while let Some(relative) = lower[index..].find(&key_lower) {
        let start = index + relative;
        let after_key = start + key.len();
        let separator = skip_ascii_whitespace(input, after_key);
        if !input[separator..].starts_with('=') {
            output.push_str(&input[index..after_key]);
            index = after_key;
            continue;
        }
        let value_start = skip_ascii_whitespace(input, separator + 1);
        output.push_str(&input[index..value_start]);
        output.push_str(REDACTED);
        let value_end = assignment_value_end(input, value_start);
        index = value_end;
    }
    output.push_str(&input[index..]);
    output
}

fn assignment_value_end(input: &str, start: usize) -> usize {
    let Some(first) = input[start..].chars().next() else {
        return start;
    };
    match first {
        '"' => string_literal_end(input, start).unwrap_or(input.len()),
        '\'' => quoted_literal_end(input, start, '\'').unwrap_or(input.len()),
        '{' => balanced_json_like_end(input, start, '{', '}'),
        '[' => balanced_json_like_end(input, start, '[', ']'),
        _ => bearer_assignment_value_end(input, start).unwrap_or_else(|| {
            input[start..]
                .find(assignment_value_delimiter)
                .map_or(input.len(), |relative_end| start + relative_end)
        }),
    }
}

fn bearer_assignment_value_end(input: &str, start: usize) -> Option<usize> {
    const BEARER: &str = "bearer";
    let rest = &input[start..];
    let prefix = rest.get(..BEARER.len())?;
    if !prefix.eq_ignore_ascii_case(BEARER) {
        return None;
    }
    let after_scheme = start + BEARER.len();
    if !input[after_scheme..]
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_whitespace())
    {
        return None;
    }
    let token_start = skip_ascii_whitespace(input, after_scheme);
    Some(
        input[token_start..]
            .find(assignment_value_delimiter)
            .map_or(input.len(), |relative_end| token_start + relative_end),
    )
}

fn assignment_value_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '&' | '"' | '\'' | '<' | '>' | ')' | ']' | ',')
}

fn classify_reqwest_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "timeout".into();
    }
    if error.is_connect() {
        let text = error.to_string().to_ascii_lowercase();
        if text.contains("refused") {
            return "connection_refused".into();
        }
        if text.contains("reset") {
            return "connection_reset".into();
        }
        return "connect_error".into();
    }
    if error.is_decode() {
        return "decode_error".into();
    }
    if error.is_request() {
        return "request_error".into();
    }
    if error.is_body() {
        return "body_error".into();
    }
    "transport_error".into()
}

fn classify_json_error(error: &serde_json::Error) -> String {
    match error.classify() {
        serde_json::error::Category::Eof => "eof".into(),
        serde_json::error::Category::Syntax => "invalid_json".into(),
        serde_json::error::Category::Data => "unexpected_json_shape".into(),
        serde_json::error::Category::Io => "io_error".into(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    SENSITIVE_KEYWORDS.iter().any(|needle| {
        normalized == needle.replace('_', "") || normalized.contains(&needle.replace('_', ""))
    })
}

fn bounded_chars(input: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx == max_chars {
            output.push_str("...[truncated]");
            return output;
        }
        output.push(ch);
    }
    output
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_redacts_fixture_secrets_before_persistence() {
        let input = concat!(
            "Authorization: Bearer fixturebearertoken\n",
            "token=fixture12345 ",
            "OPENAI_API_KEY=providerfixture12345 ",
            "https://user:pass@example.test/path?token=fixture12345&safe=ok"
        );

        let sanitized = sanitize_text(input);

        assert!(!sanitized.contains("fixturebearertoken"));
        assert!(!sanitized.contains("fixture12345"));
        assert!(!sanitized.contains("providerfixture12345"));
        assert!(!sanitized.contains("user:pass"));
        assert!(sanitized.contains("Authorization: <redacted>"));
        assert!(sanitized.contains("token=<redacted>"));
        assert!(sanitized.contains("OPENAI_API_KEY=<redacted>"));
    }

    #[test]
    fn sanitizer_redacts_sensitive_assignments_with_whitespace() {
        let input = concat!(
            "provider said retry later token = fixture-token\n",
            "api_key = fixture-api-key\n",
            "password = \"fixture-password\"\n",
            "authorization = Bearer fixture-token\n",
            "refresh_token\t=\tfixture-refresh"
        );

        let sanitized = sanitize_text(input);

        assert!(!sanitized.contains("fixture-token"));
        assert!(!sanitized.contains("fixture-api-key"));
        assert!(!sanitized.contains("fixture-password"));
        assert!(!sanitized.contains("fixture-refresh"));
        assert!(!sanitized.contains("Bearer fixture-token"));
        assert!(sanitized.contains("provider said retry later token = <redacted>"));
        assert!(sanitized.contains("password = <redacted>"));
        assert!(sanitized.contains("authorization = <redacted>"));
    }

    #[test]
    fn endpoint_path_keeps_path_and_redacts_sensitive_query_values() {
        let url =
            Url::parse("https://example.test/v1/rerank?token=fixture12345&wait=true#frag").unwrap();

        assert_eq!(
            sanitized_endpoint_path(&url),
            "/v1/rerank?token=<redacted>&wait=true"
        );
    }

    #[test]
    fn sanitizer_redacts_minified_json_secret_fields_after_safe_first_key() {
        let input = concat!(
            r#"{"error":"bad","#,
            r#""token":"fixture-token","#,
            r#""api_key":"fixture-api-key","#,
            r#""password":"fixture-password","#,
            r#""authorization":"Bearer fixture-auth","#,
            r#""safe":"ok"}"#
        );

        let sanitized = sanitize_text(input);
        let value: Value = serde_json::from_str(&sanitized).unwrap();

        assert_json_secret_free(&sanitized);
        assert_eq!(value["error"], "bad");
        assert_eq!(value["safe"], "ok");
        assert_eq!(value["token"], REDACTED);
        assert_eq!(value["api_key"], REDACTED);
        assert_eq!(value["password"], REDACTED);
        assert_eq!(value["authorization"], REDACTED);
    }

    #[test]
    fn sanitizer_redacts_nested_json_secret_fields() {
        let input = concat!(
            r#"{"error":"bad","details":{"access_token":"fixture-access","#,
            r#""items":[{"refresh_token":"fixture-refresh"}],"safe":"ok"}}"#
        );

        let sanitized = sanitize_text(input);
        let value: Value = serde_json::from_str(&sanitized).unwrap();

        assert_json_secret_free(&sanitized);
        assert_eq!(value["details"]["access_token"], REDACTED);
        assert_eq!(value["details"]["items"][0]["refresh_token"], REDACTED);
        assert_eq!(value["details"]["safe"], "ok");
    }

    #[test]
    fn sanitizer_redacts_malformed_json_like_secret_fields() {
        let input = concat!(
            r#"{"error":"bad","nested":{"token":"fixture-token"},"#,
            r#""items":[{"api_key":"fixture-api-key"}],"#,
            r#""password" : "fixture-password","#,
            r#""authorization":"Bearer fixture-auth""#
        );

        let sanitized = sanitize_text(input);

        assert_json_secret_free(&sanitized);
        assert!(sanitized.contains(r#""token":"<redacted>""#));
        assert!(sanitized.contains(r#""api_key":"<redacted>""#));
        assert!(sanitized.contains(r#""password" : "<redacted>""#));
        assert!(sanitized.contains(r#""authorization":"<redacted>""#));
    }

    fn assert_json_secret_free(sanitized: &str) {
        assert!(!sanitized.contains("fixture-token"));
        assert!(!sanitized.contains("fixture-api-key"));
        assert!(!sanitized.contains("fixture-password"));
        assert!(!sanitized.contains("fixture-auth"));
        assert!(!sanitized.contains("fixture-access"));
        assert!(!sanitized.contains("fixture-refresh"));
    }
}
