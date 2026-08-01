struct BlockingVectorIndex {
    hits: Vec<(ChunkId, f32)>,
    first_search_barrier: Arc<std::sync::Barrier>,
    search_count: AtomicUsize,
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

    backend_barrier.wait();
    let first_result = first.join().expect("first dense search thread").unwrap();
    let second_result = second.join().expect("second dense search thread").unwrap();

    for (hits, path) in [first_result, second_result] {
        assert_eq!(hits, vec![(ChunkId("chunk-alpha".into()), 0.9)]);
        assert_eq!(path, RetrievalDenseVectorPath::ResidentHnsw);
    }
    assert_eq!(vector_index.search_count.load(Ordering::SeqCst), 2);
    assert_eq!(resource.snapshot().active, 0);
    assert_eq!(resource.snapshot().queued, 0);
}

fn spawn_dense_search(
    vector_index: Arc<BlockingVectorIndex>,
    resource: Arc<ObservableResource>,
) -> std::thread::JoinHandle<Result<(Vec<(ChunkId, f32)>, RetrievalDenseVectorPath)>> {
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
        runtime.block_on(pipeline.dense_search(&[1.0, 0.0], 1, None, &mut counters))
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

    pipeline
        .dense_search(&[1.0, 0.0], 1, None, &mut counters)
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

    let error = pipeline
        .dense_search(&[1.0, 0.0], 1, None, &mut counters)
        .await
        .expect_err("queued dense search times out");
    let error_chain = format!("{error:#}");
    assert!(error_chain.contains("acquire vector search resource"));
    assert!(error_chain.contains("timed out"));
    assert_eq!(resource.snapshot().queued, 0);
}
