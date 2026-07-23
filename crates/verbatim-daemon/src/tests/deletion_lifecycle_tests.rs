#[tokio::test]
async fn delete_source_returns_accepted_when_remote_erasure_is_pending() {
    let test_dir = TestDir::new("delete-source-pending-receipt");
    let source_path = test_dir.path().join("doc.md");
    fs::write(&source_path, "delete me remotely").unwrap();
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.qdrant.enabled = true;
    config.qdrant.url = "http://127.0.0.1:9".to_string();
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&source_path).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);

    let response = delete_source(State(Arc::clone(&state)), Path(source_id.0.clone()))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let report: verbatim_core::deletion::PersistedDeletionReport =
        serde_json::from_slice(&body).unwrap();
    assert_eq!(
        report
            .report
            .status_for(verbatim_core::deletion::DeletionProduct::Qdrant),
        Some(verbatim_core::deletion::DeletionOutcome::Pending)
    );
}

#[tokio::test]
async fn deletion_scheduler_continues_beyond_the_startup_batch_limit() {
    let test_dir = TestDir::new("deletion-reconcile-scheduler-continuation");
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.qdrant.enabled = true;
    config.qdrant.url = "http://127.0.0.1:9".to_string();
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let mut source_ids = Vec::new();
    for index in 0..=STARTUP_DELETION_RECONCILE_BATCH_SIZE {
        let source_path = test_dir.path().join(format!("doc-{index}.md"));
        fs::write(&source_path, "delete me remotely").unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.remove_source(&source_id).await.unwrap();
        source_ids.push(source_id);
    }
    let initial_report_count = pipeline.store().list_deletion_reports().unwrap().len();
    // Untouched candidates all have a NULL attempt sequence, so the store's stable
    // `source_id` tiebreaker identifies the candidate immediately after batch 16.
    source_ids.sort_by(|left, right| left.0.cmp(&right.0));
    let source_after_startup_batch = source_ids
        .into_iter()
        .nth(STARTUP_DELETION_RECONCILE_BATCH_SIZE)
        .expect("a seventeenth actionable deletion candidate");
    let state = test_state(config, test_dir.path(), pipeline);
    let scheduler = start_deletion_reconcile_scheduler(Arc::clone(&state));

    let reconciliation = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            // The reconciliation task owns the mutable pipeline slot while it writes.
            // Observe the durable receipt through a separate readonly connection instead.
            let reports =
                with_task_store_read(&state, |store| store.list_deletion_reports()).await?;
            let source_reconciled = reports
                .iter()
                .filter(|report| report.source_id == source_after_startup_batch)
                .count()
                >= 2;
            if source_reconciled {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    scheduler.abort();
    reconciliation
        .expect("post-ready scheduler reaches the seventeenth candidate without restart")
        .expect("scheduler receipts remain durably observable");
    assert!(
        initial_report_count >= STARTUP_DELETION_RECONCILE_BATCH_SIZE.saturating_add(1),
        "the scheduler started with more actionable candidates than one bounded batch"
    );
}
