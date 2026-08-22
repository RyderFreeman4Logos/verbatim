use std::collections::{HashMap, HashSet};

use anyhow::Result;
use verbatim_core::retrieve::refresh_evidence_pack_debug;
use verbatim_core::store::Store;
use verbatim_core::types::{ChunkId, EvidenceKind, RetrievalDebug, RetrievalResult};

pub(super) fn filter_generated_retrieval_evidence(
    store: &Store,
    results: &mut Vec<RetrievalResult>,
    mut debug: Option<&mut RetrievalDebug>,
    include_debug: bool,
) -> Result<()> {
    let has_debug_identities = include_debug
        && debug
            .as_ref()
            .is_some_and(|debug| retrieval_debug_has_chunk_identities(debug));
    if results.is_empty() && !has_debug_identities {
        return Ok(());
    }

    let source_bounded_chunk_ids = source_bounded_retrieval_chunk_ids(
        store,
        results,
        if has_debug_identities {
            debug.as_deref()
        } else {
            None
        },
    )?;
    let result_count = results.len();
    results.retain(|result| {
        let directly_source_bounded = result
            .evidence_units
            .iter()
            .all(|evidence| matches!(evidence.kind, EvidenceKind::Text | EvidenceKind::Image));
        let persisted_source_bounded = source_bounded_chunk_ids.contains(&result.chunk_id)
            && result
                .provenance
                .seed_chunk_id
                .as_ref()
                .is_none_or(|seed_chunk_id| source_bounded_chunk_ids.contains(seed_chunk_id));
        directly_source_bounded && persisted_source_bounded
    });
    let results_changed = results.len() != result_count;
    if results_changed {
        for (index, result) in results.iter_mut().enumerate() {
            result.provenance.result_rank = index + 1;
        }
        if let Some(debug) = debug.as_deref_mut() {
            refresh_evidence_pack_debug(debug, results);
        }
    }

    if has_debug_identities {
        if let Some(debug) = debug {
            filter_retrieval_debug_chunk_identities(
                debug,
                &source_bounded_chunk_ids,
                results_changed,
                results,
            );
        }
    }
    Ok(())
}

fn retrieval_debug_has_chunk_identities(debug: &RetrievalDebug) -> bool {
    !debug.bm25_hits.is_empty()
        || !debug.dense_hits.is_empty()
        || !debug.rrf_fused_hits.is_empty()
        || !debug.graph_expanded_hits.is_empty()
        || !debug.reranker.scores.is_empty()
}

fn source_bounded_retrieval_chunk_ids(
    store: &Store,
    results: &[RetrievalResult],
    debug: Option<&RetrievalDebug>,
) -> Result<HashSet<ChunkId>> {
    let mut candidate_ids = HashSet::new();
    for result in results {
        candidate_ids.insert(result.chunk_id.clone());
        if let Some(seed_chunk_id) = &result.provenance.seed_chunk_id {
            candidate_ids.insert(seed_chunk_id.clone());
        }
    }
    if let Some(debug) = debug {
        candidate_ids.extend(debug.bm25_hits.iter().map(|hit| hit.chunk_id.clone()));
        candidate_ids.extend(debug.dense_hits.iter().map(|hit| hit.chunk_id.clone()));
        candidate_ids.extend(debug.rrf_fused_hits.iter().map(|hit| hit.chunk_id.clone()));
        for hit in &debug.graph_expanded_hits {
            candidate_ids.insert(hit.seed_chunk_id.clone());
            candidate_ids.insert(hit.expanded_chunk_id.clone());
        }
        candidate_ids.extend(
            debug
                .reranker
                .scores
                .iter()
                .map(|score| score.chunk_id.clone()),
        );
    }

    let candidate_ids = candidate_ids.into_iter().collect::<Vec<_>>();
    let chunks = store.get_chunks(&candidate_ids)?;
    let evidence_ids = chunks
        .values()
        .filter_map(|chunk| chunk.as_ref().ok())
        .flat_map(|chunk| chunk.evidence_unit_ids.iter().cloned())
        .collect::<Vec<_>>();
    let evidence = store.get_evidence_batch(&evidence_ids)?;
    let mut source_bounded_chunk_ids = HashSet::new();
    for (chunk_id, chunk) in chunks {
        let Ok(chunk) = chunk else {
            continue;
        };
        if chunk.evidence_unit_ids.is_empty() {
            continue;
        }
        let source_bounded = chunk.evidence_unit_ids.iter().all(|evidence_id| {
            matches!(
                evidence.get(evidence_id),
                Some(Ok(evidence))
                    if matches!(evidence.kind, EvidenceKind::Text | EvidenceKind::Image)
            )
        });
        if source_bounded {
            source_bounded_chunk_ids.insert(chunk_id);
        }
    }
    Ok(source_bounded_chunk_ids)
}

fn filter_retrieval_debug_chunk_identities(
    debug: &mut RetrievalDebug,
    source_bounded_chunk_ids: &HashSet<ChunkId>,
    results_changed: bool,
    results: &[RetrievalResult],
) {
    debug
        .bm25_hits
        .retain(|hit| source_bounded_chunk_ids.contains(&hit.chunk_id));
    for (index, hit) in debug.bm25_hits.iter_mut().enumerate() {
        hit.rank = index + 1;
    }
    debug
        .dense_hits
        .retain(|hit| source_bounded_chunk_ids.contains(&hit.chunk_id));
    for (index, hit) in debug.dense_hits.iter_mut().enumerate() {
        hit.rank = index + 1;
    }
    let dense_ranks = debug
        .dense_hits
        .iter()
        .map(|hit| (hit.chunk_id.clone(), hit.rank))
        .collect::<HashMap<_, _>>();
    let bm25_ranks = debug
        .bm25_hits
        .iter()
        .map(|hit| (hit.chunk_id.clone(), hit.rank))
        .collect::<HashMap<_, _>>();
    debug
        .rrf_fused_hits
        .retain(|hit| source_bounded_chunk_ids.contains(&hit.chunk_id));
    for (index, hit) in debug.rrf_fused_hits.iter_mut().enumerate() {
        hit.rank = index + 1;
        hit.dense_rank = dense_ranks.get(&hit.chunk_id).copied();
        hit.bm25_rank = bm25_ranks.get(&hit.chunk_id).copied();
    }
    debug
        .reranker
        .scores
        .retain(|score| source_bounded_chunk_ids.contains(&score.chunk_id));
    for (index, score) in debug.reranker.scores.iter_mut().enumerate() {
        score.rank = index + 1;
    }

    debug.graph_expanded_hits.retain(|hit| {
        source_bounded_chunk_ids.contains(&hit.seed_chunk_id)
            && source_bounded_chunk_ids.contains(&hit.expanded_chunk_id)
    });
    if results_changed {
        let result_ranks = results
            .iter()
            .map(|result| (result.chunk_id.clone(), result.provenance.result_rank))
            .collect::<HashMap<_, _>>();
        debug.graph_expanded_hits.retain_mut(|hit| {
            let (Some(result_rank), Some(seed_rank)) = (
                result_ranks.get(&hit.expanded_chunk_id),
                result_ranks.get(&hit.seed_chunk_id),
            ) else {
                return false;
            };
            hit.result_rank = *result_rank;
            hit.seed_rank = *seed_rank;
            true
        });
    }
}
