use super::*;
use verbatim_core::store::SqliteDurabilityProfile;

#[tokio::test]
async fn health_reports_effective_sqlite_durability_and_rpo() {
    let td = TestDir::new("sqlite-durability-health");
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.store.durability = SqliteDurabilityProfile::Durable;
    let pipeline = IngestPipeline::new(&config, td.path()).unwrap();
    let state = test_state(config, td.path(), pipeline);
    let Json(health) = health(State(state)).await;
    let durability = health.sqlite_durability.unwrap();

    assert_eq!(
        durability.effective.profile,
        SqliteDurabilityProfile::Durable
    );
    assert_eq!(durability.effective.journal_mode, "wal");
    assert_eq!(durability.effective.synchronous, "full");
    assert!(durability.effective.rpo.contains("RPO=0"));
    assert!(durability.disk.is_some());
}

#[tokio::test]
async fn task_store_read_uses_ephemeral_busy_timeout() {
    let td = TestDir::new("task-store-read-ephemeral-timeout");
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.store.durability = SqliteDurabilityProfile::Ephemeral;
    let pipeline = IngestPipeline::new(&config, td.path()).unwrap();
    let state = test_state(config, td.path(), pipeline);

    let effective = with_task_store_read(&state, Store::effective_durability)
        .await
        .unwrap();

    assert_eq!(effective.profile, SqliteDurabilityProfile::Ephemeral);
    assert_eq!(effective.busy_timeout_millis, 5_000);
}
