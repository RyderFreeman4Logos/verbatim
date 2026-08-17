use super::*;

use verbatim_core::api::{AuditReceiptResult, AUDIT_RECEIPT_VERSION};

fn receipt_result_tuples(response: &RetrieveResponse) -> Vec<AuditReceiptResult> {
    response
        .results
        .iter()
        .map(|result| AuditReceiptResult {
            evidence_id: result.evidence_id.clone(),
            text_hash: result.text_hash.clone(),
            source_hash: result.source_hash.clone(),
        })
        .collect()
}

#[test]
fn compact_retrieve_emits_versioned_audit_receipt() {
    let (_dir, store, persisted) = persisted_output_fixture("audit-receipt-compact", None);
    let response =
        final_retrieve_response(&store, retrieve_output_input(persisted, false)).unwrap();

    assert_eq!(response.audit_receipt.version, AUDIT_RECEIPT_VERSION);
    assert_eq!(
        response.audit_receipt.embedding_profile_id,
        response.embedding_profile_id
    );
    assert_eq!(
        response.audit_receipt.source_bounded,
        response.source_bounded
    );
    assert_eq!(response.audit_receipt.controls, response.controls);
    assert_eq!(
        response.audit_receipt.results,
        receipt_result_tuples(&response)
    );

    let value = serde_json::to_value(&response).unwrap();
    let receipt = value
        .get("audit_receipt")
        .expect("compact retrieve serializes an audit_receipt");
    assert_eq!(receipt["version"], AUDIT_RECEIPT_VERSION);
    let tuples = receipt["results"].as_array().unwrap();
    assert_eq!(tuples.len(), response.results.len());
    assert_eq!(tuples[0]["evidence_id"], response.results[0].evidence_id);
    assert_eq!(tuples[0]["text_hash"], response.results[0].text_hash);
    assert_eq!(tuples[0]["source_hash"], response.results[0].source_hash);
}

#[test]
fn passage_retrieve_emits_same_version_receipt_for_same_snapshot() {
    let (_dir, store, persisted) = persisted_output_fixture("audit-receipt-passage", None);
    let compact =
        final_retrieve_response(&store, retrieve_output_input(persisted.clone(), false)).unwrap();
    let passage = final_retrieve_response(&store, retrieve_output_input(persisted, true)).unwrap();

    assert_eq!(response_version(&compact), response_version(&passage));
    assert_eq!(compact.audit_receipt.results, passage.audit_receipt.results);
    assert_eq!(
        passage.audit_receipt.results,
        receipt_result_tuples(&passage)
    );
}

fn response_version(response: &RetrieveResponse) -> u8 {
    response.audit_receipt.version
}

#[test]
fn empty_final_page_emits_empty_tupled_receipt() {
    let (_dir, store, persisted) = persisted_output_fixture("audit-receipt-empty", None);
    let mut input = retrieve_output_input(persisted, false);
    input.controls.page = 2;
    let response = final_retrieve_response(&store, input)
        .expect("empty final page still emits a versioned audit receipt");

    assert_eq!(response.returned_results, 0);
    assert!(response.results.is_empty());
    assert_eq!(response.audit_receipt.version, AUDIT_RECEIPT_VERSION);
    assert!(response.audit_receipt.results.is_empty());

    let value = serde_json::to_value(&response).unwrap();
    assert!(value["audit_receipt"]["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn generated_evidence_cannot_enter_audit_receipt() {
    let (_dir, store, persisted) = persisted_output_fixture("audit-receipt-generated", None);
    let mut input = retrieve_output_input(persisted.clone(), false);
    input.results.push(generated_caption_result(
        2,
        "chunk-2",
        "ev-2",
        "Generated caption.",
        None,
    ));
    let response = final_retrieve_response(&store, input)
        .expect("source-bounded filtering still emits an audit receipt");

    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].evidence_id, "ev-1");
    assert_eq!(response.audit_receipt.results.len(), 1);
    let tuple = &response.audit_receipt.results[0];
    assert_eq!(tuple.evidence_id, "ev-1");
    assert_eq!(tuple.text_hash, response.results[0].text_hash);
    assert_eq!(tuple.source_hash, response.results[0].source_hash);
}

#[test]
fn mutated_evidence_fails_closed_without_audit_receipt() {
    let (_dir, store, captured) = persisted_output_fixture("audit-receipt-mutated", None);
    let mut reindexed = captured.evidence_units[0].clone();
    reindexed.text = "Mutated audit receipt evidence.".into();
    reindexed.text_hash = verbatim_core::types::hex_sha256(reindexed.text.as_bytes());
    replace_persisted_evidence(&store, &reindexed, &captured.chunk);

    let error = final_retrieve_response(&store, retrieve_output_input(captured, false))
        .expect_err("mutated evidence must fail closed before an audit receipt");
    assert!(
        error.to_string().contains("changed since retrieval"),
        "{error}"
    );
}

#[test]
fn audit_receipt_is_deterministic_and_bound_sensitive() {
    let (_dir, store, results) = persisted_pair_fixture("same-snapshot", "source-hash");
    let response_a = final_retrieve_response(&store, pair_input(results.clone(), false)).unwrap();
    let response_b = final_retrieve_response(&store, pair_input(results.clone(), false)).unwrap();
    assert_eq!(
        serde_json::to_string(&response_a.audit_receipt).unwrap(),
        serde_json::to_string(&response_b.audit_receipt).unwrap()
    );
    assert_eq!(response_a.audit_receipt.results.len(), 2);

    let mut swapped = results.clone();
    swapped.reverse();
    let response_swapped = final_retrieve_response(&store, pair_input(swapped, false)).unwrap();
    assert_ne!(
        serde_json::to_string(&response_a.audit_receipt).unwrap(),
        serde_json::to_string(&response_swapped.audit_receipt).unwrap()
    );

    let (_dir_b, store_b, _) = persisted_pair_fixture("other-source-hash", "source-hash-2");
    let response_other = final_retrieve_response(&store_b, pair_input(results, false)).unwrap();
    assert_ne!(
        serde_json::to_string(&response_a.audit_receipt).unwrap(),
        serde_json::to_string(&response_other.audit_receipt).unwrap()
    );
}

fn persisted_pair_fixture(name: &str, source_hash: &str) -> (TestDir, Store, Vec<RetrievalResult>) {
    let dir = TestDir::new(&format!("audit-receipt-{name}"));
    let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
    let mut results = Vec::new();
    let mut added_sources = HashSet::new();
    for (rank, chunk_id, evidence_id, text) in [
        (1usize, "chunk-a", "ev-a", "Alpha persisted text."),
        (2, "chunk-b", "ev-b", "Beta persisted text."),
    ] {
        let mut result = test_retrieval_result(rank, chunk_id, evidence_id, EvidenceKind::Text);
        let evidence = &mut result.evidence_units[0];
        evidence.text = text.into();
        evidence.text_hash = verbatim_core::types::hex_sha256(evidence.text.as_bytes());
        result.chunk.text.clone_from(&evidence.text);
        if added_sources.insert(evidence.source_id.clone()) {
            store
                .add_source(&Source {
                    id: evidence.source_id.clone(),
                    path: dir.path().join(format!("{chunk_id}.md")),
                    hash: source_hash.into(),
                    status: SourceStatus::Indexed,
                    parser_used: Some("markdown".into()),
                    last_ingested_at: None,
                })
                .unwrap();
        }
        store
            .bulk_insert_evidence(std::slice::from_ref(evidence))
            .unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&result.chunk))
            .unwrap();
        store
            .link_chunk_evidence(&[(result.chunk.id.clone(), result.evidence_units[0].id.clone())])
            .unwrap();
        results.push(result);
    }
    (dir, store, results)
}

fn pair_input(results: Vec<RetrievalResult>, passage: bool) -> RetrieveResponseInput {
    let mut debug = empty_retrieval_debug();
    refresh_final_evidence_pack_debug(&mut debug, &results);
    RetrieveResponseInput {
        task_id: TaskId("task-audit-receipt".into()),
        query: "What is bound?".into(),
        source_filter: Some(SourceId("src".into())),
        collection_filter: None,
        collection_provenance: HashMap::new(),
        embedding_profile_id: EmbeddingProfileId::default_profile(),
        controls: EffectiveRetrieveControls {
            limit: 2,
            page_size: 2,
            page: 1,
            include_debug: false,
            include_debug_packs: false,
            include_locator: true,
            passage,
            bypass_cache: false,
            fast: false,
            config: Config::default(),
            retrieval_config: RetrievalConfig::default(),
            rerank_config: RerankConfig::default(),
        },
        results,
        debug,
        sources: HashMap::new(),
        retrieval_ms: 1,
    }
}
