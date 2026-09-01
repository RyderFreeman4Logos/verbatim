//! Test helpers for `sqlite_fts` (kept in a sibling file so the ratcheted
//! module stays within its no-growth budget; see issue #368).

use crate::index::sqlite_fts::SqliteFtsIndex;
use crate::store::Store;
use crate::traits::LexicalIndex;
use crate::types::{
    Chunk, ChunkId, ChunkType, EvidenceId, EvidenceKind, EvidenceUnit, Source, SourceId,
    SourceLocator, SourceStatus,
};
use std::path::PathBuf;

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
        annotations: Default::default(),
    }
}

#[test]
fn filtered_fts_fails_closed_at_sqlite_variable_limit() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("src-variable-limit".into()),
        path: PathBuf::from("/tmp/src-variable-limit.txt"),
        hash: "hash-src-variable-limit".into(),
        status: SourceStatus::Indexed,
        parser_used: Some("plaintext".into()),
        last_ingested_at: None,
    };
    let evidence = evidence(&source.id, "ev-variable-limit");
    let chunk = Chunk {
        id: ChunkId("chunk-variable-limit".into()),
        source_id: source.id.clone(),
        chunk_hash: "hash-chunk-variable-limit".into(),
        embedding_input_hash: None,
        text: "alpha content".into(),
        context_text: None,
        token_count: 2,
        chunk_type: ChunkType::Child,
        parent_chunk_id: None,
        heading_path: vec!["Heading".into()],
        evidence_unit_ids: vec![evidence.id.clone()],
    };
    store.add_source(&source).unwrap();
    store.bulk_insert_evidence(&[evidence]).unwrap();
    store
        .bulk_insert_chunks(std::slice::from_ref(&chunk))
        .unwrap();
    let index = SqliteFtsIndex::new(&store);
    let variable_limit = store
        .connection()
        .limit(rusqlite::limits::Limit::SQLITE_LIMIT_VARIABLE_NUMBER)
        .unwrap() as usize;
    assert!(variable_limit > 2);
    let max_source_ids = variable_limit - 2;
    let mut source_filter = std::collections::HashSet::with_capacity(max_source_ids + 1);
    source_filter.insert(source.id.clone());
    for index in 0..(max_source_ids - 1) {
        source_filter.insert(SourceId(format!("src-filter-{index}")));
    }
    assert_eq!(source_filter.len(), max_source_ids);

    let results = index
        .search_filtered("alpha", 1, Some(&source_filter))
        .unwrap();
    assert_eq!(results[0].0 .0, "chunk-variable-limit");

    source_filter.insert(SourceId("src-filter-over-limit".into()));
    let error = index
        .search_filtered("alpha", 1, Some(&source_filter))
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<crate::overfetch::OverfetchError>(),
        Some(&crate::overfetch::OverfetchError::UnsupportedStrictFilter)
    );
}
