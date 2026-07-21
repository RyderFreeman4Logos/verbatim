use crate::chunker::{
    deterministic_chunk_hash, estimate_tokens, full_unit_evidence_spans, ChunkOutput,
};
use crate::types::{Chunk, ChunkId, ChunkType, EvidenceUnit, SourceId};

pub(crate) fn chunk_caption_evidence(
    source_id: &SourceId,
    evidence: &[EvidenceUnit],
) -> ChunkOutput {
    let mut chunks = Vec::with_capacity(evidence.len());
    let mut links = Vec::with_capacity(evidence.len());

    for unit in evidence {
        let evidence_hashes = vec![format!("evidence:{}:{}", unit.id.0, unit.text_hash)];
        let chunk_hash = deterministic_chunk_hash(
            ChunkType::Child,
            &unit.text,
            &unit.heading_path,
            &evidence_hashes,
        );
        let chunk_id = ChunkId(format!("{}:chunk:{}", unit.id.0, &chunk_hash[..16]));
        chunks.push(Chunk {
            id: chunk_id.clone(),
            source_id: source_id.clone(),
            chunk_hash,
            embedding_input_hash: None,
            text: unit.text.clone(),
            context_text: None,
            token_count: estimate_tokens(&unit.text),
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: unit.heading_path.clone(),
            evidence_unit_ids: vec![unit.id.clone()],
        });
        links.push((chunk_id, unit.id.clone()));
    }

    let evidence_spans = full_unit_evidence_spans(&chunks, evidence);
    ChunkOutput {
        chunks,
        links,
        evidence_spans,
    }
}
