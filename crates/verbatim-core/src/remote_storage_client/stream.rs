//! Streaming/range-read and bounded pagination request shapes (contract only).

use serde::{Deserialize, Serialize};

use crate::storage_ports::{PageCursor, PageRequest, StorageError, StorageResult};

/// Default page limit when callers omit an explicit value.
pub const DEFAULT_PAGE_LIMIT: u32 = 50;
/// Hard page limit ceiling for remote list/search.
pub const MAX_PAGE_LIMIT: u32 = 1_000;
/// Hard ceiling for a single range-read window.
pub const MAX_RANGE_BYTES: u64 = 32 * 1024 * 1024;
/// Hard ceiling for a stream chunk hint.
pub const MAX_STREAM_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// Bounded pagination request for remote evidence/search lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedPageRequest {
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
}

impl BoundedPageRequest {
    pub fn new(limit: u32) -> StorageResult<Self> {
        let req = Self {
            limit,
            cursor: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn with_cursor(mut self, cursor: PageCursor) -> StorageResult<Self> {
        if cursor.0.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "page cursor must not be empty",
            ));
        }
        self.cursor = Some(cursor);
        Ok(self)
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.limit == 0 {
            return Err(StorageError::invalid_request(
                "bounded page limit must be > 0",
            ));
        }
        if self.limit > MAX_PAGE_LIMIT {
            return Err(StorageError::invalid_request(format!(
                "bounded page limit {} exceeds max {MAX_PAGE_LIMIT}",
                self.limit
            )));
        }
        if let Some(cursor) = &self.cursor {
            if cursor.0.trim().is_empty() {
                return Err(StorageError::invalid_request(
                    "page cursor must not be empty",
                ));
            }
        }
        Ok(())
    }

    /// Convert to the storage-port [`PageRequest`].
    pub fn to_page_request(&self) -> StorageResult<PageRequest> {
        self.validate()?;
        let mut page = PageRequest::new(self.limit)?;
        if let Some(cursor) = &self.cursor {
            page = page.with_cursor(cursor.clone());
        }
        Ok(page)
    }
}

impl Default for BoundedPageRequest {
    fn default() -> Self {
        Self {
            limit: DEFAULT_PAGE_LIMIT,
            cursor: None,
        }
    }
}

/// Inclusive-start / exclusive-end byte range for blob reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeReadRequest {
    pub start_byte: u64,
    /// Exclusive end; must be > start_byte.
    pub end_byte: u64,
}

impl RangeReadRequest {
    pub fn new(start_byte: u64, end_byte: u64) -> StorageResult<Self> {
        let range = Self {
            start_byte,
            end_byte,
        };
        range.validate()?;
        Ok(range)
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.end_byte <= self.start_byte {
            return Err(StorageError::invalid_request(
                "range end_byte must be > start_byte",
            ));
        }
        let len = self.end_byte - self.start_byte;
        if len > MAX_RANGE_BYTES {
            return Err(StorageError::invalid_request(format!(
                "range length {len} exceeds max {MAX_RANGE_BYTES}"
            )));
        }
        Ok(())
    }

    pub fn len_bytes(&self) -> u64 {
        self.end_byte.saturating_sub(self.start_byte)
    }
}

/// Preferred stream chunk size for transport adapters (hint only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunkHint {
    pub max_chunk_bytes: u64,
}

impl StreamChunkHint {
    pub fn new(max_chunk_bytes: u64) -> StorageResult<Self> {
        if max_chunk_bytes == 0 {
            return Err(StorageError::invalid_request(
                "stream chunk hint must be > 0",
            ));
        }
        if max_chunk_bytes > MAX_STREAM_CHUNK_BYTES {
            return Err(StorageError::invalid_request(format!(
                "stream chunk hint {max_chunk_bytes} exceeds max {MAX_STREAM_CHUNK_BYTES}"
            )));
        }
        Ok(Self { max_chunk_bytes })
    }
}

/// Streaming blob/read request shape (no live stream body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamReadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeReadRequest>,
    pub chunk_hint: StreamChunkHint,
}

impl StreamReadRequest {
    pub fn full(chunk_hint: StreamChunkHint) -> StorageResult<Self> {
        chunk_hint
            .max_chunk_bytes
            .checked_mul(1)
            .ok_or_else(|| StorageError::invalid_request("invalid chunk hint"))?;
        let req = Self {
            range: None,
            chunk_hint,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn ranged(range: RangeReadRequest, chunk_hint: StreamChunkHint) -> StorageResult<Self> {
        let req = Self {
            range: Some(range),
            chunk_hint,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> StorageResult<()> {
        StreamChunkHint::new(self.chunk_hint.max_chunk_bytes)?;
        if let Some(range) = &self.range {
            range.validate()?;
        }
        Ok(())
    }
}
