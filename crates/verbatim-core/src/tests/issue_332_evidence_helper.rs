//! Test helper for issue_332 relocation tests (kept in a sibling file so the
//! ratcheted test module stays within its 800-line no-growth budget).

use crate::index::hnsw::HnswIndex;
use crate::store::Store;
use crate::types::{EvidenceId, EvidenceKind, EvidenceUnit, SourceId, SourceLocator};
use std::fs;

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

#[tokio::test]
async fn issue_615_relocation_preserves_verse_evidence() {
    let tempdir = tempfile::tempdir().unwrap();
    let old_path = tempdir.path().join("verse.usfm");
    fs::write(
        &old_path,
        "\\id JHN\n\\c 3\n\\v 16 For God so loved the world.\n",
    )
    .unwrap();
    let store = Store::in_memory().unwrap();
    let mut pipeline = crate::ingest::IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        super::RelocationEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&old_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let before = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].kind, EvidenceKind::Verse);
    let locator = before[0].locator.clone();

    let new_path = tempdir.path().join("renamed.usfm");
    fs::rename(&old_path, &new_path).unwrap();
    pipeline.relocate_source(&source_id, &new_path).unwrap();

    let after = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].kind, EvidenceKind::Verse);
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[0].locator, locator);
}
