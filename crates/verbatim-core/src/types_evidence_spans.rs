use serde::{Deserialize, Serialize};

use crate::types::{ChunkId, EvidenceId, SourceLocator};

/// Trust classification for the source evidence represented by a chunk span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSpanTrust {
    Direct,
    Derived,
}

/// An exact byte range in a chunk that is backed by persisted evidence.
///
/// The paired chunk/evidence ranges use UTF-8 byte offsets. `evidence_text_hash`
/// and `locator` retain the identity needed to resolve the range even after the
/// source has been reloaded for retrieval or citation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkEvidenceSpan {
    pub chunk_id: ChunkId,
    pub evidence_id: EvidenceId,
    pub chunk_byte_start: u64,
    pub chunk_byte_end: u64,
    pub evidence_byte_start: u64,
    pub evidence_byte_end: u64,
    pub evidence_text_hash: String,
    pub locator: SourceLocator,
    pub trust: EvidenceSpanTrust,
}
