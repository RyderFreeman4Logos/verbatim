use super::*;

const TASK_STATUS_WAIT_BUDGET: Duration = Duration::from_secs(30);

pub(super) fn task_status_wait_deadline() -> Instant {
    Instant::now() + TASK_STATUS_WAIT_BUDGET
}

fn assert_status_wait(
    remaining: Duration,
    task_id: &TaskId,
    status: TaskStatus,
    observed: TaskStatus,
) {
    assert!(
        !remaining.is_zero(),
        "ingest task {} did not reach {status:?} within {}s; last durable status was {observed:?}",
        task_id.0,
        TASK_STATUS_WAIT_BUDGET.as_secs()
    );
}

pub(super) async fn wait_for_task_status(
    state: &SharedState,
    task_id: &TaskId,
    status: TaskStatus,
) {
    wait_for_task_status_until(state, task_id, status, task_status_wait_deadline()).await;
}

pub(super) async fn wait_for_task_status_until(
    state: &SharedState,
    task_id: &TaskId,
    status: TaskStatus,
    deadline: Instant,
) {
    loop {
        let response = task_summary_response(state, task_id.clone()).await;
        let Ok(response) = response else {
            panic!(
                "ingest task {} status wait could not read durable state",
                task_id.0
            );
        };
        let task = response.task;
        if task.status == status {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert_status_wait(remaining, task_id, status, task.status);
        tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
    }
}

#[tokio::test]
async fn task_status_wait_checks_durable_state_after_budget_elapsed() {
    let test_dir = TestDir::new("task-status-wait-durable");
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ingest,
        ingest_task_request_metadata_with_queue_claim(None, false, None, false, true),
    )
    .await
    .unwrap();
    ensure_ingest_task_started(&state, &task_id).await.unwrap();
    finish_task_success(
        &state,
        &task_id,
        ingest_result_metadata(0, &EmbeddingCacheStats::default()),
    )
    .await
    .unwrap();

    wait_for_task_status_until(&state, &task_id, TaskStatus::Succeeded, Instant::now()).await;
}

#[test]
#[should_panic(
    expected = "ingest task task-id did not reach Succeeded within 30s; last durable status was Queued"
)]
fn task_status_wait_timeout_names_waited_condition() {
    assert_status_wait(
        Duration::ZERO,
        &TaskId("task-id".into()),
        TaskStatus::Succeeded,
        TaskStatus::Queued,
    );
}
