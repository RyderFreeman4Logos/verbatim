use super::*;
use crate::store::EmbeddingCacheEntry;
use crate::types::{
    Chunk, ChunkId, ChunkType, EvidenceId, EvidenceKind, EvidenceUnit, ImageArtifact, ImageId,
    Source, SourceLocator, SourceStatus,
};
use crate::vision_caption::{
    CaptionAttempt, ImageCaption, ImageCaptionContentType, VISION_CAPTION_PROMPT_VERSION,
};

#[test]
fn sole_source_erasure_purges_unreferenced_embedding_and_caption_caches() {
    let store = Store::in_memory().unwrap();
    let profile = EmbeddingProfileId::default_profile();
    let source = Source {
        id: SourceId("sole-cache-source".into()),
        path: std::path::PathBuf::from("sole-cache-source.md"),
        hash: "sole-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    seed_source_with_caches(
        &store,
        &source,
        "child-sole",
        "ev-sole",
        "img-sole",
        "shared-input-hash",
        "shared-image-hash",
        &profile,
        "config-a",
    );

    store.remove_source(&source.id).unwrap();

    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "shared-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("shared-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
    assert_eq!(embedding_cache_row_count(&store), 0);
    assert!(store.list_image_captions().unwrap().is_empty());
}

#[test]
fn shared_cache_hashes_survive_until_last_live_source_is_erased() {
    let store = Store::in_memory().unwrap();
    let profile = EmbeddingProfileId::default_profile();
    let first = Source {
        id: SourceId("shared-cache-first".into()),
        path: std::path::PathBuf::from("shared-cache-first.md"),
        hash: "first-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    let second = Source {
        id: SourceId("shared-cache-second".into()),
        path: std::path::PathBuf::from("shared-cache-second.md"),
        hash: "second-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    seed_source_with_caches(
        &store,
        &first,
        "child-first",
        "ev-first",
        "img-first",
        "shared-input-hash",
        "shared-image-hash",
        &profile,
        "config-a",
    );
    seed_source_with_caches(
        &store,
        &second,
        "child-second",
        "ev-second",
        "img-second",
        "shared-input-hash",
        "shared-image-hash",
        &profile,
        "config-a",
    );

    store.remove_source(&first.id).unwrap();
    assert_eq!(
        store
            .get_embedding_cache_vector(&profile, "config-a", "shared-input-hash")
            .unwrap()
            .unwrap(),
        vec![0.25, 0.75]
    );
    assert!(store
        .get_image_caption("shared-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_some());
    assert_eq!(embedding_cache_row_count(&store), 1);
    assert_eq!(store.list_image_captions().unwrap().len(), 1);

    store.remove_source(&second.id).unwrap();
    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "shared-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("shared-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
    assert_eq!(embedding_cache_row_count(&store), 0);
    assert!(store.list_image_captions().unwrap().is_empty());
}

#[test]
fn sole_source_erasure_prevents_cache_hit_resurrection_for_reseeded_source() {
    let store = Store::in_memory().unwrap();
    let profile = EmbeddingProfileId::default_profile();
    let source = Source {
        id: SourceId("cache-resurrection-source".into()),
        path: std::path::PathBuf::from("cache-resurrection-source.md"),
        hash: "resurrection-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    seed_source_with_caches(
        &store,
        &source,
        "child-resurrection",
        "ev-resurrection",
        "img-resurrection",
        "resurrection-input-hash",
        "resurrection-image-hash",
        &profile,
        "config-a",
    );

    store.remove_source(&source.id).unwrap();
    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "resurrection-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("resurrection-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());

    // Same content hashes under a brand-new source id must not see deleted cache rows.
    // Seed live rows only; do not re-write content-addressed cache entries.
    let reseeded = Source {
        id: SourceId("cache-resurrection-source-v2".into()),
        path: std::path::PathBuf::from("cache-resurrection-source-v2.md"),
        hash: "resurrection-hash-v2".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&reseeded).unwrap();
    store
        .bulk_insert_evidence(&[EvidenceUnit {
            id: EvidenceId("ev-resurrection-v2".into()),
            source_id: reseeded.id.clone(),
            kind: EvidenceKind::Image,
            derived_from: None,
            locator: SourceLocator::Pdf {
                page: 1,
                paragraph: 1,
                bbox: None,
            },
            text: "image evidence for reseeded source".into(),
            text_hash: "hash-ev-resurrection-v2".into(),
            heading_path: vec!["Images".into()],
            position: 0,
        }])
        .unwrap();
    store
        .bulk_insert_chunks(&[Chunk {
            id: ChunkId("child-resurrection-v2".into()),
            source_id: reseeded.id.clone(),
            chunk_hash: "chunk-hash-child-resurrection-v2".into(),
            embedding_input_hash: Some("resurrection-input-hash".into()),
            text: "chunk text for reseeded source".into(),
            context_text: None,
            token_count: 8,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Images".into()],
            evidence_unit_ids: vec![EvidenceId("ev-resurrection-v2".into())],
        }])
        .unwrap();
    store
        .bulk_insert_image_artifacts(&[ImageArtifact {
            image_id: ImageId("img-resurrection-v2".into()),
            source_id: reseeded.id.clone(),
            evidence_id: EvidenceId("ev-resurrection-v2".into()),
            relative_path: std::path::PathBuf::from("images/img-resurrection-v2.png"),
            content_hash: "resurrection-image-hash".into(),
            mime_type: "image/png".into(),
            width: 16,
            height: 8,
            page: 1,
            image_index: 1,
            bbox: None,
        }])
        .unwrap();

    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "resurrection-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("resurrection-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
    assert_eq!(embedding_cache_row_count(&store), 0);
    assert!(store.list_image_captions().unwrap().is_empty());
}

fn seed_source_with_caches(
    store: &Store,
    source: &Source,
    chunk_id: &str,
    evidence_id: &str,
    image_id: &str,
    embedding_input_hash: &str,
    image_hash: &str,
    profile: &EmbeddingProfileId,
    profile_config_hash: &str,
) {
    store.add_source(source).unwrap();
    store
        .bulk_insert_evidence(&[EvidenceUnit {
            id: EvidenceId(evidence_id.into()),
            source_id: source.id.clone(),
            kind: EvidenceKind::Image,
            derived_from: None,
            locator: SourceLocator::Pdf {
                page: 1,
                paragraph: 1,
                bbox: None,
            },
            text: format!("image evidence for {}", source.id.0),
            text_hash: format!("hash-{evidence_id}"),
            heading_path: vec!["Images".into()],
            position: 0,
        }])
        .unwrap();
    store
        .bulk_insert_chunks(&[Chunk {
            id: ChunkId(chunk_id.into()),
            source_id: source.id.clone(),
            chunk_hash: format!("chunk-hash-{chunk_id}"),
            embedding_input_hash: Some(embedding_input_hash.into()),
            text: format!("chunk text for {}", source.id.0),
            context_text: None,
            token_count: 8,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Images".into()],
            evidence_unit_ids: vec![EvidenceId(evidence_id.into())],
        }])
        .unwrap();
    store
        .bulk_insert_image_artifacts(&[ImageArtifact {
            image_id: ImageId(image_id.into()),
            source_id: source.id.clone(),
            evidence_id: EvidenceId(evidence_id.into()),
            relative_path: std::path::PathBuf::from(format!("images/{image_id}.png")),
            content_hash: image_hash.into(),
            mime_type: "image/png".into(),
            width: 16,
            height: 8,
            page: 1,
            image_index: 1,
            bbox: None,
        }])
        .unwrap();
    // Content-addressed caches may already exist from another live source; only insert once.
    if store
        .get_embedding_cache_vector(profile, profile_config_hash, embedding_input_hash)
        .unwrap()
        .is_none()
    {
        store
            .upsert_embedding_cache_entries(
                profile,
                profile_config_hash,
                &[EmbeddingCacheEntry {
                    embedding_input_hash: embedding_input_hash.into(),
                    vector: vec![0.25, 0.75],
                }],
            )
            .unwrap();
    }
    if store
        .get_image_caption(image_hash, "vision-test", "prompt-hash")
        .unwrap()
        .is_none()
    {
        let caption = ImageCaption {
            content_type: ImageCaptionContentType::Other,
            short_caption: "shared caption".into(),
            detailed_description: "shared detailed caption".into(),
            visible_text: vec![],
            key_entities: vec![],
            relationships: vec![],
            answerable_questions: vec![],
            uncertainties: vec![],
        };
        store
            .upsert_image_caption_attempt(
                image_hash,
                "vision-test",
                VISION_CAPTION_PROMPT_VERSION,
                "prompt-hash",
                &CaptionAttempt::success(caption, r#"{"ok":true}"#.into(), 1),
            )
            .unwrap();
    }
}

fn embedding_cache_row_count(store: &Store) -> i64 {
    store
        .connection()
        .query_row("SELECT COUNT(*) FROM embedding_cache", [], |row| row.get(0))
        .unwrap()
}
