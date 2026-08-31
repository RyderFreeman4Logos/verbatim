use super::*;
use verbatim_core::wire_schemas::QueryPlanControls;

#[test]
fn effective_default_collections_bind_distinct_query_plan_identities() {
    let request = RetrieveRequest {
        question: "What is cited?".into(),
        source_id: None,
        collection_filter: CollectionFilterRequest::default(),
        embedding_profile_id: None,
        limit: None,
        page_size: None,
        page: None,
        fast: false,
        rerank: None,
        dense_top_k: None,
        bm25_top_k: None,
        rerank_top_n: None,
        bypass_cache: false,
        include_debug: false,
        include_debug_packs: false,
        include_locator: false,
        passage: false,
    };
    let mut first_config = Config::default();
    first_config.retrieval.default_collections = vec!["alpha".into()];
    let mut second_config = Config::default();
    second_config.retrieval.default_collections = vec!["beta".into()];
    let first_scope = apply_default_collection_scope(
        &first_config.retrieval,
        None,
        request.collection_filter.clone(),
    );
    let second_scope = apply_default_collection_scope(
        &second_config.retrieval,
        None,
        request.collection_filter.clone(),
    );

    let first_plan = verbatim_core::api::query_plan_from_effective_controls_with_profile(
        &request.question,
        request.source_id.as_deref(),
        &first_scope,
        request.embedding_profile_id.as_deref(),
        QueryPlanControls::default(),
    )
    .unwrap();
    let second_plan = verbatim_core::api::query_plan_from_effective_controls_with_profile(
        &request.question,
        request.source_id.as_deref(),
        &second_scope,
        request.embedding_profile_id.as_deref(),
        QueryPlanControls::default(),
    )
    .unwrap();

    assert_ne!(
        first_plan.header.identity.content_hash,
        second_plan.header.identity.content_hash
    );
    assert_eq!(
        first_plan.collection_filter.as_ref().unwrap().names,
        vec!["alpha"]
    );
    assert_eq!(
        second_plan.collection_filter.as_ref().unwrap().names,
        vec!["beta"]
    );
}

#[test]
fn configured_default_collection_scope_applies_only_to_unscoped_retrieve() {
    let mut config = Config::default();
    config.retrieval.default_collections = vec!["articles".into(), "csb_bible".into()];

    let scoped =
        apply_default_collection_scope(&config.retrieval, None, CollectionFilterRequest::default());
    assert_eq!(scoped.names, vec!["articles", "csb_bible"]);

    let source_scoped = apply_default_collection_scope(
        &config.retrieval,
        Some(&SourceId("src-1".into())),
        CollectionFilterRequest::default(),
    );
    assert!(source_scoped.is_empty());

    let explicitly_scoped = apply_default_collection_scope(
        &config.retrieval,
        None,
        CollectionFilterRequest {
            collection_ids: Vec::new(),
            names: vec!["manual".into()],
            require_fresh: true,
        },
    );
    assert_eq!(explicitly_scoped.names, vec!["manual"]);
}
