use super::api_retrieve_envelope_wire_tests::sample_retrieve_response;
use super::{
    AnswerKind, AskRequest, AskResponse, ResponseTextTaxonomy, RetrieveRequest, RetrieveResponse,
};

#[test]
fn retrieve_query_plan_identity_changes_with_retrieval_controls() {
    let mut request = RetrieveRequest {
        question: "What is cited?".into(),
        source_id: Some("source-a".into()),
        collection_filter: Default::default(),
        embedding_profile_id: Some("profile:default".into()),
        limit: Some(12),
        page_size: Some(2),
        page: Some(1),
        fast: true,
        rerank: Some(true),
        dense_top_k: Some(80),
        bm25_top_k: Some(50),
        rerank_top_n: Some(12),
        bypass_cache: true,
        include_debug: true,
        include_debug_packs: true,
        include_locator: true,
        passage: true,
    };
    let first = serde_json::to_value(&request).unwrap();
    request.page = Some(2);
    let second = serde_json::to_value(&request).unwrap();
    assert_ne!(
        first["query_plan"]["header"]["identity"]["content_hash"],
        second["query_plan"]["header"]["identity"]["content_hash"]
    );
    assert_eq!(first["query_plan"]["page"], 1);
    assert_eq!(first["query_plan"]["source_id"], "source-a");
}

#[test]
fn retrieve_query_plan_must_match_supplied_request_controls() {
    let request = RetrieveRequest {
        question: "What is cited?".into(),
        source_id: Some("source-a".into()),
        collection_filter: Default::default(),
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
    let mut encoded = serde_json::to_value(&request).unwrap();
    encoded["source_id"] = serde_json::json!("source-b");
    let error = serde_json::from_value::<RetrieveRequest>(encoded)
        .expect_err("query plan/source mismatch must fail closed")
        .to_string();
    assert!(
        error.contains("query plan identity does not match"),
        "{error}"
    );
}

#[test]
fn ask_query_plan_maps_retrieval_controls_but_excludes_output_flags() {
    let request: AskRequest = serde_json::from_value(serde_json::json!({
        "question": "What is cited?",
        "source_id": "source-a",
        "collection_filter": {"names": ["beta", "alpha"], "require_fresh": true},
        "embedding_profile_id": "profile:default",
        "show_retrieval": true,
        "context_only": false,
        "limit": 12,
        "page_size": 2,
        "page": 1,
    }))
    .unwrap();
    let encoded = serde_json::to_value(&request).unwrap();
    let plan = &encoded["query_plan"];
    assert_eq!(plan["source_id"], "source-a");
    assert_eq!(
        plan["collection_filter"]["names"],
        serde_json::json!(["alpha", "beta"])
    );
    assert_eq!(plan["limit"], 12);
    assert_eq!(plan["page_size"], 2);
    assert_eq!(plan["page"], 1);
    assert!(plan.get("show_retrieval").is_none());
    assert!(plan.get("context_only").is_none());

    let mut output_only_changed = encoded;
    output_only_changed["show_retrieval"] = serde_json::json!(false);
    serde_json::from_value::<AskRequest>(output_only_changed)
        .expect("show_retrieval must not change QueryPlan identity");
}

#[test]
fn retrieve_and_ask_context_packs_keep_the_nondefault_query_plan_lineage() {
    let request: RetrieveRequest = serde_json::from_value(serde_json::json!({
        "question": "What is cited?",
        "source_id": "source-a",
        "limit": 3,
        "page_size": 1,
        "page": 2,
        "fast": true,
        "bypass_cache": true,
        "include_debug_packs": true,
        "include_locator": true,
        "passage": true,
    }))
    .unwrap();
    let plan = crate::api::query_plan_from_retrieve_request_with_profile(&request, None).unwrap();
    let mut response = sample_retrieve_response("What is cited?", "ev-1");
    response.query_plan = Some(plan.clone());

    let encoded = serde_json::to_value(&response).unwrap();
    assert_eq!(
        encoded["evidence_pack"]["query_plan_hash"],
        plan.header.identity.content_hash.as_str()
    );
    let decoded: RetrieveResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        decoded
            .evidence_pack
            .as_ref()
            .expect("validated evidence pack is retained")
            .query_plan_hash,
        plan.header.identity.content_hash.as_str()
    );

    let ask = AskResponse {
        task_id: "ask-1".into(),
        answer: String::new(),
        answer_kind: AnswerKind::EvidenceOnly,
        text_taxonomy: ResponseTextTaxonomy::ask_response(),
        generated_interpretation: None,
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: Some(decoded),
        collection_filter: None,
    };
    let encoded = serde_json::to_value(ask).unwrap();
    assert_eq!(
        encoded["context_pack"]["evidence_pack_hash"],
        encoded["context"]["evidence_pack"]["header"]["identity"]["content_hash"]
    );
}
