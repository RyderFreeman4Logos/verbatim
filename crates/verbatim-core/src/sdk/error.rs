//! Typed SDK client errors (transport, auth, validation, unsupported, compatibility).

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::capability::SdkCapabilityKind;

/// Result alias for SDK client operations.
pub type ClientResult<T> = Result<T, ClientError>;

/// Typed client-side failure classes for the stable SDK surface.
///
/// Adapters should project transport/daemon faults into these classes rather
/// than leaking internal `anyhow` / HTTP status strings as the public contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum ClientError {
    /// Network / HTTP transport failure (unreachable, reset, protocol).
    Transport {
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Authentication or authorization failure.
    Auth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Client-side structural/semantic validation before transport.
    Validation { detail: String },
    /// Server/client reported that a capability or operation is unsupported.
    Unsupported {
        capability: SdkCapabilityKind,
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Protocol / wire schema / SDK capability negotiation mismatch.
    Compatibility {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Requested resource was not found.
    NotFound { resource: String, id: String },
    /// Operation exceeded the client or server timeout budget.
    Timeout {
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Cursor / pagination contract failure projected from [`crate::pagination`].
    Pagination {
        /// Stable pagination failure class (e.g. `mode_mismatch`).
        ///
        /// Named `error_class` so it does not collide with the serde
        /// `#[serde(tag = "class")]` discriminator field.
        error_class: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl ClientError {
    pub fn transport(operation: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Transport {
            operation: operation.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn auth(detail: impl Into<String>) -> Self {
        Self::Auth {
            detail: Some(detail.into()),
        }
    }

    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }

    pub fn unsupported(
        capability: SdkCapabilityKind,
        operation: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            capability,
            operation: operation.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn compatibility(detail: impl Into<String>) -> Self {
        Self::Compatibility {
            detail: Some(detail.into()),
        }
    }

    pub fn not_found(resource: impl Into<String>, id: impl Into<String>) -> Self {
        Self::NotFound {
            resource: resource.into(),
            id: id.into(),
        }
    }

    pub fn timeout(operation: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Timeout {
            operation: operation.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn pagination(error_class: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Pagination {
            error_class: error_class.into(),
            detail: Some(detail.into()),
        }
    }

    /// Stable class name for metrics / redacted logs.
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "transport",
            Self::Auth { .. } => "auth",
            Self::Validation { .. } => "validation",
            Self::Unsupported { .. } => "unsupported",
            Self::Compatibility { .. } => "compatibility",
            Self::NotFound { .. } => "not_found",
            Self::Timeout { .. } => "timeout",
            Self::Pagination { .. } => "pagination",
        }
    }

    /// Whether callers may safely retry the same logical operation.
    ///
    /// Validation, auth, unsupported, and compatibility errors are not retryable.
    /// Transport and timeout may be retryable under an adapter policy.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport { .. } | Self::Timeout { .. })
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { operation, detail } => {
                write!(f, "sdk transport error during {operation}")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Auth { detail } => {
                write!(f, "sdk authentication/authorization failed")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Validation { detail } => write!(f, "sdk validation error: {detail}"),
            Self::Unsupported {
                capability,
                operation,
                detail,
            } => {
                write!(
                    f,
                    "sdk unsupported capability {} for operation {operation}",
                    capability.as_str()
                )?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Compatibility { detail } => {
                write!(f, "sdk compatibility error")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::NotFound { resource, id } => {
                write!(f, "sdk resource {resource} not found: {id}")
            }
            Self::Timeout { operation, detail } => {
                write!(f, "sdk timeout during {operation}")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Pagination {
                error_class,
                detail,
            } => {
                write!(f, "sdk pagination error ({error_class})")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for ClientError {}

impl From<crate::pagination::CursorError> for ClientError {
    fn from(err: crate::pagination::CursorError) -> Self {
        Self::Pagination {
            error_class: err.class_name().to_string(),
            detail: Some(err.to_string()),
        }
    }
}
