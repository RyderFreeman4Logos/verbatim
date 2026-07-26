//! Mutation idempotency key + pure in-memory registry (one logical op per key).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageResult};

/// Client-supplied idempotency key for mutations under a principal.
///
/// Distinct from remote-client retry classification: this registry records the
/// logical operation outcome so retries return the same result token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MutationIdempotencyKey(pub String);

impl MutationIdempotencyKey {
    pub fn new(value: impl Into<String>) -> StorageResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "mutation idempotency key must not be empty",
            ));
        }
        if value.len() > 256 {
            return Err(StorageError::invalid_request(
                "mutation idempotency key exceeds 256 bytes",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Deterministic fingerprint of the mutation request body / parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MutationOperationFingerprint(pub String);

impl MutationOperationFingerprint {
    pub fn new(value: impl Into<String>) -> StorageResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "mutation operation fingerprint must not be empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque token identifying the completed logical mutation result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MutationResultToken(pub String);

impl MutationResultToken {
    pub fn new(value: impl Into<String>) -> StorageResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "mutation result token must not be empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Outcome of claiming an idempotency key before executing a mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdempotencyClaim {
    /// First use — caller should execute the mutation then [`InMemoryIdempotencyRegistry::complete`].
    Fresh,
    /// Prior completion — return the stored result token without re-executing.
    Replay { result: MutationResultToken },
    /// Key seen but not yet completed (concurrent / interrupted first attempt).
    InProgress,
}

/// Failures from the pure idempotency registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum IdempotencyError {
    /// Same key + principal reused with a different operation fingerprint.
    Conflict {
        principal: String,
        key: String,
        detail: String,
    },
    Invalid {
        detail: String,
    },
}

impl IdempotencyError {
    pub fn conflict(
        principal: impl Into<String>,
        key: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Conflict {
            principal: principal.into(),
            key: key.into(),
            detail: detail.into(),
        }
    }

    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
        }
    }

    pub fn to_storage_error(&self) -> StorageError {
        match self {
            Self::Conflict {
                principal,
                key,
                detail,
            } => StorageError::conflict(format!("idempotency:{principal}:{key}"))
                .with_detail_if_supported(detail),
            Self::Invalid { detail } => StorageError::invalid_request(detail.clone()),
        }
    }
}

impl std::fmt::Display for IdempotencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict {
                principal,
                key,
                detail,
            } => write!(
                f,
                "idempotency conflict for principal {principal} key {key}: {detail}"
            ),
            Self::Invalid { detail } => write!(f, "idempotency invalid: {detail}"),
        }
    }
}

impl std::error::Error for IdempotencyError {}

/// Stored registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryEntry {
    operation: MutationOperationFingerprint,
    result: Option<MutationResultToken>,
}

/// Pure in-memory idempotency registry: one logical operation per (principal, key).
#[derive(Debug, Default)]
pub struct InMemoryIdempotencyRegistry {
    entries: HashMap<(String, String), RegistryEntry>,
}

impl InMemoryIdempotencyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim a key for execution or return a prior result.
    pub fn claim(
        &mut self,
        principal: impl Into<String>,
        key: &MutationIdempotencyKey,
        operation: &MutationOperationFingerprint,
    ) -> Result<IdempotencyClaim, IdempotencyError> {
        let principal = principal.into();
        if principal.trim().is_empty() {
            return Err(IdempotencyError::invalid("principal must not be empty"));
        }
        let map_key = (principal, key.0.clone());
        match self.entries.get(&map_key) {
            None => {
                self.entries.insert(
                    map_key,
                    RegistryEntry {
                        operation: operation.clone(),
                        result: None,
                    },
                );
                Ok(IdempotencyClaim::Fresh)
            }
            Some(entry) if entry.operation != *operation => Err(IdempotencyError::conflict(
                map_key.0,
                map_key.1,
                "idempotency key reused with a different operation fingerprint",
            )),
            Some(entry) => match &entry.result {
                Some(result) => Ok(IdempotencyClaim::Replay {
                    result: result.clone(),
                }),
                None => Ok(IdempotencyClaim::InProgress),
            },
        }
    }

    /// Record completion of a Fresh claim. Replays of the same result are no-ops.
    pub fn complete(
        &mut self,
        principal: impl Into<String>,
        key: &MutationIdempotencyKey,
        operation: &MutationOperationFingerprint,
        result: MutationResultToken,
    ) -> Result<(), IdempotencyError> {
        let principal = principal.into();
        if principal.trim().is_empty() {
            return Err(IdempotencyError::invalid("principal must not be empty"));
        }
        let map_key = (principal.clone(), key.0.clone());
        match self.entries.get_mut(&map_key) {
            None => Err(IdempotencyError::invalid(
                "cannot complete unknown idempotency key; claim first",
            )),
            Some(entry) if entry.operation != *operation => Err(IdempotencyError::conflict(
                principal,
                key.0.clone(),
                "complete operation fingerprint does not match claim",
            )),
            Some(entry) => match &entry.result {
                Some(existing) if existing != &result => Err(IdempotencyError::conflict(
                    principal,
                    key.0.clone(),
                    "idempotency key already completed with a different result token",
                )),
                Some(_) => Ok(()),
                None => {
                    entry.result = Some(result);
                    Ok(())
                }
            },
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Local helper: StorageError has no with_detail builder for Conflict in all
/// variants; attach detail by reconstructing when possible.
trait ConflictDetailExt {
    fn with_detail_if_supported(self, detail: &str) -> Self;
}

impl ConflictDetailExt for StorageError {
    fn with_detail_if_supported(self, detail: &str) -> Self {
        match self {
            StorageError::Conflict { resource, .. } => StorageError::Conflict {
                resource,
                detail: Some(detail.to_string()),
            },
            other => other,
        }
    }
}
