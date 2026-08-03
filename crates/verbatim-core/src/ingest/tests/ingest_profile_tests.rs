use super::*;

#[test]
fn capability_refresh_preserves_mixed_case_embedding_identities() {
    let mut config = embedding_context_config(8192);
    config.embedding.model = "Qwen/Qwen3-Embedding-8B".into();
    config.embedding.served_model = Some("Qwen/Qwen3-Embedding-8B-Served".into());
    config.embedding.dtype = Some("FLOAT16".into());
    config.embedding.quantization = Some("FP16".into());
    config.embedding.weight_identity = Some("Sha256:ABCDef".into());
    let mut spec = EmbeddingProfileSpec::from_config(&config.embedding);
    let before_hash = spec.config_hash();

    spec.apply_endpoint_capabilities(EmbeddingEndpointCapabilities {
        endpoint_identity: Some("https://embeddings.example.test/v1".into()),
        requested_model: Some(" Qwen/Qwen3-Embedding-8B ".into()),
        served_model: Some(" Qwen/Qwen3-Embedding-8B-Served ".into()),
        max_context_tokens: Some(8192),
        dtype: Some("float16".into()),
        quantization: Some("fp16".into()),
        weight_identity: Some(" Sha256:ABCDef ".into()),
    });

    assert_eq!(
        spec.requested_model.as_deref(),
        Some("Qwen/Qwen3-Embedding-8B")
    );
    assert_eq!(
        spec.served_model.as_deref(),
        Some("Qwen/Qwen3-Embedding-8B-Served")
    );
    assert_eq!(spec.dtype.as_deref(), Some("float16"));
    assert_eq!(spec.quantization.as_deref(), Some("fp16"));
    assert_eq!(spec.weight_identity.as_deref(), Some("Sha256:ABCDef"));
    assert_eq!(spec.config_hash(), before_hash);
}

#[test]
fn canonical_chunker_config_changes_profile_hash() {
    let config = embedding_context_config(8192);
    let first = EmbeddingProfileSpec::from_config(&config.embedding);
    let mut second = first.clone();
    second.canonical_chunker_config.overlap_units += 1;

    assert_ne!(first.config_hash(), second.config_hash());
}

#[test]
fn same_model_with_different_exposed_capability_has_distinct_safe_fingerprint() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut first = embedding_context_config(8192);
    first.embedding.base_url =
        "https://user:secret@embeddings.example.test/v1?api_key=hidden#frag".into();
    first.embedding.served_model = Some("served-a".into());
    first.embedding.dtype = Some("float16".into());
    first.embedding.quantization = Some("q4_k_m".into());
    first.embedding.weight_identity = Some("sha256:weights-a".into());
    let mut second = first.clone();
    second.embedding.served_model = Some("served-b".into());
    second.embedding.dtype = Some("float32".into());
    second.embedding.quantization = Some("q8_0".into());
    second.embedding.weight_identity = Some("sha256:weights-b".into());

    let first_spec = EmbeddingProfileSpec::from_config(&first.embedding);
    let second_spec = EmbeddingProfileSpec::from_config(&second.embedding);
    let pipeline = IngestPipeline::new(&first, tempdir.path()).unwrap();
    let status_json = serde_json::to_string(&pipeline.index_status().unwrap()).unwrap();

    assert_eq!(first_spec.model, second_spec.model);
    assert_ne!(first_spec.config_hash(), second_spec.config_hash());
    assert_eq!(
        first_spec.endpoint_identity.as_deref(),
        Some("https://embeddings.example.test/v1")
    );
    assert!(!status_json.contains("secret"));
    assert!(!status_json.contains("api_key"));
    assert!(!status_json.contains("hidden"));
    assert!(status_json.contains("served-a"));
    assert!(status_json.contains("q4_k_m"));
}
