//! Typed cursor iterator layered on snapshot-bound pagination (API-003 / #354).

use std::future::Future;
use std::pin::Pin;

use crate::pagination::{PaginationMode, SnapshotPageRequest, SnapshotPageResponse};
use crate::storage_ports::{PageCursor, StorageGeneration};

use super::error::{ClientError, ClientResult};

/// Async page fetcher used by [`CursorIterator`].
///
/// Transport adapters implement this by calling the underlying search/list
/// endpoint with a [`SnapshotPageRequest`]. The walking skeleton keeps the
/// trait object-friendly (`Send` futures) without embedding HTTP types.
pub trait CursorPageFetcher<T>: Send + Sync
where
    T: Send + 'static,
{
    /// Fetch one snapshot-bound page for `request`.
    fn fetch_page(
        &self,
        request: SnapshotPageRequest,
    ) -> Pin<Box<dyn Future<Output = ClientResult<SnapshotPageResponse<T>>> + Send + '_>>;
}

/// Typed wrapper that walks `next_cursor` pages under a fixed snapshot binding.
///
/// Fail-closed rules match [`crate::pagination`]: mode, generation, and cursor
/// presence are not silently rewritten. Callers supply the first-page request
/// template; subsequent pages clone binding fields and attach the prior
/// `next_cursor`.
#[derive(Debug, Clone)]
pub struct CursorIterator {
    pub mode: PaginationMode,
    pub limit: u32,
    pub query_plan_hash: String,
    pub principal: String,
    pub publication_generation: StorageGeneration,
    pub profile_ref: String,
    pub policy_version: String,
    pub next_cursor: Option<PageCursor>,
    pub exhausted: bool,
}

impl CursorIterator {
    /// Build an iterator from a validated first-page request template.
    pub fn from_request(request: &SnapshotPageRequest) -> ClientResult<Self> {
        request
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        Ok(Self {
            mode: request.mode,
            limit: request.limit,
            query_plan_hash: request.query_plan_hash.clone(),
            principal: request.principal.clone(),
            publication_generation: request.publication_generation,
            profile_ref: request.profile_ref.clone(),
            policy_version: request.policy_version.clone(),
            next_cursor: request.cursor.clone(),
            exhausted: false,
        })
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    pub fn has_next(&self) -> bool {
        !self.exhausted
    }

    /// Construct the next page request. Returns `None` when exhausted.
    pub fn next_request(&self) -> ClientResult<Option<SnapshotPageRequest>> {
        if self.exhausted {
            return Ok(None);
        }
        let req = SnapshotPageRequest::new(crate::pagination::SnapshotPageRequestFields {
            mode: self.mode,
            limit: self.limit,
            query_plan_hash: self.query_plan_hash.clone(),
            principal: self.principal.clone(),
            publication_generation: self.publication_generation,
            profile_ref: self.profile_ref.clone(),
            policy_version: self.policy_version.clone(),
            cursor: self.next_cursor.clone(),
            pointer_epoch: None,
        })
        .map_err(|err| ClientError::validation(err.to_string()))?;
        Ok(Some(req))
    }

    /// Advance from a successful page response.
    ///
    /// Generation and mode mismatches fail closed. When `exhausted` or
    /// `next_cursor` is absent, the iterator stops.
    pub fn advance_with<T>(&mut self, page: &SnapshotPageResponse<T>) -> ClientResult<()> {
        if page.mode != self.mode {
            return Err(ClientError::pagination(
                "mode_mismatch",
                format!(
                    "iterator mode {} does not match page mode {}",
                    self.mode.as_str(),
                    page.mode.as_str()
                ),
            ));
        }
        if page.publication_generation != self.publication_generation {
            return Err(ClientError::pagination(
                "generation_mismatch",
                format!(
                    "iterator generation {} does not match page generation {}",
                    self.publication_generation, page.publication_generation
                ),
            ));
        }
        if page.exhausted || page.next_cursor.is_none() {
            self.next_cursor = None;
            self.exhausted = true;
            return Ok(());
        }
        self.next_cursor = page.next_cursor.clone();
        self.exhausted = false;
        Ok(())
    }

    /// Fetch the next page via `fetcher` and advance. Returns `None` when done.
    pub async fn next_page<T, F>(
        &mut self,
        fetcher: &F,
    ) -> ClientResult<Option<SnapshotPageResponse<T>>>
    where
        T: Send + 'static,
        F: CursorPageFetcher<T>,
    {
        let Some(request) = self.next_request()? else {
            return Ok(None);
        };
        let page = fetcher.fetch_page(request).await?;
        self.advance_with(&page)?;
        Ok(Some(page))
    }
}
