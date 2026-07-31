use std::collections::HashSet;

use crate::retrieval_telemetry::{CandidateCounters, TelemetryResult};
use crate::types::SourceId;

pub(super) fn single_source_filter(source_filter: Option<&HashSet<SourceId>>) -> Option<&SourceId> {
    let source_ids = source_filter?;
    if source_ids.len() == 1 {
        source_ids.iter().next()
    } else {
        None
    }
}

pub(super) fn source_filter_excludes(
    source_filter: Option<&HashSet<SourceId>>,
    source_id: &SourceId,
    candidate_counters: &mut CandidateCounters,
) -> TelemetryResult<bool> {
    let excluded = source_filter.is_some_and(|source_ids| !source_ids.contains(source_id));
    candidate_counters.add_filtered(u64::from(excluded))?;
    Ok(excluded)
}

pub(super) fn source_filter_scope(source_filter: Option<&HashSet<SourceId>>) -> String {
    match source_filter {
        Some(source_ids) if source_ids.len() == 1 => source_ids
            .iter()
            .next()
            .map(|source_id| format!(" for source '{}'", source_id.0))
            .unwrap_or_default(),
        Some(source_ids) => format!(" for {} selected sources", source_ids.len()),
        None => String::new(),
    }
}

pub(super) fn source_filter_ingest_hint(source_filter: Option<&HashSet<SourceId>>) -> String {
    match source_filter {
        Some(source_ids) if source_ids.len() == 1 => source_ids
            .iter()
            .next()
            .map(|source_id| format!(" {}", source_id.0))
            .unwrap_or_default(),
        _ => String::new(),
    }
}
