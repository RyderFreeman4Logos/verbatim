//! Ranked search vs exhaustive enumeration page envelopes.

use serde::{Deserialize, Serialize};

use crate::index_publication::{QueryPublicationBinding, QueryPublicationBindingKind};
use crate::storage_ports::{PageCursor, StorageError, StorageGeneration, StorageResult};

/// Default limit for first-page snapshot requests when callers omit one.
pub const DEFAULT_SNAPSHOT_PAGE_LIMIT: u32 = 50;
/// Hard ceiling for a single snapshot page.
pub const MAX_SNAPSHOT_PAGE_LIMIT: u32 = 1_000;

/// Pagination mode. Ranked search and exhaustive enumeration are **not**
/// interchangeable: a cursor sealed under one mode must not continue the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaginationMode {
    /// Ordinary relevance / score ordered search (may be approximate).
    RankedSearch,
    /// Exhaustive keyset/id enumeration over a fixed generation snapshot.
    ExhaustiveEnumeration,
}

impl PaginationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RankedSearch => "ranked_search",
            Self::ExhaustiveEnumeration => "exhaustive_enumeration",
        }
    }
}

/// Field bundle for constructing a first-page or continuation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPageRequestFields {
    pub mode: PaginationMode,
    pub limit: u32,
    pub query_plan_hash: String,
    pub principal: String,
    pub publication_generation: StorageGeneration,
    pub profile_ref: String,
    pub policy_version: String,
    /// Opaque continuation token from a prior response, when paging.
    pub cursor: Option<PageCursor>,
    /// Optional pointer epoch observed when the page binding was created.
    pub pointer_epoch: Option<crate::index_publication::PointerEpoch>,
}

/// Snapshot-bound page request envelope.
///
/// First pages carry the full binding context. Continuations must present the
/// same binding context **and** a sealed cursor; adapters reject mismatches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPageRequest {
    pub mode: PaginationMode,
    pub limit: u32,
    pub query_plan_hash: String,
    pub principal: String,
    pub publication_generation: StorageGeneration,
    pub profile_ref: String,
    pub policy_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer_epoch: Option<crate::index_publication::PointerEpoch>,
}

impl SnapshotPageRequest {
    pub fn new(fields: SnapshotPageRequestFields) -> StorageResult<Self> {
        let req = Self {
            mode: fields.mode,
            limit: fields.limit,
            query_plan_hash: fields.query_plan_hash,
            principal: fields.principal,
            publication_generation: fields.publication_generation,
            profile_ref: fields.profile_ref,
            policy_version: fields.policy_version,
            cursor: fields.cursor,
            pointer_epoch: fields.pointer_epoch,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.limit == 0 {
            return Err(StorageError::invalid_request(
                "snapshot page limit must be > 0",
            ));
        }
        if self.limit > MAX_SNAPSHOT_PAGE_LIMIT {
            return Err(StorageError::invalid_request(format!(
                "snapshot page limit {} exceeds max {MAX_SNAPSHOT_PAGE_LIMIT}",
                self.limit
            )));
        }
        require_non_empty("query_plan_hash", &self.query_plan_hash)?;
        require_non_empty("principal", &self.principal)?;
        require_non_empty("profile_ref", &self.profile_ref)?;
        require_non_empty("policy_version", &self.policy_version)?;
        if let Some(cursor) = &self.cursor {
            if cursor.0.trim().is_empty() {
                return Err(StorageError::invalid_request(
                    "snapshot page cursor must not be empty when set",
                ));
            }
        }
        Ok(())
    }

    /// Build a cursor-kind publication binding for the request generation.
    pub fn publication_binding(
        &self,
        consumer_id: impl Into<String>,
    ) -> StorageResult<QueryPublicationBinding> {
        let mut binding = QueryPublicationBinding::new(
            QueryPublicationBindingKind::Cursor,
            self.publication_generation,
            consumer_id,
        )?;
        if let Some(epoch) = self.pointer_epoch {
            binding = binding.with_pointer_epoch(epoch);
        }
        binding.validate()?;
        Ok(binding)
    }

    pub fn is_continuation(&self) -> bool {
        self.cursor.is_some()
    }
}

/// Snapshot-bound page response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPageResponse<T> {
    pub mode: PaginationMode,
    pub items: Vec<T>,
    /// Opaque sealed cursor for the next page, when more results may exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
    /// Generation the page was served from (must match request binding).
    pub publication_generation: StorageGeneration,
    /// True when the server knows no further items exist under this snapshot.
    pub exhausted: bool,
    /// Optional total when the server truly knows it. Never invent this from a
    /// single page's `items.len()` (last-page length ≠ snapshot total). Prefer
    /// `None` over a false total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_hint: Option<u64>,
}

impl<T> SnapshotPageResponse<T> {
    /// Empty exhausted page with a known total of zero.
    pub fn empty(mode: PaginationMode, publication_generation: StorageGeneration) -> Self {
        Self {
            mode,
            items: Vec::new(),
            next_cursor: None,
            publication_generation,
            exhausted: true,
            total_hint: Some(0),
        }
    }

    /// Build a page envelope.
    ///
    /// `total_hint` must be supplied only when the caller has a known snapshot
    /// total. Exhausted multi-page last pages must pass `None` unless the true
    /// total is known — never derive it from `items.len()`.
    pub fn page(
        mode: PaginationMode,
        publication_generation: StorageGeneration,
        items: Vec<T>,
        next_cursor: Option<PageCursor>,
        exhausted: bool,
        total_hint: Option<u64>,
    ) -> Self {
        Self {
            mode,
            items,
            next_cursor,
            publication_generation,
            exhausted,
            total_hint,
        }
    }
}

fn require_non_empty(field: &str, value: &str) -> StorageResult<()> {
    if value.trim().is_empty() {
        return Err(StorageError::invalid_request(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}
