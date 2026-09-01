//! Test helper for issue_332 relocation tests (kept in a sibling file so the
//! ratcheted test module stays within its 800-line no-growth budget).

use crate::types::{EvidenceId, EvidenceKind, EvidenceUnit, SourceId, SourceLocator};

pub(super) fn synthetic_evidence(id: &str, source_id: &SourceId, position: u32) -> EvidenceUnit {
    EvidenceUnit {
        id: EvidenceId(id.into()),
        source_id: source_id.clone(),
        kind: EvidenceKind::Text,
        derived_from: None,
        locator: SourceLocator::Document {
            path_or_url: "/tmp/parser.txt".into(),
            line_start: position + 1,
            line_end: None,
        },
        text: format!("evidence {position}"),
        text_hash: format!("hash-{position}"),
        heading_path: Vec::new(),
        language: None,
        position,
        annotations: Default::default(),
    }
}
