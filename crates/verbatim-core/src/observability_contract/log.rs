//! Structured logs and automatic redaction policy.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use super::common::{require_non_empty, validate_schema_version};
use super::trace::TraceContext;
use super::OBSERVABILITY_CONTRACT_SCHEMA_VERSION;

/// Structured log severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Categories of sensitive material that must not appear in default logs/metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveKind {
    Query,
    Evidence,
    Path,
    Credential,
    Token,
    Metadata,
}

/// Automatic redaction policy for logs and metric labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionPolicy {
    pub schema_version: u32,
    /// Field names (case-insensitive) that are always redacted.
    pub sensitive_field_names: BTreeSet<String>,
    /// Kinds enabled for pattern/heuristic redaction.
    pub enabled_kinds: BTreeSet<SensitiveKind>,
    /// Replacement token written in place of redacted values.
    pub replacement: String,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self::strict_default()
    }
}

impl RedactionPolicy {
    /// Production-default policy: redact queries, evidence, paths, credentials,
    /// tokens, and sensitive metadata field names.
    pub fn strict_default() -> Self {
        let sensitive_field_names = [
            "query",
            "user_query",
            "prompt",
            "evidence",
            "evidence_text",
            "snippet",
            "path",
            "file_path",
            "source_path",
            "authorization",
            "api_key",
            "token",
            "access_token",
            "refresh_token",
            "password",
            "secret",
            "credential",
            "cookie",
            "set_cookie",
            "raw_body",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let enabled_kinds = BTreeSet::from([
            SensitiveKind::Query,
            SensitiveKind::Evidence,
            SensitiveKind::Path,
            SensitiveKind::Credential,
            SensitiveKind::Token,
            SensitiveKind::Metadata,
        ]);
        Self {
            schema_version: OBSERVABILITY_CONTRACT_SCHEMA_VERSION,
            sensitive_field_names,
            enabled_kinds,
            replacement: "[REDACTED]".to_string(),
        }
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_schema()?;
        require_non_empty("redaction replacement", &self.replacement)?;
        Ok(())
    }

    /// True when the field name is listed as sensitive (case-insensitive).
    pub fn is_sensitive_field(&self, field_name: &str) -> bool {
        self.sensitive_field_names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(field_name))
    }

    /// Redact a structured field value when the field is sensitive or the value
    /// looks like a secret/path under enabled kinds.
    pub fn redact_field(&self, field_name: &str, value: &str) -> String {
        if self.is_sensitive_field(field_name) || self.looks_sensitive(value) {
            self.replacement.clone()
        } else {
            value.to_string()
        }
    }

    /// Redact free-form message text (path/token/bearer heuristics only).
    pub fn redact_message(&self, message: &str) -> String {
        if self.looks_sensitive(message) {
            self.replacement.clone()
        } else {
            message.to_string()
        }
    }

    fn looks_sensitive(&self, value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        let credentialish = self.enabled_kinds.contains(&SensitiveKind::Credential)
            || self.enabled_kinds.contains(&SensitiveKind::Token);
        if credentialish
            && (lower.contains("bearer ")
                || lower.contains("api_key=")
                || lower.contains("authorization:")
                || looks_like_token(value))
        {
            return true;
        }
        self.enabled_kinds.contains(&SensitiveKind::Path) && looks_like_path(value)
    }
}

fn looks_like_token(value: &str) -> bool {
    // Long opaque strings with no whitespace are treated as credential-like.
    let trimmed = value.trim();
    if trimmed.len() < 32 || trimmed.contains(char::is_whitespace) {
        return false;
    }
    let alnumish = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='))
        .count();
    alnumish * 100 / trimmed.len() >= 90
}

fn looks_like_path(value: &str) -> bool {
    // Whole-value absolute POSIX path, or a path embedded in free-form text.
    if value.split_whitespace().any(is_absolute_path_token) {
        return true;
    }
    // Also catch path-like tokens glued to punctuation: `(/var/lib/x)`.
    value
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | ')' | '('))
        .any(is_absolute_path_token)
}

fn is_absolute_path_token(token: &str) -> bool {
    let trimmed =
        token.trim_matches(|c: char| matches!(c, ':' | ',' | ';' | '"' | '\'' | ')' | '('));
    if trimmed.starts_with('/') && trimmed.matches('/').count() >= 2 && trimmed.len() > 3 {
        return true;
    }
    let bytes = trimmed.as_bytes();
    // Windows drive path: C:\...
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Structured log entry with automatic field redaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub schema_version: u32,
    pub level: LogLevel,
    pub message: String,
    pub context: TraceContext,
    /// Already-redacted structured fields (callers should use [`LogEntry::build`]).
    pub fields: BTreeMap<String, String>,
    /// Unix epoch milliseconds (UTC).
    pub timestamp_unix_ms: u64,
}

/// Inputs for [`LogEntry::build`].
#[derive(Debug, Clone)]
pub struct LogEntryParams {
    pub level: LogLevel,
    pub message: String,
    pub context: TraceContext,
    pub fields: BTreeMap<String, String>,
    pub timestamp_unix_ms: u64,
    pub policy: RedactionPolicy,
}

impl LogEntry {
    /// Build a log entry applying [`RedactionPolicy`] to message and fields.
    pub fn build(params: LogEntryParams) -> Result<Self> {
        params.context.validate()?;
        params.policy.validate()?;
        let fields = params
            .fields
            .into_iter()
            .map(|(key, value)| {
                let redacted = params.policy.redact_field(&key, &value);
                (key, redacted)
            })
            .collect();
        let entry = Self {
            schema_version: OBSERVABILITY_CONTRACT_SCHEMA_VERSION,
            level: params.level,
            message: params.policy.redact_message(&params.message),
            context: params.context,
            fields,
            timestamp_unix_ms: params.timestamp_unix_ms,
        };
        entry.validate()?;
        Ok(entry)
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_schema()?;
        self.context.validate()?;
        require_non_empty("log message", &self.message)?;
        Ok(())
    }
}

/// Decode JSON — fail closed on unknown schema versions.
pub fn decode_log_entry_json(bytes: &[u8]) -> Result<LogEntry> {
    let entry: LogEntry = serde_json::from_slice(bytes)?;
    entry.validate()?;
    Ok(entry)
}

/// Decode JSON — fail closed on unknown schema versions.
pub fn decode_redaction_policy_json(bytes: &[u8]) -> Result<RedactionPolicy> {
    let policy: RedactionPolicy = serde_json::from_slice(bytes)?;
    policy.validate()?;
    Ok(policy)
}
