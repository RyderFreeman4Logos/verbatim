//! Explicit cursor validation failures (fail closed; no silent page rebinding).

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageGeneration};

use super::page::PaginationMode;

/// Typed cursor / pagination contract failure.
///
/// Adapters should surface these classes to clients rather than rewriting the
/// page under a new generation, principal, or profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum CursorError {
    /// Cursor past its expiry epoch.
    Expired { expires_at_unix: u64, now_unix: u64 },
    /// Malformed, tampered, or structurally invalid cursor.
    Invalid { detail: String },
    /// Principal / authorization scope does not match the sealed cursor.
    Unauthorized { detail: String },
    /// Bound publication generation is no longer readable / retained.
    GenerationGone {
        bound: StorageGeneration,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        available: Option<StorageGeneration>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Ranking / embedding / retrieval profile drifted from the cursor.
    ProfileChanged { expected: String, actual: String },
    /// Lifecycle / ACL policy version drifted from the cursor.
    PolicyChanged { expected: String, actual: String },
    /// Query plan identity does not match the continuation request.
    QueryMismatch { expected: String, actual: String },
    /// Ranked vs exhaustive mode mismatch (modes are not interchangeable).
    ModeMismatch {
        expected: PaginationMode,
        actual: PaginationMode,
    },
}

impl CursorError {
    pub fn expired(expires_at_unix: u64, now_unix: u64) -> Self {
        Self::Expired {
            expires_at_unix,
            now_unix,
        }
    }

    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
        }
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::Unauthorized {
            detail: detail.into(),
        }
    }

    pub fn generation_gone(
        bound: StorageGeneration,
        available: Option<StorageGeneration>,
        detail: impl Into<String>,
    ) -> Self {
        Self::GenerationGone {
            bound,
            available,
            detail: Some(detail.into()),
        }
    }

    pub fn profile_changed(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::ProfileChanged {
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    pub fn policy_changed(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::PolicyChanged {
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    pub fn query_mismatch(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::QueryMismatch {
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    pub fn mode_mismatch(expected: PaginationMode, actual: PaginationMode) -> Self {
        Self::ModeMismatch { expected, actual }
    }

    /// Stable class name for metrics / redacted logs.
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Expired { .. } => "expired",
            Self::Invalid { .. } => "invalid",
            Self::Unauthorized { .. } => "unauthorized",
            Self::GenerationGone { .. } => "generation_gone",
            Self::ProfileChanged { .. } => "profile_changed",
            Self::PolicyChanged { .. } => "policy_changed",
            Self::QueryMismatch { .. } => "query_mismatch",
            Self::ModeMismatch { .. } => "mode_mismatch",
        }
    }

    /// Map into the shared storage error surface used by ports/adapters.
    pub fn to_storage_error(&self) -> StorageError {
        match self {
            Self::Expired {
                expires_at_unix,
                now_unix,
            } => StorageError::invalid_request(format!(
                "cursor expired at unix {expires_at_unix} (now {now_unix})"
            )),
            Self::Invalid { detail } => {
                StorageError::invalid_request(format!("cursor invalid: {detail}"))
            }
            Self::Unauthorized { detail } => {
                StorageError::unauthorized(format!("cursor: {detail}"))
            }
            Self::GenerationGone {
                bound,
                available,
                detail,
            } => {
                let detail_msg = detail.clone().unwrap_or_else(|| {
                    "cursor publication generation is gone or no longer readable".into()
                });
                // Only map to StaleGeneration when a real observed generation is
                // known. Do not invent StorageGeneration::INITIAL as `actual`.
                match available {
                    Some(actual) => {
                        let mut err = StorageError::stale_generation(*bound, *actual);
                        if let StorageError::StaleGeneration { detail: slot, .. } = &mut err {
                            *slot = Some(detail_msg);
                        }
                        err
                    }
                    None => StorageError::unavailable(format!(
                        "cursor generation gone: bound {bound}: {detail_msg}"
                    )),
                }
            }
            Self::ProfileChanged { expected, actual } => StorageError::invalid_request(format!(
                "cursor profile changed: expected {expected}, actual {actual}"
            )),
            Self::PolicyChanged { expected, actual } => StorageError::invalid_request(format!(
                "cursor policy version changed: expected {expected}, actual {actual}"
            )),
            Self::QueryMismatch { expected, actual } => StorageError::invalid_request(format!(
                "cursor query plan mismatch: expected {expected}, actual {actual}"
            )),
            Self::ModeMismatch { expected, actual } => StorageError::invalid_request(format!(
                "cursor pagination mode mismatch: expected {}, actual {}",
                expected.as_str(),
                actual.as_str()
            )),
        }
    }
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expired {
                expires_at_unix,
                now_unix,
            } => write!(
                f,
                "cursor expired at unix {expires_at_unix} (now {now_unix})"
            ),
            Self::Invalid { detail } => write!(f, "invalid cursor: {detail}"),
            Self::Unauthorized { detail } => write!(f, "unauthorized cursor: {detail}"),
            Self::GenerationGone {
                bound,
                available,
                detail,
            } => {
                write!(f, "cursor generation gone: bound {bound}")?;
                if let Some(available) = available {
                    write!(f, ", available {available}")?;
                }
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::ProfileChanged { expected, actual } => {
                write!(
                    f,
                    "cursor profile changed: expected {expected}, actual {actual}"
                )
            }
            Self::PolicyChanged { expected, actual } => write!(
                f,
                "cursor policy version changed: expected {expected}, actual {actual}"
            ),
            Self::QueryMismatch { expected, actual } => write!(
                f,
                "cursor query plan mismatch: expected {expected}, actual {actual}"
            ),
            Self::ModeMismatch { expected, actual } => write!(
                f,
                "cursor pagination mode mismatch: expected {}, actual {}",
                expected.as_str(),
                actual.as_str()
            ),
        }
    }
}

impl Error for CursorError {}

/// Result alias for cursor open / validation operations.
pub type CursorResult<T> = Result<T, CursorError>;
