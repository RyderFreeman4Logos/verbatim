use super::*;
use crate::task::TaskKind;
use async_trait::async_trait;

struct ContentEmbeddingClient;

#[async_trait]
impl EmbeddingClient for ContentEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("new-vector") {
                    vec![0.0, 1.0]
                } else {
                    vec![1.0, 0.0]
                }
            })
            .collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

fn start_ingest_tasks(
    pipeline: &IngestPipeline<ContentEmbeddingClient>,
    task_prefix: &str,
    source_ids: &[SourceId],
) -> Vec<TaskId> {
    source_ids
        .iter()
        .enumerate()
        .map(|(index, source_id)| {
            let task_id = TaskId(format!("{task_prefix}-{index}"));
            pipeline
                .store()
                .create_task(
                    &task_id,
                    TaskKind::Ingest,
                    &serde_json::json!({ "source_id": source_id.0 }),
                )
                .unwrap();
            pipeline.store().start_task(&task_id).unwrap();
            task_id
        })
        .collect()
}

#[tokio::test]
async fn source_batch_index_publish_failure_marks_committed_sources_stale() {
    let source_tempdir = tempfile::tempdir().unwrap();
    let blocked_data_dir = tempfile::NamedTempFile::new().unwrap();
    let store = Store::in_memory().unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        ContentEmbeddingClient,
        blocked_data_dir.path().to_path_buf(),
    );
    let first_path = source_tempdir.path().join("batch-first.md");
    let second_path = source_tempdir.path().join("batch-second.md");
    fs::write(&first_path, "# First\n\nAlpha source batch vector body.").unwrap();
    fs::write(&second_path, "# Second\n\nBeta source batch vector body.").unwrap();
    let first_id = pipeline.add_source(&first_path).unwrap();
    let second_id = pipeline.add_source(&second_path).unwrap();
    let first_task = TaskId("task-batch-publish-fail-first".into());
    let second_task = TaskId("task-batch-publish-fail-second".into());
    for (task_id, source_id) in [(&first_task, &first_id), (&second_task, &second_id)] {
        pipeline
            .store()
            .create_task(
                task_id,
                TaskKind::Ingest,
                &serde_json::json!({ "source_id": source_id.0 }),
            )
            .unwrap();
        pipeline.store().start_task(task_id).unwrap();
    }
    let profile = EmbeddingProfileId::default_profile();
    let generation_before = pipeline
        .store()
        .index_generation_for_profile(&profile)
        .unwrap();

    let outcomes = pipeline
        .ingest_sources_with_tasks(&[
            (first_id.clone(), first_task.clone()),
            (second_id.clone(), second_task.clone()),
        ])
        .await;

    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|outcome| {
        outcome
            .result
            .as_ref()
            .unwrap_err()
            .contains("batched index publication failed after source commit")
    }));
    assert_eq!(
        pipeline
            .store()
            .index_generation_for_profile(&profile)
            .unwrap(),
        generation_before + 1,
        "batch stage failure after committed rows must invalidate the old generation"
    );
    assert!(pipeline.hnsw().is_empty());
    for source_id in [&first_id, &second_id] {
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(&profile, Some(source_id))
                .unwrap(),
            1
        );
        assert!(
            pipeline
                .store()
                .source_vectors_stale_for_profile(&profile, source_id)
                .unwrap(),
            "committed source must fail freshness after batched publish failure"
        );
    }
}

#[tokio::test]
async fn source_batch_enospc_with_failed_sqlite_compensation_restarts_from_committed_vectors() {
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir.path().join("verbatim.db");
    let store = Store::new(&db_path).unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        ContentEmbeddingClient,
        data_dir.path().to_path_buf(),
    );
    let first_path = data_dir.path().join("restart-first.md");
    let second_path = data_dir.path().join("restart-second.md");
    fs::write(&first_path, "# First\n\nold-vector alpha").unwrap();
    fs::write(&second_path, "# Second\n\nold-vector beta").unwrap();
    let source_ids = [
        pipeline.add_source(&first_path).unwrap(),
        pipeline.add_source(&second_path).unwrap(),
    ];
    let initial_tasks = start_ingest_tasks(&pipeline, "task-restart-initial", &source_ids);
    let initial_outcomes = pipeline
        .ingest_sources_with_tasks(&[
            (source_ids[0].clone(), initial_tasks[0].clone()),
            (source_ids[1].clone(), initial_tasks[1].clone()),
        ])
        .await;
    assert_eq!(initial_outcomes.len(), source_ids.len());
    assert!(initial_outcomes
        .iter()
        .all(|outcome| outcome.result.is_ok()));
    let profile = EmbeddingProfileId::default_profile();
    let old_generation = pipeline
        .store()
        .index_generation_for_profile(&profile)
        .unwrap();
    let old_manifest_generation = read_index_manifest(data_dir.path(), &profile)
        .unwrap()
        .unwrap()
        .generation;
    assert_eq!(old_manifest_generation, old_generation);
    let old_published_vectors = pipeline.hnsw().points().to_vec();

    fs::write(&first_path, "# First\n\nnew-vector alpha").unwrap();
    fs::write(&second_path, "# Second\n\nnew-vector beta").unwrap();
    let retry_tasks = start_ingest_tasks(&pipeline, "task-restart-retry", &source_ids);
    pipeline.fail_next_batched_index_stage_with_enospc();
    pipeline.fail_next_batched_compensation_write_with_readonly();

    let failed_outcomes = pipeline
        .ingest_sources_with_tasks(&[
            (source_ids[0].clone(), retry_tasks[0].clone()),
            (source_ids[1].clone(), retry_tasks[1].clone()),
        ])
        .await;

    let failed_errors = failed_outcomes
        .iter()
        .filter_map(|outcome| outcome.result.as_ref().err())
        .collect::<Vec<_>>();
    assert!(
        failed_errors.len() == failed_outcomes.len()
            && failed_errors
                .iter()
                .all(|error| error.contains("No space left on device")),
        "unexpected batch outcomes: {failed_outcomes:?}"
    );
    let committed_generation = pipeline
        .store()
        .index_generation_for_profile(&profile)
        .unwrap();
    assert_eq!(committed_generation, old_generation + 1);
    let committed_vectors = pipeline
        .store()
        .list_vector_documents_for_profile(&profile)
        .unwrap();
    assert_ne!(committed_vectors, old_published_vectors);
    assert!(source_ids.iter().all(|source_id| {
        !pipeline
            .store()
            .source_vectors_stale_for_profile(&profile, source_id)
            .unwrap()
    }));
    drop(pipeline);

    let reopened_store = Store::new(&db_path).unwrap();
    assert_eq!(
        reopened_store
            .index_generation_for_profile(&profile)
            .unwrap(),
        committed_generation
    );
    assert_eq!(
        read_index_manifest(data_dir.path(), &profile)
            .unwrap()
            .unwrap()
            .generation,
        old_manifest_generation,
        "failed staging must leave the previously published manifest in place"
    );
    let reloaded = load_published_vector_index(data_dir.path(), &reopened_store, &profile).unwrap();
    let mut rebuilt = HnswIndex::new();
    rebuilt
        .rebuild_from_store_for_profile(&reopened_store, &profile)
        .unwrap();

    assert_ne!(reloaded.points(), old_published_vectors);
    assert_eq!(reloaded.points(), committed_vectors);
    assert_eq!(reloaded.points(), rebuilt.points());
}

#[tokio::test]
async fn published_hnsw_is_revalidated_against_sqlite_point_set() {
    let data_dir = tempfile::tempdir().unwrap();
    let store = Store::new(&data_dir.path().join("verbatim.db")).unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        ContentEmbeddingClient,
        data_dir.path().to_path_buf(),
    );
    let first_path = data_dir.path().join("a.md");
    let second_path = data_dir.path().join("b.md");
    fs::write(&first_path, "first-vector").unwrap();
    fs::write(&second_path, "new-vector").unwrap();
    let first_source = pipeline.add_source(&first_path).unwrap();
    let second_source = pipeline.add_source(&second_path).unwrap();
    pipeline.ingest_source(&first_source).await.unwrap();
    pipeline.ingest_source(&second_source).await.unwrap();

    let profile = EmbeddingProfileId::default_profile();
    let sqlite_points = pipeline
        .store()
        .list_vector_documents_for_profile(&profile)
        .unwrap();
    assert_eq!(sqlite_points.len(), 2);
    let first_point = sqlite_points
        .iter()
        .find(|point| point.source_id == first_source)
        .unwrap()
        .clone();
    let second_point = sqlite_points
        .iter()
        .find(|point| point.source_id == second_source)
        .unwrap()
        .clone();
    assert_eq!(first_point.vector, vec![1.0, 0.0]);
    assert_eq!(second_point.vector, vec![0.0, 1.0]);
    let generation = pipeline
        .store()
        .index_generation_for_profile(&profile)
        .unwrap();
    let hnsw_path =
        index_generation_dir(data_dir.path(), &profile, generation).join("vectors.hnsw");

    let mut published = HnswIndex::new();
    published.load(&hnsw_path).unwrap();
    let untouched =
        load_published_vector_index(data_dir.path(), pipeline.store(), &profile).unwrap();
    assert_eq!(untouched.points(), published.points());
    assert_eq!(
        untouched
            .search(&[1.0, 0.0], 1)
            .first()
            .map(|(chunk_id, _)| chunk_id),
        Some(&first_point.chunk_id)
    );

    // A semantically equal point set must be accepted regardless of publication order.
    let reordered_points = sqlite_points.iter().rev().cloned().collect::<Vec<_>>();
    let mut reordered = HnswIndex::new();
    reordered.replace_all(reordered_points.clone());
    reordered.save(&hnsw_path).unwrap();
    let reordered =
        load_published_vector_index(data_dir.path(), pipeline.store(), &profile).unwrap();
    assert_eq!(reordered.points(), reordered_points);

    let tampered_point_sets = [
        (
            "count-preserving vector swap",
            vec![
                VectorDocument {
                    vector: sqlite_points[1].vector.clone(),
                    ..sqlite_points[0].clone()
                },
                VectorDocument {
                    vector: sqlite_points[0].vector.clone(),
                    ..sqlite_points[1].clone()
                },
            ],
        ),
        (
            "duplicate chunk IDs",
            vec![
                sqlite_points[0].clone(),
                VectorDocument {
                    vector: sqlite_points[1].vector.clone(),
                    ..sqlite_points[0].clone()
                },
            ],
        ),
        (
            "wrong source IDs",
            vec![
                VectorDocument {
                    source_id: sqlite_points[1].source_id.clone(),
                    ..sqlite_points[0].clone()
                },
                VectorDocument {
                    source_id: sqlite_points[0].source_id.clone(),
                    ..sqlite_points[1].clone()
                },
            ],
        ),
        ("missing point", vec![sqlite_points[1].clone()]),
        (
            "extra point",
            vec![
                sqlite_points[0].clone(),
                sqlite_points[1].clone(),
                VectorDocument {
                    chunk_id: ChunkId("chunk-extra".into()),
                    source_id: SourceId("source-extra".into()),
                    vector: vec![-1.0, 0.0],
                },
            ],
        ),
        (
            "altered vector",
            vec![
                VectorDocument {
                    vector: vec![0.6, 0.8],
                    ..sqlite_points[0].clone()
                },
                sqlite_points[1].clone(),
            ],
        ),
    ];

    for (case, points) in tampered_point_sets {
        let mut tampered = HnswIndex::new();
        tampered.replace_all(points);
        tampered.save(&hnsw_path).unwrap();

        let loaded =
            load_published_vector_index(data_dir.path(), pipeline.store(), &profile).unwrap();
        assert_eq!(
            loaded
                .search(&[1.0, 0.0], 1)
                .first()
                .map(|(chunk_id, _)| chunk_id),
            Some(&first_point.chunk_id),
            "{case} must rebuild before serving"
        );
        assert_eq!(
            loaded.points(),
            sqlite_points,
            "{case} must restore SQLite's exact point set"
        );
    }
}
