//! Request bounds: deadlines, payload limits, concurrency/queue, cancellation.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::storage_ports::{DurationMillis, StorageError, StorageResult};

/// Absolute or relative deadline for a single remote call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestDeadline {
    /// Relative timeout from call start.
    Timeout(DurationMillis),
    /// Absolute deadline as unix epoch milliseconds.
    AbsoluteUnixMs(u64),
}

impl RequestDeadline {
    pub fn from_timeout(duration: Duration) -> StorageResult<Self> {
        let ms = duration.as_millis();
        if ms == 0 {
            return Err(StorageError::invalid_request(
                "request deadline timeout must be > 0",
            ));
        }
        if ms > u64::MAX as u128 {
            return Err(StorageError::invalid_request(
                "request deadline timeout exceeds u64 millis",
            ));
        }
        Ok(Self::Timeout(DurationMillis(ms as u64)))
    }

    pub fn absolute_unix_ms(ms: u64) -> StorageResult<Self> {
        if ms == 0 {
            return Err(StorageError::invalid_request(
                "absolute deadline must be > 0 unix-ms",
            ));
        }
        Ok(Self::AbsoluteUnixMs(ms))
    }

    pub fn validate(&self) -> StorageResult<()> {
        match self {
            Self::Timeout(DurationMillis(0)) => Err(StorageError::invalid_request(
                "request deadline timeout must be > 0",
            )),
            Self::AbsoluteUnixMs(0) => Err(StorageError::invalid_request(
                "absolute deadline must be > 0 unix-ms",
            )),
            _ => Ok(()),
        }
    }
}

/// Payload size and item count ceilings for request and response bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadLimits {
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub max_items: u32,
}

impl PayloadLimits {
    pub const DEFAULT_MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
    pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
    pub const DEFAULT_MAX_ITEMS: u32 = 1_000;
    /// Hard ceiling so absurd limits fail closed at construction.
    pub const ABSURD_MAX_BYTES: u64 = 512 * 1024 * 1024;
    pub const ABSURD_MAX_ITEMS: u32 = 1_000_000;

    pub fn defaults() -> Self {
        Self {
            max_request_bytes: Self::DEFAULT_MAX_REQUEST_BYTES,
            max_response_bytes: Self::DEFAULT_MAX_RESPONSE_BYTES,
            max_items: Self::DEFAULT_MAX_ITEMS,
        }
    }

    pub fn new(
        max_request_bytes: u64,
        max_response_bytes: u64,
        max_items: u32,
    ) -> StorageResult<Self> {
        let limits = Self {
            max_request_bytes,
            max_response_bytes,
            max_items,
        };
        limits.validate()?;
        Ok(limits)
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.max_request_bytes == 0 {
            return Err(StorageError::invalid_request(
                "max_request_bytes must be > 0",
            ));
        }
        if self.max_response_bytes == 0 {
            return Err(StorageError::invalid_request(
                "max_response_bytes must be > 0",
            ));
        }
        if self.max_items == 0 {
            return Err(StorageError::invalid_request("max_items must be > 0"));
        }
        if self.max_request_bytes > Self::ABSURD_MAX_BYTES {
            return Err(StorageError::invalid_request(format!(
                "max_request_bytes {} exceeds absurd ceiling {}",
                self.max_request_bytes,
                Self::ABSURD_MAX_BYTES
            )));
        }
        if self.max_response_bytes > Self::ABSURD_MAX_BYTES {
            return Err(StorageError::invalid_request(format!(
                "max_response_bytes {} exceeds absurd ceiling {}",
                self.max_response_bytes,
                Self::ABSURD_MAX_BYTES
            )));
        }
        if self.max_items > Self::ABSURD_MAX_ITEMS {
            return Err(StorageError::invalid_request(format!(
                "max_items {} exceeds absurd ceiling {}",
                self.max_items,
                Self::ABSURD_MAX_ITEMS
            )));
        }
        Ok(())
    }
}

/// In-flight concurrency ceiling for a client or call class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrencyBound {
    pub max_in_flight: u32,
}

impl ConcurrencyBound {
    pub fn new(max_in_flight: u32) -> StorageResult<Self> {
        if max_in_flight == 0 {
            return Err(StorageError::invalid_request(
                "max_in_flight concurrency must be > 0",
            ));
        }
        if max_in_flight > 10_000 {
            return Err(StorageError::invalid_request(
                "max_in_flight concurrency exceeds absurd ceiling 10000",
            ));
        }
        Ok(Self { max_in_flight })
    }

    pub fn validate(self) -> StorageResult<()> {
        Self::new(self.max_in_flight).map(|_| ())
    }
}

/// Outbound queue depth bound before the client must shed load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueBound {
    pub max_queued: u32,
}

impl QueueBound {
    pub fn new(max_queued: u32) -> StorageResult<Self> {
        if max_queued == 0 {
            return Err(StorageError::invalid_request(
                "max_queued queue bound must be > 0",
            ));
        }
        if max_queued > 100_000 {
            return Err(StorageError::invalid_request(
                "max_queued exceeds absurd ceiling 100000",
            ));
        }
        Ok(Self { max_queued })
    }

    pub fn validate(self) -> StorageResult<()> {
        Self::new(self.max_queued).map(|_| ())
    }
}

/// Cooperative cancellation token identity (no runtime waiter).
///
/// Transport adapters map this onto their cancellation primitive. The contract
/// only carries a non-empty token id so hops can correlate cancel signals.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CancellationToken {
    pub token_id: String,
    pub cancelled: bool,
}

impl CancellationToken {
    pub fn new(token_id: impl Into<String>) -> StorageResult<Self> {
        let token_id = token_id.into();
        if token_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "cancellation token_id must not be empty",
            ));
        }
        Ok(Self {
            token_id,
            cancelled: false,
        })
    }

    pub fn cancel(mut self) -> Self {
        self.cancelled = true;
        self
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.token_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "cancellation token_id must not be empty",
            ));
        }
        Ok(())
    }

    pub fn check_not_cancelled(&self) -> StorageResult<()> {
        self.validate()?;
        if self.cancelled {
            return Err(StorageError::timeout("cancelled"));
        }
        Ok(())
    }
}

/// Aggregate bounds attached to every remote request envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBounds {
    pub deadline: RequestDeadline,
    pub payload: PayloadLimits,
    pub concurrency: ConcurrencyBound,
    pub queue: QueueBound,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<CancellationToken>,
}

impl RequestBounds {
    pub fn new(
        deadline: RequestDeadline,
        payload: PayloadLimits,
        concurrency: ConcurrencyBound,
        queue: QueueBound,
    ) -> StorageResult<Self> {
        let bounds = Self {
            deadline,
            payload,
            concurrency,
            queue,
            cancellation: None,
        };
        bounds.validate()?;
        Ok(bounds)
    }

    pub fn with_cancellation(mut self, token: CancellationToken) -> StorageResult<Self> {
        token.validate()?;
        self.cancellation = Some(token);
        Ok(self)
    }

    pub fn validate(&self) -> StorageResult<()> {
        self.deadline.validate()?;
        self.payload.validate()?;
        self.concurrency.validate()?;
        self.queue.validate()?;
        if let Some(token) = &self.cancellation {
            token.validate()?;
        }
        Ok(())
    }

    /// Defaults suitable for unit tests and small local fixtures.
    pub fn test_defaults() -> Self {
        Self {
            deadline: RequestDeadline::Timeout(DurationMillis(5_000)),
            payload: PayloadLimits::defaults(),
            concurrency: ConcurrencyBound { max_in_flight: 8 },
            queue: QueueBound { max_queued: 64 },
            cancellation: None,
        }
    }
}
