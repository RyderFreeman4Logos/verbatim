use super::*;
use crate::store::{EmbeddingCacheEntry, SourceContentsReplacement};
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
            locator: SourceLocator::legacy_pdf(1, 1, None),
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

#[test]
fn v1_to_v2_replace_then_erase_purges_historical_v1_caches() {
    let store = Store::in_memory().unwrap();
    let profile = EmbeddingProfileId::default_profile();
    let source = Source {
        id: SourceId("v1-v2-replace-source".into()),
        path: std::path::PathBuf::from("v1-v2-replace-source.md"),
        hash: "v1-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    // V1 content + content-addressed caches.
    seed_source_with_caches(
        &store,
        &source,
        "child-v1",
        "ev-v1",
        "img-v1",
        "v1-input-hash",
        "v1-image-hash",
        &profile,
        "config-a",
    );
    assert_eq!(
        store
            .get_embedding_cache_vector(&profile, "config-a", "v1-input-hash")
            .unwrap()
            .unwrap(),
        vec![0.25, 0.75]
    );
    assert!(store
        .get_image_caption("v1-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_some());

    // V2 replace with different content: CASCADE drops V1 live rows, but leaves
    // historical content-addressed cache rows unless erasure anti-join purges them.
    let v2_source = Source {
        id: source.id.clone(),
        path: source.path.clone(),
        hash: "v2-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    let v2_evidence = EvidenceUnit {
        id: EvidenceId("ev-v2".into()),
        source_id: source.id.clone(),
        kind: EvidenceKind::Image,
        derived_from: None,
        locator: SourceLocator::legacy_pdf(1, 1, None),
        text: "image evidence for v2".into(),
        text_hash: "hash-ev-v2".into(),
        heading_path: vec!["Images".into()],
        position: 0,
    };
    let v2_chunk = Chunk {
        id: ChunkId("child-v2".into()),
        source_id: source.id.clone(),
        chunk_hash: "chunk-hash-child-v2".into(),
        embedding_input_hash: Some("v2-input-hash".into()),
        text: "chunk text for v2".into(),
        context_text: None,
        token_count: 8,
        chunk_type: ChunkType::Child,
        parent_chunk_id: None,
        heading_path: vec!["Images".into()],
        evidence_unit_ids: vec![EvidenceId("ev-v2".into())],
    };
    let v2_image = ImageArtifact {
        image_id: ImageId("img-v2".into()),
        source_id: source.id.clone(),
        evidence_id: EvidenceId("ev-v2".into()),
        relative_path: std::path::PathBuf::from("images/img-v2.png"),
        content_hash: "v2-image-hash".into(),
        mime_type: "image/png".into(),
        width: 16,
        height: 8,
        page: 1,
        image_index: 1,
        bbox: None,
    };
    store
        .replace_source_contents(SourceContentsReplacement {
            source: &v2_source,
            evidence: std::slice::from_ref(&v2_evidence),
            chunks: std::slice::from_ref(&v2_chunk),
            embedding_profile_id: &profile,
            vectors: &[],
            links: &[(ChunkId("child-v2".into()), EvidenceId("ev-v2".into()))],
            evidence_spans: &[],
            image_artifacts: std::slice::from_ref(&v2_image),
            graph_nodes: &[],
            graph_edges: &[],
        })
        .unwrap();
    // Seed V2 caches after replace so the current-source collection path would
    // only see V2 keys at delete time.
    store
        .upsert_embedding_cache_entries(
            &profile,
            "config-a",
            &[EmbeddingCacheEntry {
                embedding_input_hash: "v2-input-hash".into(),
                vector: vec![0.5, 0.5],
            }],
        )
        .unwrap();
    let v2_caption = ImageCaption {
        content_type: ImageCaptionContentType::Other,
        short_caption: "v2 caption".into(),
        detailed_description: "v2 detailed caption".into(),
        visible_text: vec![],
        key_entities: vec![],
        relationships: vec![],
        answerable_questions: vec![],
        uncertainties: vec![],
    };
    store
        .upsert_image_caption_attempt(
            "v2-image-hash",
            "vision-test",
            VISION_CAPTION_PROMPT_VERSION,
            "prompt-hash",
            &CaptionAttempt::success(v2_caption, r#"{"ok":true}"#.into(), 1),
        )
        .unwrap();

    // Historical V1 cache still present after replace (pre-fix orphan risk).
    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "v1-input-hash")
        .unwrap()
        .is_some());
    assert!(store
        .get_image_caption("v1-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_some());

    store.remove_source(&source.id).unwrap();

    // V1 caches must be anti-join purged even though delete only saw V2 live rows.
    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "v1-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("v1-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "v2-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("v2-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
    assert_eq!(embedding_cache_row_count(&store), 0);
    assert!(store.list_image_captions().unwrap().is_empty());

    // A brand-new source reusing V1 content hashes must not hit leftover caches.
    let reseeded = Source {
        id: SourceId("v1-v2-reseeded-source".into()),
        path: std::path::PathBuf::from("v1-v2-reseeded-source.md"),
        hash: "reseed-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&reseeded).unwrap();
    store
        .bulk_insert_evidence(&[EvidenceUnit {
            id: EvidenceId("ev-reseed-v1".into()),
            source_id: reseeded.id.clone(),
            kind: EvidenceKind::Image,
            derived_from: None,
            locator: SourceLocator::legacy_pdf(1, 1, None),
            text: "image evidence reusing v1 content".into(),
            text_hash: "hash-ev-reseed-v1".into(),
            heading_path: vec!["Images".into()],
            position: 0,
        }])
        .unwrap();
    store
        .bulk_insert_chunks(&[Chunk {
            id: ChunkId("child-reseed-v1".into()),
            source_id: reseeded.id.clone(),
            chunk_hash: "chunk-hash-child-reseed-v1".into(),
            embedding_input_hash: Some("v1-input-hash".into()),
            text: "chunk text reusing v1 content".into(),
            context_text: None,
            token_count: 8,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Images".into()],
            evidence_unit_ids: vec![EvidenceId("ev-reseed-v1".into())],
        }])
        .unwrap();
    store
        .bulk_insert_image_artifacts(&[ImageArtifact {
            image_id: ImageId("img-reseed-v1".into()),
            source_id: reseeded.id.clone(),
            evidence_id: EvidenceId("ev-reseed-v1".into()),
            relative_path: std::path::PathBuf::from("images/img-reseed-v1.png"),
            content_hash: "v1-image-hash".into(),
            mime_type: "image/png".into(),
            width: 16,
            height: 8,
            page: 1,
            image_index: 1,
            bbox: None,
        }])
        .unwrap();

    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "v1-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("v1-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
}

#[test]
fn pre_commit_orphan_caches_are_purged_on_erasure() {
    let store = Store::in_memory().unwrap();
    let profile = EmbeddingProfileId::default_profile();
    // Live source with its own caches (erasure victim / trigger).
    let live = Source {
        id: SourceId("orphan-purge-live".into()),
        path: std::path::PathBuf::from("orphan-purge-live.md"),
        hash: "live-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    seed_source_with_caches(
        &store,
        &live,
        "child-live",
        "ev-live",
        "img-live",
        "live-input-hash",
        "live-image-hash",
        &profile,
        "config-a",
    );

    // Simulate cache write that never gained a committed live owner (crash before
    // source commit). These rows are invisible to current-source hash collection.
    store
        .upsert_embedding_cache_entries(
            &profile,
            "config-a",
            &[EmbeddingCacheEntry {
                embedding_input_hash: "orphan-input-hash".into(),
                vector: vec![0.1, 0.9],
            }],
        )
        .unwrap();
    let orphan_caption = ImageCaption {
        content_type: ImageCaptionContentType::Other,
        short_caption: "orphan caption".into(),
        detailed_description: "orphan detailed caption".into(),
        visible_text: vec![],
        key_entities: vec![],
        relationships: vec![],
        answerable_questions: vec![],
        uncertainties: vec![],
    };
    store
        .upsert_image_caption_attempt(
            "orphan-image-hash",
            "vision-test",
            VISION_CAPTION_PROMPT_VERSION,
            "prompt-hash",
            &CaptionAttempt::success(orphan_caption, r#"{"ok":true}"#.into(), 1),
        )
        .unwrap();
    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "orphan-input-hash")
        .unwrap()
        .is_some());
    assert!(store
        .get_image_caption("orphan-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_some());
    assert_eq!(embedding_cache_row_count(&store), 2);
    assert_eq!(store.list_image_captions().unwrap().len(), 2);

    store.remove_source(&live.id).unwrap();

    // Anti-join purge must remove both the live-owned caches and pre-commit orphans.
    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "orphan-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("orphan-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
    assert_eq!(embedding_cache_row_count(&store), 0);
    assert!(store.list_image_captions().unwrap().is_empty());

    // Re-use of the orphaned content hashes must recompute (no cache hit).
    let recompute = Source {
        id: SourceId("orphan-recompute-source".into()),
        path: std::path::PathBuf::from("orphan-recompute-source.md"),
        hash: "recompute-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&recompute).unwrap();
    store
        .bulk_insert_evidence(&[EvidenceUnit {
            id: EvidenceId("ev-recompute".into()),
            source_id: recompute.id.clone(),
            kind: EvidenceKind::Image,
            derived_from: None,
            locator: SourceLocator::legacy_pdf(1, 1, None),
            text: "image evidence for recompute".into(),
            text_hash: "hash-ev-recompute".into(),
            heading_path: vec!["Images".into()],
            position: 0,
        }])
        .unwrap();
    store
        .bulk_insert_chunks(&[Chunk {
            id: ChunkId("child-recompute".into()),
            source_id: recompute.id.clone(),
            chunk_hash: "chunk-hash-child-recompute".into(),
            embedding_input_hash: Some("orphan-input-hash".into()),
            text: "chunk text for recompute".into(),
            context_text: None,
            token_count: 8,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Images".into()],
            evidence_unit_ids: vec![EvidenceId("ev-recompute".into())],
        }])
        .unwrap();
    store
        .bulk_insert_image_artifacts(&[ImageArtifact {
            image_id: ImageId("img-recompute".into()),
            source_id: recompute.id.clone(),
            evidence_id: EvidenceId("ev-recompute".into()),
            relative_path: std::path::PathBuf::from("images/img-recompute.png"),
            content_hash: "orphan-image-hash".into(),
            mime_type: "image/png".into(),
            width: 16,
            height: 8,
            page: 1,
            image_index: 1,
            bbox: None,
        }])
        .unwrap();

    assert!(store
        .get_embedding_cache_vector(&profile, "config-a", "orphan-input-hash")
        .unwrap()
        .is_none());
    assert!(store
        .get_image_caption("orphan-image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
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
            locator: SourceLocator::legacy_pdf(1, 1, None),
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
