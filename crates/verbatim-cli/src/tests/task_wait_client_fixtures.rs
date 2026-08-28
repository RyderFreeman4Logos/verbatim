const TASK_WAIT_TERMINAL_TASK: &str = r#"{"id":"task-1","kind":"ask","status":"succeeded","created_at":"1","updated_at":"2","started_at":"1","finished_at":"2","request":{},"result":{},"error":null}"#;
const TASK_WAIT_RUNNING_TASK: &str = r#"{"id":"task-1","kind":"ask","status":"running","created_at":"1","updated_at":"2","started_at":"1","finished_at":null,"request":{},"result":null,"error":null,"progress":{"phase":{"name":"chat","started_at":"1","elapsed_ms":100},"recent_status":"streaming"}}"#;

fn task_wait_response(task: &str, events: &str, spans: &str, terminal: bool) -> String {
    let task = serde_json::from_str::<TaskSummary>(task).unwrap();
    let events = serde_json::from_str::<Vec<TaskEvent>>(events).unwrap();
    let spans = serde_json::from_str::<Vec<TaskSpan>>(spans).unwrap();
    let event = TaskWaitEvent::new(task, events, spans, terminal).unwrap();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: task\ndata: {}\n\n",
        serde_json::to_string(&event).unwrap()
    )
}
