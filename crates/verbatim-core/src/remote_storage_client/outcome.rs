//! Typed remote outcomes and mapping onto storage port errors.

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageCapabilityKind, StorageError, StorageGeneration, StorageResult};

/// Machine-readable remote call status (including partial success).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteStatus {
    Ok,
    Partial,
    Unavailable {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Timeout {
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Conflict {
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    StaleGeneration {
        expected: StorageGeneration,
        actual: StorageGeneration,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Unauthorized {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Unsupported {
        capability: StorageCapabilityKind,
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    NotFound {
        resource: String,
        id: String,
    },
    InvalidRequest {
        detail: String,
    },
}

impl RemoteStatus {
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Unavailable { .. } => "unavailable",
            Self::Timeout { .. } => "timeout",
            Self::Conflict { .. } => "conflict",
            Self::StaleGeneration { .. } => "stale_generation",
            Self::Unauthorized { .. } => "unauthorized",
            Self::Unsupported { .. } => "unsupported",
            Self::NotFound { .. } => "not_found",
            Self::InvalidRequest { .. } => "invalid_request",
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Ok | Self::Partial)
    }

    pub fn is_partial(&self) -> bool {
        matches!(self, Self::Partial)
    }
}

/// Metadata describing a marked partial result (never silent truncation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialResultMeta {
    /// Why the result is partial (timeout mid-stream, backend shard down, …).
    pub reason: String,
    /// True when more data may exist beyond what was returned.
    pub truncated: bool,
    /// Optional opaque resume cursor for a follow-up call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cursor: Option<String>,
    /// Optional count of items omitted or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_hint: Option<u64>,
}

impl PartialResultMeta {
    pub fn new(reason: impl Into<String>, truncated: bool) -> StorageResult<Self> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "partial result reason must not be empty",
            ));
        }
        Ok(Self {
            reason,
            truncated,
            resume_cursor: None,
            omitted_hint: None,
        })
    }

    pub fn with_resume_cursor(mut self, cursor: impl Into<String>) -> StorageResult<Self> {
        let cursor = cursor.into();
        if cursor.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "partial result resume_cursor must not be empty when set",
            ));
        }
        self.resume_cursor = Some(cursor);
        Ok(self)
    }
}

/// Full remote outcome envelope: status + optional partial metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteOutcome {
    pub status: RemoteStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<PartialResultMeta>,
}

impl RemoteOutcome {
    pub fn ok() -> Self {
        Self {
            status: RemoteStatus::Ok,
            partial: None,
        }
    }

    pub fn partial(meta: PartialResultMeta) -> Self {
        Self {
            status: RemoteStatus::Partial,
            partial: Some(meta),
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::Unavailable {
                detail: Some(detail.into()),
            },
            partial: None,
        }
    }

    pub fn timeout(operation: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::Timeout {
                operation: operation.into(),
                detail: None,
            },
            partial: None,
        }
    }

    pub fn conflict(resource: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::Conflict {
                resource: resource.into(),
                detail: None,
            },
            partial: None,
        }
    }

    pub fn stale_generation(expected: StorageGeneration, actual: StorageGeneration) -> Self {
        Self {
            status: RemoteStatus::StaleGeneration {
                expected,
                actual,
                detail: None,
            },
            partial: None,
        }
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::Unauthorized {
                detail: Some(detail.into()),
            },
            partial: None,
        }
    }

    pub fn unsupported(capability: StorageCapabilityKind, operation: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::Unsupported {
                capability,
                operation: operation.into(),
                detail: None,
            },
            partial: None,
        }
    }

    pub fn not_found(resource: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::NotFound {
                resource: resource.into(),
                id: id.into(),
            },
            partial: None,
        }
    }

    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::InvalidRequest {
                detail: detail.into(),
            },
            partial: None,
        }
    }

    pub fn validate(&self) -> StorageResult<()> {
        match (&self.status, &self.partial) {
            (RemoteStatus::Partial, None) => Err(StorageError::invalid_request(
                "partial remote status requires PartialResultMeta",
            )),
            (RemoteStatus::Partial, Some(meta)) => {
                if meta.reason.trim().is_empty() {
                    return Err(StorageError::invalid_request(
                        "partial result reason must not be empty",
                    ));
                }
                Ok(())
            }
            (_, Some(_)) => Err(StorageError::invalid_request(
                "PartialResultMeta is only valid with RemoteStatus::Partial",
            )),
            _ => Ok(()),
        }
    }
}

/// Result alias using [`RemoteOutcome`] for failures that remain machine-readable
/// including partial success wrappers at the transport boundary.
pub type RemoteResult<T> = Result<T, RemoteOutcome>;

/// Map a non-success remote outcome onto [`StorageError`].
///
/// - `Ok` and `Partial` are not errors; mapping them returns `InvalidRequest`
///   so callers cannot silently drop partial markers.
/// - All other statuses project 1:1 onto storage error classes.
pub fn map_remote_outcome_to_storage_error(outcome: &RemoteOutcome) -> StorageResult<StorageError> {
    outcome.validate()?;
    let err = match &outcome.status {
        RemoteStatus::Ok => {
            return Err(StorageError::invalid_request(
                "cannot map RemoteStatus::Ok to StorageError",
            ));
        }
        RemoteStatus::Partial => {
            return Err(StorageError::invalid_request(
                "cannot map RemoteStatus::Partial to StorageError; surface PartialResultMeta",
            ));
        }
        RemoteStatus::Unavailable { detail } => match detail {
            Some(d) => StorageError::unavailable(d.clone()),
            None => StorageError::Unavailable { detail: None },
        },
        RemoteStatus::Timeout { operation, detail } => StorageError::Timeout {
            operation: operation.clone(),
            detail: detail.clone(),
        },
        RemoteStatus::Conflict { resource, detail } => StorageError::Conflict {
            resource: resource.clone(),
            detail: detail.clone(),
        },
        RemoteStatus::StaleGeneration {
            expected,
            actual,
            detail,
        } => StorageError::StaleGeneration {
            expected: *expected,
            actual: *actual,
            detail: detail.clone(),
        },
        RemoteStatus::Unauthorized { detail } => StorageError::Unauthorized {
            detail: detail.clone(),
        },
        RemoteStatus::Unsupported {
            capability,
            operation,
            detail,
        } => StorageError::Unsupported {
            capability: *capability,
            operation: operation.clone(),
            detail: detail.clone(),
        },
        RemoteStatus::NotFound { resource, id } => StorageError::NotFound {
            resource: resource.clone(),
            id: id.clone(),
        },
        RemoteStatus::InvalidRequest { detail } => StorageError::invalid_request(detail.clone()),
    };
    Ok(err)
}
