//! SDK client configuration (endpoint, auth token, timeout, user-agent, capability cache).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::capability::CapabilityCache;
use super::error::{ClientError, ClientResult};

/// Default request timeout for SDK clients (seconds).
pub const DEFAULT_SDK_TIMEOUT_SECS: u64 = 30;

/// Default user-agent label for SDK clients.
pub const DEFAULT_SDK_USER_AGENT: &str = "verbatim-sdk/0.1";

/// Field bundle for constructing [`SdkConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkConfigFields {
    pub endpoint: String,
    pub auth_token: Option<String>,
    pub timeout: Duration,
    pub user_agent: String,
    pub capability_cache: CapabilityCache,
}

/// Client-side configuration for a Verbatim SDK binding.
///
/// Secrets stay off Debug (token redacted). This type does not open sockets or
/// perform HTTP — transport adapters consume it later.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkConfig {
    /// Base API endpoint URL (http/https). Must be non-empty and absolute-shaped.
    pub endpoint: String,
    /// Optional bearer token. Never logged; Debug redacts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// Per-request timeout budget.
    #[serde(with = "duration_secs")]
    pub timeout: Duration,
    /// User-agent product token sent by adapters.
    pub user_agent: String,
    /// Cached capability negotiation result (may be empty until discovered).
    pub capability_cache: CapabilityCache,
}

impl SdkConfig {
    pub fn new(fields: SdkConfigFields) -> ClientResult<Self> {
        let cfg = Self {
            endpoint: fields.endpoint,
            auth_token: fields.auth_token,
            timeout: fields.timeout,
            user_agent: fields.user_agent,
            capability_cache: fields.capability_cache,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Construct with defaults for timeout, user-agent, and empty capability cache.
    pub fn with_endpoint(endpoint: impl Into<String>) -> ClientResult<Self> {
        Self::new(SdkConfigFields {
            endpoint: endpoint.into(),
            auth_token: None,
            timeout: Duration::from_secs(DEFAULT_SDK_TIMEOUT_SECS),
            user_agent: DEFAULT_SDK_USER_AGENT.to_string(),
            capability_cache: CapabilityCache::empty(),
        })
    }

    pub fn with_auth_token(mut self, token: impl Into<String>) -> ClientResult<Self> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(ClientError::validation(
                "auth_token must not be empty when set",
            ));
        }
        self.auth_token = Some(token);
        Ok(self)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> ClientResult<Self> {
        if timeout.is_zero() {
            return Err(ClientError::validation("timeout must be > 0"));
        }
        self.timeout = timeout;
        Ok(self)
    }

    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> ClientResult<Self> {
        let user_agent = user_agent.into();
        if user_agent.trim().is_empty() {
            return Err(ClientError::validation("user_agent must not be empty"));
        }
        self.user_agent = user_agent;
        Ok(self)
    }

    pub fn with_capability_cache(mut self, cache: CapabilityCache) -> Self {
        self.capability_cache = cache;
        self
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("endpoint", &self.endpoint)?;
        if !(self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://")) {
            return Err(ClientError::validation(
                "endpoint must start with http:// or https://",
            ));
        }
        // Reject whitespace-only authority-ish paths and bare scheme.
        let rest = self
            .endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        if rest.trim().is_empty() || rest.contains(char::is_whitespace) {
            return Err(ClientError::validation(
                "endpoint must include a non-empty host without whitespace",
            ));
        }
        if self.timeout.is_zero() {
            return Err(ClientError::validation("timeout must be > 0"));
        }
        require_non_empty("user_agent", &self.user_agent)?;
        if let Some(token) = &self.auth_token {
            if token.trim().is_empty() {
                return Err(ClientError::validation(
                    "auth_token must not be empty when set",
                ));
            }
        }
        self.capability_cache.validate()?;
        Ok(())
    }

    pub fn has_auth_token(&self) -> bool {
        self.auth_token
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty())
    }

    pub fn timeout_secs(&self) -> u64 {
        self.timeout.as_secs().max(1)
    }
}

impl std::fmt::Debug for SdkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkConfig")
            .field("endpoint", &self.endpoint)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("timeout", &self.timeout)
            .field("user_agent", &self.user_agent)
            .field("capability_cache", &self.capability_cache)
            .finish()
    }
}

fn require_non_empty(field: &str, value: &str) -> ClientResult<()> {
    if value.trim().is_empty() {
        return Err(ClientError::validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(Duration::from_secs(secs))
    }
}
