//! Idempotency keys and safe retry classification for remote mutations.

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageResult};

/// Client-supplied idempotency key for mutations.
///
/// Keys are opaque strings; servers treat identical keys for the same
/// principal/operation as the same logical mutation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdempotencyKey(pub String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> StorageResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "idempotency key must not be empty",
            ));
        }
        if value.len() > 256 {
            return Err(StorageError::invalid_request(
                "idempotency key exceeds 256 bytes",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// High-level mutation categories used for retry policy lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    /// Create-or-replace with server-side idempotency (safe to retry with key).
    Upsert,
    /// Delete by id (idempotent: missing target is success or not-found).
    Delete,
    /// Publish with generation fence (retry only with same key + generation).
    Publish,
    /// Enqueue task (safe only with idempotency key).
    Enqueue,
    /// Claim lease (not safe to blindly retry — may double-claim).
    Claim,
    /// Finish / ack task (safe with key).
    Finish,
    /// Non-mutation read (always safe).
    Read,
}

impl MutationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
            Self::Publish => "publish",
            Self::Enqueue => "enqueue",
            Self::Claim => "claim",
            Self::Finish => "finish",
            Self::Read => "read",
        }
    }

    pub fn is_mutation(self) -> bool {
        !matches!(self, Self::Read)
    }
}

/// Whether a failed remote call may be retried by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    /// Safe to retry without changing semantics (reads; keyed mutations).
    Safe,
    /// Retry only when the original [`IdempotencyKey`] is resent unchanged.
    SafeWithIdempotencyKey,
    /// Must not retry — side effects may have already applied.
    Unsafe,
}

impl RetryClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::SafeWithIdempotencyKey => "safe_with_idempotency_key",
            Self::Unsafe => "unsafe",
        }
    }

    pub fn is_safe(self) -> bool {
        matches!(self, Self::Safe | Self::SafeWithIdempotencyKey)
    }
}

/// Retry policy for one operation under the client contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub kind: MutationKind,
    pub class: RetryClass,
    /// Maximum attempts including the first try. Zero is invalid.
    pub max_attempts: u32,
    /// Optional required idempotency key when class needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
}

impl RetryPolicy {
    pub fn for_operation(
        kind: MutationKind,
        idempotency_key: Option<IdempotencyKey>,
    ) -> StorageResult<Self> {
        let class = classify_retry(kind, idempotency_key.is_some());
        let max_attempts = default_max_attempts(class);
        let policy = Self {
            kind,
            class,
            max_attempts,
            idempotency_key,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.max_attempts == 0 {
            return Err(StorageError::invalid_request(
                "retry max_attempts must be > 0",
            ));
        }
        if self.max_attempts > 32 {
            return Err(StorageError::invalid_request(
                "retry max_attempts exceeds absurd ceiling 32",
            ));
        }
        match self.class {
            RetryClass::SafeWithIdempotencyKey => {
                if self.idempotency_key.is_none() {
                    return Err(StorageError::invalid_request(format!(
                        "mutation {} requires an idempotency key for safe retry",
                        self.kind.as_str()
                    )));
                }
            }
            RetryClass::Safe | RetryClass::Unsafe => {}
        }
        if self.kind == MutationKind::Read && self.class != RetryClass::Safe {
            return Err(StorageError::invalid_request(
                "read operations must use RetryClass::Safe",
            ));
        }
        if self.kind.is_mutation()
            && self.class == RetryClass::Safe
            && !matches!(self.kind, MutationKind::Delete)
        {
            // Only Delete is inherently safe without a key among mutations.
            return Err(StorageError::invalid_request(format!(
                "mutation {} cannot be RetryClass::Safe without special-case policy",
                self.kind.as_str()
            )));
        }
        Ok(())
    }

    /// Whether the client may schedule another attempt after a retryable fault.
    pub fn allows_retry(&self, attempt: u32) -> bool {
        if attempt >= self.max_attempts {
            return false;
        }
        match self.class {
            RetryClass::Safe => true,
            RetryClass::SafeWithIdempotencyKey => self.idempotency_key.is_some(),
            RetryClass::Unsafe => false,
        }
    }
}

/// Classify retry safety for a mutation kind given presence of an idempotency key.
pub fn classify_retry(kind: MutationKind, has_idempotency_key: bool) -> RetryClass {
    match kind {
        MutationKind::Read => RetryClass::Safe,
        MutationKind::Delete => {
            // Deletes are naturally idempotent by id.
            RetryClass::Safe
        }
        MutationKind::Upsert
        | MutationKind::Publish
        | MutationKind::Enqueue
        | MutationKind::Finish => {
            if has_idempotency_key {
                RetryClass::SafeWithIdempotencyKey
            } else {
                RetryClass::Unsafe
            }
        }
        MutationKind::Claim => RetryClass::Unsafe,
    }
}

fn default_max_attempts(class: RetryClass) -> u32 {
    match class {
        RetryClass::Safe => 3,
        RetryClass::SafeWithIdempotencyKey => 3,
        RetryClass::Unsafe => 1,
    }
}
