//! Indexed-count boundary that forbids count-via-materialization.

use super::{OverfetchResult, RetrievalFilters};

/// Metadata-count adapter for normal retrieval planning.
///
/// Implementations must use an indexed `COUNT(*)` or equivalent metadata count.
/// They must not derive a count by materializing chunks, text, evidence links, or
/// any `list_all()?.len()` equivalent.
pub trait CountPort {
    fn count_indexed(&self, filters: &RetrievalFilters) -> OverfetchResult<u64>;
}
