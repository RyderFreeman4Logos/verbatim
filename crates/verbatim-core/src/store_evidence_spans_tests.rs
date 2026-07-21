use super::tests::{
    profile_id, sample_chunks, sample_evidence, sample_source, test_profile_config,
};
use super::*;
use crate::chunker::CHUNKER_VERSION;
use crate::evidence_spans::{ChunkEvidenceSpan, EvidenceSpanTrust};
use crate::traits::VectorDocument;

#[test]
fn chunker_version_change_quarantines_old_profile_vectors() {
    let store = Store::in_memory().unwrap();
    let profile = profile_id("chunker-migration");
    let source = sample_source();
    let chunk_id = ChunkId("child-1".into());
    let old_config = EmbeddingProfileConfig {
        chunker_version: "parent-child-v2",
        ..test_profile_config("test", "model", 2, true, "", "")
    };
    let new_config = EmbeddingProfileConfig {
        chunker_version: CHUNKER_VERSION,
        ..test_profile_config("test", "model", 2, true, "", "")
    };

    store
        .ensure_embedding_profile(&profile, old_config)
        .unwrap();
    store.add_source(&source).unwrap();
    store
        .bulk_insert_chunks(&sample_chunks(&source.id.0))
        .unwrap();
    store
        .replace_all_vector_documents_for_profile(
            &profile,
            &[VectorDocument {
                chunk_id: chunk_id.clone(),
                source_id: source.id.clone(),
                vector: vec![1.0, 0.0],
            }],
        )
        .unwrap();
    store
        .set_source_embedding_status(
            &profile,
            &source.id,
            SourceEmbeddingStatus::Embedded,
            1,
            None,
        )
        .unwrap();

    assert!(store
        .ensure_embedding_profile(&profile, new_config)
        .unwrap());
    assert!(store
        .list_vector_documents_for_profile(&profile)
        .unwrap()
        .is_empty());
    assert!(store
        .source_vectors_stale_for_profile(&profile, &source.id)
        .unwrap());
    assert_eq!(
        store
            .load_embedding_profile_config(&profile)
            .unwrap()
            .expect("profile is retained for reindex")
            .chunker_version,
        CHUNKER_VERSION
    );
}

#[test]
fn replace_source_contents_persists_resolvable_evidence_spans() {
    let store = Store::in_memory().unwrap();
    let profile = profile_id("span-provenance");
    let source = sample_source();
    let evidence = sample_evidence(&source.id.0);
    let chunks = sample_chunks(&source.id.0);
    let child_id = ChunkId("child-1".into());
    let span = ChunkEvidenceSpan {
        chunk_id: child_id.clone(),
        evidence_id: evidence[0].id.clone(),
        chunk_byte_start: 0,
        chunk_byte_end: 5,
        evidence_byte_start: 0,
        evidence_byte_end: 5,
        evidence_text_hash: evidence[0].text_hash.clone(),
        locator: evidence[0].locator.clone(),
        trust: EvidenceSpanTrust::Direct,
    };

    store
        .ensure_embedding_profile(
            &profile,
            test_profile_config("test", "span-model", 2, true, "", ""),
        )
        .unwrap();
    store
        .replace_source_contents(SourceContentsReplacement {
            source: &source,
            evidence: &evidence,
            chunks: &chunks,
            embedding_profile_id: &profile,
            vectors: &[],
            links: &[
                (ChunkId("parent-1".into()), evidence[0].id.clone()),
                (ChunkId("parent-1".into()), evidence[1].id.clone()),
                (child_id.clone(), evidence[0].id.clone()),
            ],
            evidence_spans: std::slice::from_ref(&span),
            image_artifacts: &[],
            graph_nodes: &[],
            graph_edges: &[],
        })
        .unwrap();

    assert_eq!(
        store.list_chunk_evidence_spans(&child_id).unwrap(),
        vec![span]
    );
}
