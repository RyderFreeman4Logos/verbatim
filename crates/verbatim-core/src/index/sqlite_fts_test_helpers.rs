//! Test helpers for `sqlite_fts` (kept in a sibling file so the ratcheted
//! module stays within its no-growth budget; see issue #368).

use crate::types::{EvidenceId, EvidenceKind, EvidenceUnit, SourceId, SourceLocator};

pub(super) fn evidence(source_id: &SourceId, id: &str) -> EvidenceUnit {
    EvidenceUnit {
        id: EvidenceId(id.into()),
        source_id: source_id.clone(),
        kind: EvidenceKind::Text,
        derived_from: None,
        locator: SourceLocator::Document {
            path_or_url: source_id.0.clone(),
            line_start: 1,
            line_end: None,
        },
        text: "text".into(),
        text_hash: format!("hash-{id}"),
        heading_path: Vec::new(),
        language: None,
        position: 0,
    }
}
