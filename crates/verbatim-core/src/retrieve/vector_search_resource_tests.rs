struct BlockingVectorIndex {
    hits: Vec<(ChunkId, f32)>,
    first_search_barrier: Arc<std::sync::Barrier>,
    search_count: AtomicUsize,
}

struct RecordingVectorIndex {
    hits: Vec<(ChunkId, f32)>,
    search_count: AtomicUsize,
}

#[cfg(feature = "qdrant")]
struct EmptyQueryEmbeddingClient;

#[cfg(feature = "qdrant")]
#[async_trait]
impl EmbeddingClient for EmptyQueryEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| Vec::new()).collect())
    }

    fn dimension(&self) -> usize {
        0
    }
}

impl VectorIndex for RecordingVectorIndex {
    fn upsert(&mut self, _document: VectorDocument) {}

    fn delete_source(&mut self, _source_id: &SourceId) -> Result<()> {
        Ok(())
    }

    fn search(&self, _query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)> {
        self.search_count.fetch_add(1, Ordering::SeqCst);
        self.hits.iter().take(top_k).cloned().collect()
    }

    fn rebuild_from_store(&mut self, _store: &Store) -> Result<()> {
        Ok(())
    }

    fn len(&self) -> usize {
        self.hits.len()
    }
}

#[tokio::test]
async fn zero_budget_scoped_dense_search_stays_empty() {
    let store = Store::in_memory().unwrap();
    let vector_index = RecordingVectorIndex {
        hits: vec![(ChunkId("chunk-not-requested".into()), 0.9)],
        search_count: AtomicUsize::new(0),
    };
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig::default();
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );
    let source_filter = HashSet::from([SourceId("src-wanted".into())]);
    let mut counters = CandidateCounters::default();
    let mut spans = RetrievalLocalSpansMs::default();

    let (hits, _) = pipeline
        .dense_search(
            &[1.0, 0.0],
            0,
            Some(&source_filter),
            &mut counters,
            &mut spans,
        )
        .await
        .unwrap();

    assert!(hits.is_empty());
    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn qdrant_empty_and_zero_budget_inputs_use_local_dense_path() {
    let store = Store::in_memory().unwrap();
    let source = source("src-qdrant-local-empty");
    store.add_source(&source).unwrap();
    let chunk = insert_text_chunk(
        &store,
        &source,
        "chunk-qdrant-local-empty",
        "alpha local empty-vector evidence",
    );
    let vector_index = RecordingVectorIndex {
        hits: vec![(chunk.id.clone(), 0.9)],
        search_count: AtomicUsize::new(0),
    };
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = EmptyQueryEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 1,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let resource = Arc::new(ObservableResource::new(
        "vector_search",
        "vector_search",
        crate::resource::ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 1,
            queue_timeout: Duration::from_secs(1),
        },
    ));
    let (qdrant_url, handle) =
        spawn_optional_qdrant_search_response(200, r#"{"status":"ok","result":[]}"#);
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .with_vector_search_resource(Arc::clone(&resource))
    .with_qdrant_search(&qdrant_config(qdrant_url));

    let (results, debug) = pipeline
        .search_source_set_with_debug("alpha", None)
        .await
        .unwrap();

    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, chunk.id);
    assert_eq!(debug.dense_vector_path, RetrievalDenseVectorPath::ResidentHnsw);
    assert_eq!(
        debug
            .candidate_counters
            .requested_k(SpanKind::DenseRetrieval),
        1
    );
    assert_eq!(
        debug
            .candidate_counters
            .returned_k(SpanKind::DenseRetrieval),
        1
    );
    assert_eq!(debug.candidate_counters.evaluated(), 1);
    assert_eq!(debug.candidate_counters.hydrated(), 1);
    assert!(debug.local_spans_ms.vector_queue_wait_ms.is_some());
    assert!(debug.local_spans_ms.vector_service_ms.is_some());

    let mut counters = CandidateCounters::default();
    let mut spans = RetrievalLocalSpansMs::default();
    let (zero_hits, zero_path) = pipeline
        .dense_search(&[1.0, 0.0], 0, None, &mut counters, &mut spans)
        .await
        .unwrap();

    assert!(zero_hits.is_empty());
    assert_eq!(zero_path, RetrievalDenseVectorPath::ResidentHnsw);
    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 2);
    assert!(spans.vector_queue_wait_ms.is_some());
    assert!(spans.vector_service_ms.is_some());
    assert!(handle.join().unwrap().is_none());
    assert_eq!(resource.snapshot().active, 0);
    assert_eq!(resource.snapshot().queued, 0);
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn qdrant_empty_scoped_fallback_cap_exhaustion_is_typed() {
    const FOREIGN_HIT_COUNT: usize = 256;

    let (qdrant_url, handle) =
        spawn_qdrant_search_response(200, r#"{"status":"ok","result":[]}"#);
    let store = Store::in_memory().unwrap();
    let wanted_source = source("src-qdrant-cap-exhausted");
    let wanted_chunk = insert_child(
        &store,
        &wanted_source,
        "chunk-beyond-overfetch-cap",
        "alpha wanted beyond cap",
    );
    let mut local_hits = (0..FOREIGN_HIT_COUNT)
        .map(|rank| {
            (
                ChunkId(format!("chunk-foreign-{rank}")),
                (FOREIGN_HIT_COUNT - rank) as f32,
            )
        })
        .collect::<Vec<_>>();
    local_hits.push((wanted_chunk.id, 0.0));
    let vector_index = StaticVectorIndex::new(local_hits);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 1,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .with_qdrant_search(&qdrant_config(qdrant_url));

    let error = pipeline
        .search_filtered("alpha", Some(&wanted_source.id))
        .await
        .expect_err("strict source filter must fail closed at the overfetch cap");

    assert_eq!(
        error.downcast_ref::<crate::overfetch::OverfetchError>(),
        Some(&crate::overfetch::OverfetchError::UnsupportedStrictFilter)
    );
    assert_eq!(
        handle.join().unwrap(),
        "POST /collections/verbatim/points/search HTTP/1.1"
    );
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn qdrant_multi_source_complete_success_skips_local_dense_search() {
    let store = Store::in_memory().unwrap();
    let wanted_source_z = source("src-qdrant-z-authorized");
    let wanted_source_a = source("src-qdrant-a-authorized");
    let other_source = source("src-qdrant-out-of-scope");
    store.add_source(&wanted_source_z).unwrap();
    store.add_source(&wanted_source_a).unwrap();
    store.add_source(&other_source).unwrap();
    let wanted_chunk_z = insert_text_chunk(
        &store,
        &wanted_source_z,
        "chunk-qdrant-remote-first",
        "alpha wanted z",
    );
    let wanted_chunk_a = insert_text_chunk(
        &store,
        &wanted_source_a,
        "chunk-remote-preferred",
        "alpha wanted a",
    );
    let other_chunk = insert_text_chunk(
        &store,
        &other_source,
        "chunk-qdrant-out-of-scope",
        "alpha other",
    );
    store
        .replace_all_vector_documents(&[
            VectorDocument {
                chunk_id: wanted_chunk_z.id.clone(),
                source_id: wanted_source_z.id.clone(),
                vector: keyword_vector(&wanted_chunk_z.text),
            },
            VectorDocument {
                chunk_id: wanted_chunk_a.id.clone(),
                source_id: wanted_source_a.id.clone(),
                vector: keyword_vector(&wanted_chunk_a.text),
            },
            VectorDocument {
                chunk_id: other_chunk.id.clone(),
                source_id: other_source.id.clone(),
                vector: keyword_vector(&other_chunk.text),
            },
        ])
        .unwrap();
    assert_eq!(store.index_generation().unwrap(), 1);
    let (qdrant_url, handle) = spawn_qdrant_search_response_with_body(
        200,
        r#"{"status":"ok","result":[{"id":"2b2c1283-c2ff-5b12-a8ce-27bff2fff3a9","score":0.99,"payload":{"chunk_id":"chunk-qdrant-out-of-scope","profile_generation":1,"profile_id":"default","source_id":"src-qdrant-out-of-scope"}},{"id":"a5d8c7fa-ba7c-540c-95d4-31bdd9b12b99","score":0.98,"payload":{"chunk_id":"chunk-qdrant-remote-first","profile_generation":1,"profile_id":"default","source_id":"src-qdrant-z-authorized"}},{"id":"749ce13a-d809-57fe-a274-b32bec2735f0","score":0.97,"payload":{"chunk_id":"chunk-remote-preferred","profile_generation":1,"profile_id":"default","source_id":"src-qdrant-a-authorized"}}]}"#,
    );
    let vector_index = RecordingVectorIndex {
        hits: vec![
            (wanted_chunk_z.id.clone(), 0.9),
            (wanted_chunk_a.id.clone(), 0.8),
        ],
        search_count: AtomicUsize::new(0),
    };
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 2,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let resource = Arc::new(ObservableResource::new(
        "vector_search",
        "vector_search",
        crate::resource::ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 1,
            queue_timeout: Duration::from_secs(1),
        },
    ));
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .with_vector_search_resource(Arc::clone(&resource))
    .with_qdrant_search(&qdrant_config(qdrant_url));
    let mut source_filter = HashSet::new();
    source_filter.insert(wanted_source_z.id.clone());
    source_filter.insert(wanted_source_a.id.clone());

    let (results, debug) = pipeline
        .search_source_set_with_debug("alpha", Some(&source_filter))
        .await
        .unwrap();

    let (request_line, request_body) = handle.join().unwrap();
    assert_eq!(request_line, "POST /collections/verbatim/points/search HTTP/1.1");
    let request: serde_json::Value = serde_json::from_str(&request_body).unwrap();
    assert_eq!(request["limit"], 2);
    assert_eq!(request["filter"]["must"].as_array().unwrap().len(), 2);
    assert_eq!(request["filter"]["must"][0]["key"], "profile_id");
    assert_eq!(request["filter"]["must"][0]["match"]["value"], "default");
    assert_eq!(request["filter"]["must"][1]["key"], "source_id");
    assert_eq!(
        request["filter"]["must"][1]["match"]["any"],
        serde_json::json!(["src-qdrant-a-authorized", "src-qdrant-z-authorized"])
    );
    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 0);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].chunk_id, wanted_chunk_z.id);
    assert_eq!(results[0].chunk.source_id, wanted_source_z.id);
    assert_eq!(results[1].chunk_id, wanted_chunk_a.id);
    assert_eq!(results[1].chunk.source_id, wanted_source_a.id);
    assert_eq!(debug.dense_vector_path, RetrievalDenseVectorPath::Qdrant);
    assert_eq!(
        debug
            .candidate_counters
            .requested_k(SpanKind::DenseRetrieval),
        2
    );
    assert_eq!(
        debug
            .candidate_counters
            .returned_k(SpanKind::DenseRetrieval),
        2
    );
    assert_eq!(debug.candidate_counters.evaluated(), 2);
    assert_eq!(debug.candidate_counters.filtered(), 1);
    assert_eq!(debug.candidate_counters.hydrated(), 2);
    assert!(debug.local_spans_ms.vector_queue_wait_ms.is_some());
    assert!(debug.local_spans_ms.vector_service_ms.is_some());
    assert_eq!(resource.snapshot().active, 0);
    assert_eq!(resource.snapshot().queued, 0);
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn qdrant_wrong_profile_hit_fills_once_from_requested_profile() {
    let store = Store::in_memory().unwrap();
    let source = source("src-qdrant-wrong-profile");
    store.add_source(&source).unwrap();
    let chunk = insert_text_chunk(
        &store,
        &source,
        "chunk-qdrant-wrong-profile",
        "alpha requested profile",
    );
    let requested_profile = EmbeddingProfileId::default_profile();
    let wrong_profile = EmbeddingProfileId::new("wrong").unwrap();
    store
        .ensure_embedding_profile(
            &wrong_profile,
            crate::store::tests::test_profile_config("test", "wrong", 2, true, "", ""),
        )
        .unwrap();
    let document = VectorDocument {
        chunk_id: chunk.id.clone(),
        source_id: source.id.clone(),
        vector: keyword_vector(&chunk.text),
    };
    store
        .replace_all_vector_documents_for_profile(&requested_profile, &[document.clone()])
        .unwrap();
    store
        .replace_all_vector_documents_for_profile(&wrong_profile, &[document])
        .unwrap();
    assert_eq!(
        store
            .index_generation_for_profile(&requested_profile)
            .unwrap(),
        1
    );
    assert_eq!(
        store.index_generation_for_profile(&wrong_profile).unwrap(),
        1
    );
    let (qdrant_url, handle) = spawn_qdrant_search_response(
        200,
        r#"{"status":"ok","result":[{"id":"d6b51742-8bc3-5f9d-ba1c-0aeb83358342","score":0.99,"payload":{"chunk_id":"chunk-qdrant-wrong-profile","profile_generation":1,"profile_id":"wrong","source_id":"src-qdrant-wrong-profile"}}]}"#,
    );
    let vector_index = RecordingVectorIndex {
        hits: vec![(chunk.id.clone(), 0.9)],
        search_count: AtomicUsize::new(0),
    };
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 1,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let resource = Arc::new(ObservableResource::new(
        "vector_search",
        "vector_search",
        crate::resource::ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 1,
            queue_timeout: Duration::from_secs(1),
        },
    ));
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .require_embedding_profile(&requested_profile)
    .with_vector_search_resource(Arc::clone(&resource))
    .with_qdrant_search(&qdrant_config(qdrant_url));

    let (results, debug) = pipeline
        .search_source_set_with_debug("alpha", None)
        .await
        .unwrap();

    assert_eq!(
        handle.join().unwrap(),
        "POST /collections/verbatim/points/search HTTP/1.1"
    );
    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, chunk.id);
    assert_eq!(debug.dense_hits[0].score, 0.9);
    assert_eq!(debug.dense_vector_path, RetrievalDenseVectorPath::Qdrant);
    assert_eq!(
        debug
            .candidate_counters
            .requested_k(SpanKind::DenseRetrieval),
        1
    );
    assert_eq!(
        debug
            .candidate_counters
            .returned_k(SpanKind::DenseRetrieval),
        1
    );
    assert_eq!(debug.candidate_counters.evaluated(), 1);
    assert_eq!(debug.candidate_counters.hydrated(), 1);
    assert!(debug.local_spans_ms.vector_queue_wait_ms.is_some());
    assert!(debug.local_spans_ms.vector_service_ms.is_some());
    assert_eq!(resource.snapshot().active, 0);
    assert_eq!(resource.snapshot().queued, 0);
}

#[cfg(feature = "qdrant")]
#[test]
fn qdrant_wrong_source_hit_is_rejected_at_hydration() {
    let store = Store::in_memory().unwrap();
    let source = source("src-qdrant-authoritative");
    store.add_source(&source).unwrap();
    let chunk = insert_text_chunk(
        &store,
        &source,
        "chunk-qdrant-wrong-source",
        "alpha authoritative source",
    );
    store
        .replace_all_vector_documents(&[VectorDocument {
            chunk_id: chunk.id.clone(),
            source_id: source.id.clone(),
            vector: keyword_vector(&chunk.text),
        }])
        .unwrap();
    let vector_index = StaticVectorIndex::new(Vec::new());
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig::default();
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );
    let profile = EmbeddingProfileId::default_profile();
    let mut counters = CandidateCounters::default();

    let hits = pipeline
        .valid_dense_hits(
            vec![QdrantHit {
                point_id: "parsed-point-id".into(),
                chunk_id: chunk.id,
                profile_id: profile.clone(),
                source_id: SourceId("src-qdrant-forged".into()),
                score: 0.99,
                profile_generation: 1,
            }],
            1,
            None,
            Some((&profile, 1)),
            &mut counters,
        )
        .unwrap();

    assert!(hits.is_empty());
}

impl VectorIndex for BlockingVectorIndex {
    fn upsert(&mut self, _document: VectorDocument) {}

    fn delete_source(&mut self, _source_id: &SourceId) -> Result<()> {
        Ok(())
    }

    fn search(&self, _query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)> {
        if self.search_count.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first_search_barrier.wait();
        }
        self.hits.iter().take(top_k).cloned().collect()
    }

    fn rebuild_from_store(&mut self, _store: &Store) -> Result<()> {
        Ok(())
    }

    fn len(&self) -> usize {
        self.hits.len()
    }
}

#[tokio::test]
async fn dense_search_holds_vector_search_resource_through_backend_work() {
    let backend_barrier = Arc::new(std::sync::Barrier::new(2));
    let vector_index = Arc::new(BlockingVectorIndex {
        hits: vec![(ChunkId("chunk-alpha".into()), 0.9)],
        first_search_barrier: Arc::clone(&backend_barrier),
        search_count: AtomicUsize::new(0),
    });
    let resource = Arc::new(ObservableResource::new(
        "vector_search",
        "vector_search",
        crate::resource::ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 1,
            queue_timeout: Duration::from_secs(1),
        },
    ));
    let first = spawn_dense_search(Arc::clone(&vector_index), Arc::clone(&resource));

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if vector_index.search_count.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first dense search reaches blocked backend");
    assert_eq!(resource.snapshot().active, 1);

    let second = spawn_dense_search(Arc::clone(&vector_index), Arc::clone(&resource));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if resource.snapshot().queued == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second dense search enters vector resource queue");
    assert_eq!(resource.snapshot().active, 1);
    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 1);
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::time::sleep(Duration::from_millis(2)),
    )
    .await
    .expect("request timing advances beyond millisecond resolution");

    backend_barrier.wait();
    let (first_hits, first_path, first_spans) =
        first.join().expect("first dense search thread").unwrap();
    let (second_hits, second_path, second_spans) =
        second.join().expect("second dense search thread").unwrap();

    for (hits, path) in [(first_hits, first_path), (second_hits, second_path)] {
        assert_eq!(hits, vec![(ChunkId("chunk-alpha".into()), 0.9)]);
        assert_eq!(path, RetrievalDenseVectorPath::ResidentHnsw);
    }
    assert!(first_spans.vector_queue_wait_ms.is_some());
    assert!(first_spans
        .vector_service_ms
        .is_some_and(|service_ms| service_ms > 0));
    assert!(second_spans.vector_queue_wait_ms.is_some_and(|wait_ms| wait_ms > 0));
    assert!(second_spans.vector_service_ms.is_some());
    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 2);
    assert_eq!(resource.snapshot().active, 0);
    assert_eq!(resource.snapshot().queued, 0);
}

fn spawn_dense_search(
    vector_index: Arc<BlockingVectorIndex>,
    resource: Arc<ObservableResource>,
) -> std::thread::JoinHandle<
    Result<(
        Vec<(ChunkId, f32)>,
        RetrievalDenseVectorPath,
        RetrievalLocalSpansMs,
    )>,
> {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let store = Store::in_memory()?;
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig::default();
        let pipeline = RetrievalPipeline::new(
            vector_index.as_ref(),
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_vector_search_resource(resource);
        let mut counters = CandidateCounters::default();
        let mut spans = RetrievalLocalSpansMs::default();
        let (hits, path) = runtime.block_on(pipeline.dense_search(
            &[1.0, 0.0],
            1,
            None,
            &mut counters,
            &mut spans,
        ))?;
        Ok((hits, path, spans))
    })
}

#[tokio::test]
async fn dense_search_releases_vector_search_resource_after_backend_error() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("resource-error.sqlite");
    let store = Store::new(&database_path).unwrap();
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute_batch("DROP TABLE chunk_vectors")
        .unwrap();
    let vector_index = StaticVectorIndex::new(Vec::new());
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig::default();
    let resource = Arc::new(ObservableResource::new(
        "vector_search",
        "vector_search",
        crate::resource::ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 1,
            queue_timeout: Duration::from_secs(1),
        },
    ));
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .with_vector_residency(VectorIndexResidency::LowMemory)
    .with_vector_search_resource(Arc::clone(&resource));
    let mut counters = CandidateCounters::default();
    let mut spans = RetrievalLocalSpansMs::default();

    pipeline
        .dense_search(&[1.0, 0.0], 1, None, &mut counters, &mut spans)
        .await
        .expect_err("SQLite backend failure propagates");
    assert_eq!(resource.snapshot().active, 0);
    assert_eq!(resource.snapshot().queued, 0);
}

#[tokio::test]
async fn dense_search_maps_vector_search_resource_timeout() {
    let store = Store::in_memory().unwrap();
    let vector_index = StaticVectorIndex::new(Vec::new());
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig::default();
    let resource = Arc::new(ObservableResource::new(
        "vector_search",
        "vector_search",
        crate::resource::ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 1,
            queue_timeout: Duration::from_millis(10),
        },
    ));
    let _first = resource.acquire().await.expect("first permit");
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .with_vector_search_resource(Arc::clone(&resource));
    let mut counters = CandidateCounters::default();
    let mut spans = RetrievalLocalSpansMs::default();

    let error = pipeline
        .dense_search(&[1.0, 0.0], 1, None, &mut counters, &mut spans)
        .await
        .expect_err("queued dense search times out");
    let error_chain = format!("{error:#}");
    assert!(error_chain.contains("acquire vector search resource"));
    assert!(error_chain.contains("timed out"));
    assert_eq!(resource.snapshot().queued, 0);
}

#[tokio::test]
async fn dense_search_without_vector_resource_has_no_request_timings() {
    let store = Store::in_memory().unwrap();
    let vector_index = RecordingVectorIndex {
        hits: Vec::new(),
        search_count: AtomicUsize::new(0),
    };
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig::default();
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );
    let mut counters = CandidateCounters::default();
    let mut spans = RetrievalLocalSpansMs {
        vector_queue_wait_ms: Some(1),
        vector_service_ms: Some(1),
        ..RetrievalLocalSpansMs::default()
    };

    pipeline
        .dense_search(&[], 1, None, &mut counters, &mut spans)
        .await
        .unwrap();

    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 1);
    assert_eq!(spans.vector_queue_wait_ms, None);
    assert_eq!(spans.vector_service_ms, None);
}
