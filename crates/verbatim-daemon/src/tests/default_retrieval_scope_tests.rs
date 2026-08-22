use super::*;

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
