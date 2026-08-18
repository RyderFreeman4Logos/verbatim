#[test]
fn parses_context_fields_from_common_metadata_shapes() {
    for field in [
        "max_model_len",
        "max_context_length",
        "context_length",
        "max_sequence_length",
        "max_seq_len",
        "max_position_embeddings",
        "model_max_length",
        "n_ctx",
    ] {
        let response = serde_json::json!({
            "data": [
                {
                    "id": "target-model",
                    "model_extra": {
                        field: "4096"
                    }
                }
            ]
        });

        let capability =
            parse_endpoint_capability(&response, "target-model").expect("capability parsed");

        assert_eq!(capability.max_context_tokens, Some(4096), "{field}");
    }
}

#[test]
fn missing_or_invalid_capability_fields_return_unavailable() {
    for value in [
        serde_json::json!({"data": [{"id": "target-model"}]}),
        serde_json::json!({"data": [{"id": "target-model", "max_model_len": 0}]}),
        serde_json::json!({"data": [{"id": "target-model", "max_model_len": "invalid"}]}),
        serde_json::json!({"data": [{"id": "other-model", "max_model_len": 4096}, {"id": "second-model", "max_model_len": 2048}]}),
    ] {
        assert!(
            parse_endpoint_capability(&value, "target-model").is_none(),
            "{value}"
        );
    }
}

#[test]
fn parses_safe_request_shaping_hints() {
    let response = serde_json::json!({
        "data": [{
            "id": "target-model",
            "limits": {
                "max_batch_size": 8,
                "max_inputs": "4",
                "max_input_chars": 2000,
                "max_payload_chars": 10000,
                "max_candidates": 12,
                "max_documents": 10,
                "max_document_chars": 1500
            }
        }]
    });

    let capability =
        parse_endpoint_capability(&response, "target-model").expect("capability parsed");

    assert_eq!(capability.request_limits.embedding_batch_size(), Some(4));
    assert_eq!(capability.request_limits.rerank_candidate_count(), Some(10));
    assert_eq!(capability.request_limits.max_input_chars, Some(2000));
    assert_eq!(capability.request_limits.max_payload_chars, Some(10000));
    assert_eq!(capability.request_limits.max_document_chars, Some(1500));
}

#[test]
fn parses_output_dimensions_support_flag() {
    let response = serde_json::json!({
        "data": [{
            "id": "target-model",
            "model_extra": {
                "support_dimensions": true
            }
        }]
    });

    let capability =
        parse_endpoint_capability(&response, "target-model").expect("capability parsed");

    assert_eq!(capability.supports_output_dimensions, Some(true));
}

#[test]
fn cache_key_strips_query_tokens_and_distinguishes_role_provider_model() {
    let rerank_key = EndpointCapabilityCacheKey::new(
        "HTTP://LOCALHOST:8003/v1?token=fixture-secret#frag",
        EndpointCapabilityRole::Rerank,
        "VLLM",
        "model-a",
    );
    let embedding_key = EndpointCapabilityCacheKey::new(
        "http://localhost:8003/v1?token=other",
        EndpointCapabilityRole::Embedding,
        "openai_compatible",
        "model-a",
    );
    let other_model_key = EndpointCapabilityCacheKey::new(
        "http://localhost:8003/v1",
        EndpointCapabilityRole::Rerank,
        "vllm",
        "model-b",
    );

    assert_eq!(rerank_key.base_url(), "http://localhost:8003/v1");
    assert_ne!(rerank_key, embedding_key);
    assert_ne!(rerank_key, other_model_key);
}

#[test]
fn cache_respects_ttl_and_forced_refresh_path() {
    let cache = EndpointCapabilityCache::new();
    let key = EndpointCapabilityCacheKey::new(
        "http://localhost:8003/v1",
        EndpointCapabilityRole::Rerank,
        "vllm",
        "cache-model",
    );
    cache.insert(
        key.clone(),
        EndpointCapabilityLookup::refreshed(EndpointCapability {
            max_context_tokens: Some(4096),
            served_model: None,
            dtype: None,
            quantization: None,
            weight_identity: None,
            supports_output_dimensions: Some(true),
            request_limits: EndpointRequestLimits::default(),
        }),
    );

    let fresh = cache
        .get_fresh(&key, Duration::from_secs(60))
        .expect("fresh cache entry");
    assert_eq!(fresh.diagnostics.state, EndpointCapabilityState::Cached);
    assert_eq!(fresh.diagnostics.max_context_tokens, Some(4096));

    cache.age_entry(&key, Duration::from_secs(120));
    assert!(cache.get_fresh(&key, Duration::from_secs(60)).is_none());
}

#[test]
fn model_discovery_paths_support_v1_and_bare_host_base_urls() {
    assert_eq!(
        model_discovery_paths("http://127.0.0.1:8003/v1"),
        vec!["models"]
    );
    assert_eq!(
        model_discovery_paths("http://127.0.0.1:8003"),
        vec!["v1/models", "models"]
    );
}
