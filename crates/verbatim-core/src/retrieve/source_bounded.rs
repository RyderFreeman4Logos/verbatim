use std::collections::HashSet;

use anyhow::Result;

use crate::store::Store;
use crate::types::{ChunkId, EvidenceKind};

pub(super) fn filter(store: &Store, fused: &mut Vec<(ChunkId, f32)>) -> Result<()> {
    let candidate_ids = fused
        .iter()
        .map(|(chunk_id, _)| chunk_id.clone())
        .collect::<Vec<_>>();
    let source_bounded_ids = source_bounded_chunk_ids(store, &candidate_ids)?;
    fused.retain(|(chunk_id, _)| source_bounded_ids.contains(chunk_id));
    Ok(())
}

fn source_bounded_chunk_ids(store: &Store, candidate_ids: &[ChunkId]) -> Result<HashSet<ChunkId>> {
    let chunks = store.get_chunks(candidate_ids)?;
    let evidence_ids = chunks
        .values()
        .filter_map(|chunk| chunk.as_ref().ok())
        .flat_map(|chunk| chunk.evidence_unit_ids.iter().cloned())
        .collect::<Vec<_>>();
    let evidence = store.get_evidence_batch(&evidence_ids)?;
    let mut source_bounded = HashSet::new();
    for (chunk_id, chunk) in chunks {
        let Ok(chunk) = chunk else {
            continue;
        };
        if chunk.evidence_unit_ids.is_empty() {
            continue;
        }
        if chunk.evidence_unit_ids.iter().all(|evidence_id| {
            matches!(
                evidence.get(evidence_id),
                Some(Ok(evidence))
                    if matches!(evidence.kind, EvidenceKind::Text | EvidenceKind::Image)
            )
        }) {
            source_bounded.insert(chunk_id);
        }
    }
    Ok(source_bounded)
}
