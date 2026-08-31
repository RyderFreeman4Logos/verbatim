use super::query_plan_test_support::test_query_plan;
use super::*;

#[test]
fn retrieve_response_pages_context_pack_without_full_locator_by_default() {
    let results = vec![
        test_retrieval_result(1, "chunk-1", "ev-1", EvidenceKind::Text),
        test_retrieval_result(2, "chunk-2", "ev-2", EvidenceKind::Text),
    ];
    let mut debug = empty_retrieval_debug();
    refresh_final_evidence_pack_debug(&mut debug, &results);

    let response = persisted_retrieve_response(RetrieveResponseInput {
        task_id: TaskId("task-1".into()),
        query: "What is cited?".into(),
        source_filter: Some(SourceId("src".into())),
        collection_filter: None,
        collection_provenance: HashMap::new(),
        embedding_profile_id: EmbeddingProfileId::default_profile(),
        query_plan: Some(test_query_plan("What is cited?")),
        controls: EffectiveRetrieveControls {
            limit: 2,
            page_size: 1,
            page: 2,
            include_debug: false,
            include_debug_packs: false,
            include_locator: false,
            passage: false,
            bypass_cache: false,
            fast: false,
            config: Config::default(),
            retrieval_config: RetrievalConfig::default(),
            rerank_config: RerankConfig::default(),
        },
        results,
        debug,
        sources: HashMap::new(),
        retrieval_ms: 7,
    });

    assert_eq!(response.total_results, 2);
    assert_eq!(response.returned_results, 1);
    assert_eq!(response.results[0].index, 1);
    assert_eq!(response.results[0].evidence_id, "ev-2");
    assert!(response.results[0].structured_locator.is_none());
    assert!(response.results[0].provenance.is_none());
    assert!(response.debug.is_none());
}

#[test]
fn retrieve_response_no_passage_uses_canonical_display_support_pack() {
    let results = vec![test_canonical_retrieval_result(
        1,
        "chunk-2tim4",
        &[("ev-1", 1), ("ev-8", 8), ("ev-9", 9)],
    )];
    let mut debug = empty_retrieval_debug();
    refresh_final_evidence_pack_debug(&mut debug, &results);
    debug.display_evidence_pack = vec![RetrievalEvidencePackEntry {
        label: "E1".into(),
        ..debug.final_evidence_pack[1].clone()
    }];
    debug.display_evidence_count = debug.display_evidence_pack.len();

    let response = persisted_retrieve_response(RetrieveResponseInput {
        task_id: TaskId("task-1".into()),
        query: "crown of righteousness".into(),
        source_filter: Some(SourceId("src".into())),
        collection_filter: None,
        collection_provenance: HashMap::new(),
        embedding_profile_id: EmbeddingProfileId::default_profile(),
        query_plan: Some(test_query_plan("crown of righteousness")),
        controls: EffectiveRetrieveControls {
            limit: 1,
            page_size: 1,
            page: 1,
            include_debug: false,
            include_debug_packs: false,
            include_locator: true,
            passage: false,
            bypass_cache: false,
            fast: false,
            config: Config::default(),
            retrieval_config: RetrievalConfig::default(),
            rerank_config: RerankConfig::default(),
        },
        results,
        debug,
        sources: HashMap::new(),
        retrieval_ms: 7,
    });

    assert_eq!(response.total_results, 1);
    assert_eq!(response.returned_results, 1);
    let result = &response.results[0];
    assert_eq!(result.evidence_id, "ev-8");
    assert_eq!(result.chunk_id, "chunk-2tim4");
    assert_eq!(result.score, 1.0);
    assert_eq!(result.locator, "2 Timothy 4:8");
    assert_eq!(result.snippet, "verse 8 text.");
    assert!(matches!(
        result.structured_locator,
        Some(SourceLocator::Canonical { .. })
    ));
}

#[test]
fn retrieve_response_passage_mode_pages_by_canonical_chunk() {
    let results = vec![test_canonical_retrieval_result(
        1,
        "chunk-2tim4",
        &[("ev-1", 1), ("ev-2", 2), ("ev-3", 3)],
    )];
    let mut debug = empty_retrieval_debug();
    refresh_final_evidence_pack_debug(&mut debug, &results);
    debug.display_evidence_pack = vec![RetrievalEvidencePackEntry {
        label: "E1".into(),
        ..debug.final_evidence_pack[1].clone()
    }];
    debug.display_evidence_count = debug.display_evidence_pack.len();

    let response = persisted_retrieve_response(RetrieveResponseInput {
        task_id: TaskId("task-1".into()),
        query: "crown of righteousness".into(),
        source_filter: Some(SourceId("src".into())),
        collection_filter: None,
        collection_provenance: HashMap::new(),
        embedding_profile_id: EmbeddingProfileId::default_profile(),
        query_plan: Some(test_query_plan("crown of righteousness")),
        controls: EffectiveRetrieveControls {
            limit: 1,
            page_size: 1,
            page: 1,
            include_debug: false,
            include_debug_packs: false,
            include_locator: true,
            passage: true,
            bypass_cache: false,
            fast: false,
            config: Config::default(),
            retrieval_config: RetrievalConfig::default(),
            rerank_config: RerankConfig::default(),
        },
        results,
        debug,
        sources: HashMap::new(),
        retrieval_ms: 7,
    });

    assert_eq!(response.total_results, 1);
    assert_eq!(response.returned_results, 1);
    assert_eq!(response.results.len(), 1);
    let passage = &response.results[0];
    assert_eq!(passage.index, 0);
    assert_eq!(passage.rank, 1);
    assert_eq!(passage.locator, "2 Timothy 4:1-3");
    assert_eq!(passage.snippet, "verse 1 text. verse 2 text. verse 3 text.");
    assert!(matches!(
        passage.structured_locator,
        Some(SourceLocator::Canonical { .. })
    ));
}

#[test]
fn retrieve_response_passage_mode_uses_ranked_chunk_membership_without_debug_pack() {
    let results = vec![
        test_canonical_retrieval_result(1, "chunk-2tim4", &[("ev-1", 1), ("ev-2", 2), ("ev-3", 3)]),
        test_canonical_retrieval_result(
            2,
            "chunk-ps23",
            &[("ev-ps23-1", 1), ("ev-ps23-2", 2), ("ev-ps23-3", 3)],
        ),
    ];
    let mut debug = empty_retrieval_debug();
    debug.evidence_pack_mode = RetrievalDebugEvidencePackMode::Compact;
    debug.final_evidence_pack.clear();
    debug.display_evidence_pack.clear();

    let response = persisted_retrieve_response(RetrieveResponseInput {
        task_id: TaskId("task-1".into()),
        query: "crown of righteousness".into(),
        source_filter: Some(SourceId("src".into())),
        collection_filter: None,
        collection_provenance: HashMap::new(),
        embedding_profile_id: EmbeddingProfileId::default_profile(),
        query_plan: Some(test_query_plan("crown of righteousness")),
        controls: EffectiveRetrieveControls {
            limit: 1,
            page_size: 1,
            page: 1,
            include_debug: false,
            include_debug_packs: false,
            include_locator: true,
            passage: true,
            bypass_cache: false,
            fast: false,
            config: Config::default(),
            retrieval_config: RetrievalConfig::default(),
            rerank_config: RerankConfig::default(),
        },
        results,
        debug,
        sources: HashMap::new(),
        retrieval_ms: 7,
    });

    assert_eq!(response.total_results, 2);
    assert_eq!(response.returned_results, 1);
    let passage = &response.results[0];
    assert_eq!(passage.evidence_id, "ev-1");
    assert_eq!(passage.chunk_id, "chunk-2tim4");
    assert_eq!(passage.locator, "2 Timothy 4:1-3");
    assert_eq!(passage.snippet, "verse 1 text. verse 2 text. verse 3 text.");
    assert!(matches!(
        passage.structured_locator,
        Some(SourceLocator::Canonical { .. })
    ));
}
