use std::io::{BufRead, BufReader, Read, Write};

use serde::de::DeserializeOwned;
use serde_json::Value;
use verbatim_core::api::{
    AskCitationEvent, AskErrorEvent, AskResponse, AskTokenEvent, TaskWaitEvent,
};

use crate::client::{CliError, CliResult};
use crate::render;

pub fn consume_ask_sse<R, W>(reader: R, stdout: &mut W) -> CliResult<()>
where
    R: Read,
    W: Write,
{
    let mut consumer = SseConsumer::new(stdout);
    consumer.consume(reader)
}

#[derive(Debug, Default)]
pub struct TaskSseReport {
    pub last_event: Option<TaskWaitEvent>,
}

#[derive(Debug)]
pub struct TaskSseError {
    source: CliError,
    last_event: Option<Box<TaskWaitEvent>>,
}

impl TaskSseError {
    pub fn into_parts(self) -> (CliError, Option<TaskWaitEvent>) {
        (self.source, self.last_event.map(|event| *event))
    }
}

pub fn consume_task_sse<R, W>(reader: R, stdout: &mut W) -> Result<TaskSseReport, TaskSseError>
where
    R: Read,
    W: Write,
{
    let mut consumer = TaskSseConsumer::new(stdout);
    consumer.consume(reader)
}

struct SseConsumer<'a, W> {
    stdout: &'a mut W,
}

struct TaskSseConsumer<'a, W> {
    stdout: &'a mut W,
    wrote_status: bool,
    last_event: Option<TaskWaitEvent>,
}

impl<'a, W> TaskSseConsumer<'a, W>
where
    W: Write,
{
    fn new(stdout: &'a mut W) -> Self {
        Self {
            stdout,
            wrote_status: false,
            last_event: None,
        }
    }

    fn consume<R>(&mut self, reader: R) -> Result<TaskSseReport, TaskSseError>
    where
        R: Read,
    {
        let mut reader = BufReader::new(reader);
        let mut frame = String::new();
        loop {
            let mut line = String::new();
            let read = reader
                .read_line(&mut line)
                .map_err(|error| self.consume_error(error.into()))?;
            if read == 0 {
                break;
            }

            trim_line_ending(&mut line);
            if line.is_empty() {
                self.handle_frame(&frame)
                    .map_err(|error| self.consume_error(error))?;
                frame.clear();
            } else {
                if !frame.is_empty() {
                    frame.push('\n');
                }
                frame.push_str(&line);
            }
        }

        if !frame.trim().is_empty() {
            self.handle_frame(&frame)
                .map_err(|error| self.consume_error(error))?;
        }

        Ok(TaskSseReport {
            last_event: self.last_event.clone(),
        })
    }

    fn handle_frame(&mut self, frame: &str) -> CliResult<()> {
        let Some(frame) = parse_frame(frame) else {
            return Ok(());
        };

        match frame.event.as_deref().unwrap_or("message") {
            "task" => {
                let event: TaskWaitEvent = decode_event("task", &frame.data)?;
                self.last_event = Some(event.clone());
                if event.events.is_empty() && !self.wrote_status {
                    render::write_task_status_line(self.stdout, &event.task)?;
                    self.wrote_status = true;
                }
                render::write_task_events(self.stdout, &event.events)?;
                if !event.terminal && event.events.is_empty() {
                    render::write_task_progress_line(self.stdout, &event.task)?;
                }
                if event.terminal {
                    render::write_task_summary(self.stdout, &event.task, &event.spans)?;
                }
                Ok(())
            }
            "error" => Err(stream_error(&frame.data)),
            _ => Ok(()),
        }
    }

    fn consume_error(&self, source: CliError) -> TaskSseError {
        TaskSseError {
            source,
            last_event: self.last_event.clone().map(Box::new),
        }
    }
}

impl<'a, W> SseConsumer<'a, W>
where
    W: Write,
{
    fn new(stdout: &'a mut W) -> Self {
        Self { stdout }
    }

    fn consume<R>(&mut self, reader: R) -> CliResult<()>
    where
        R: Read,
    {
        let mut reader = BufReader::new(reader);
        let mut frame = String::new();
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line)?;
            if read == 0 {
                break;
            }

            trim_line_ending(&mut line);
            if line.is_empty() {
                self.handle_frame(&frame)?;
                frame.clear();
            } else {
                if !frame.is_empty() {
                    frame.push('\n');
                }
                frame.push_str(&line);
            }
        }

        if !frame.trim().is_empty() {
            self.handle_frame(&frame)?;
        }

        Ok(())
    }

    fn handle_frame(&mut self, frame: &str) -> CliResult<()> {
        let Some(frame) = parse_frame(frame) else {
            return Ok(());
        };

        match frame.event.as_deref().unwrap_or("message") {
            "token" => {
                let token: AskTokenEvent = decode_event("token", &frame.data)?;
                write!(self.stdout, "{}", token.text)?;
                self.stdout.flush()?;
                Ok(())
            }
            "citation" => {
                let citation: AskCitationEvent = decode_event("citation", &frame.data)?;
                render::write_citations(self.stdout, &citation.citations)?;
                Ok(())
            }
            "retrieval" => {
                let debug: Value = decode_event("retrieval", &frame.data)?;
                render::write_retrieval_debug(self.stdout, &debug)?;
                Ok(())
            }
            "error" => Err(stream_error(&frame.data)),
            "answer" => {
                let response: AskResponse = decode_event("answer", &frame.data)?;
                render::write_ask_response(self.stdout, &response)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
    }
    if line.ends_with('\r') {
        line.pop();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SseFrame {
    event: Option<String>,
    data: String,
}

fn parse_frame(frame: &str) -> Option<SseFrame> {
    let mut event = None;
    let mut data = Vec::new();

    for line in frame.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_string());
        }
    }

    if event.is_none() && data.is_empty() {
        return None;
    }

    Some(SseFrame {
        event,
        data: data.join("\n"),
    })
}

fn decode_event<T>(event: &'static str, data: &str) -> CliResult<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(data)
        .map_err(|error| CliError::Api(format!("daemon sent invalid {event} event: {error}")))
}

fn stream_error(data: &str) -> CliError {
    if let Ok(error) = serde_json::from_str::<AskErrorEvent>(data) {
        let prefix = error
            .status
            .map(|status| format!("daemon stream returned HTTP {status}: "))
            .unwrap_or_default();
        return CliError::Api(format!("{prefix}{}", error.error));
    }

    CliError::Api(format!("daemon stream error: {data}"))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{self, Cursor, Read, Write};

    use verbatim_core::api::{AskTokenEvent, TaskWaitEvent};
    use verbatim_core::task::{TaskEvent, TaskSpan, TaskSummary};

    use super::*;

    fn task_wait_stream(task: &str, events: &str, spans: &str, terminal: bool) -> String {
        let task = serde_json::from_str::<TaskSummary>(task).unwrap();
        let events = serde_json::from_str::<Vec<TaskEvent>>(events).unwrap();
        let spans = serde_json::from_str::<Vec<TaskSpan>>(spans).unwrap();
        let event = TaskWaitEvent::new(task, events, spans, terminal).unwrap();
        format!(
            "event: task\ndata: {}\n\n",
            serde_json::to_string(&event).unwrap()
        )
    }

    fn token_frame(text: &str) -> String {
        format!(
            "event: token\ndata: {}\n\n",
            serde_json::to_string(&AskTokenEvent::new(text).unwrap()).unwrap()
        )
    }

    #[test]
    fn parses_named_data_frame() {
        let data = serde_json::to_string(&AskTokenEvent::new("hi").unwrap()).unwrap();
        let frame = parse_frame(&format!("event: token\ndata: {data}\n")).unwrap();

        assert_eq!(
            frame,
            SseFrame {
                event: Some("token".into()),
                data,
            }
        );
    }

    #[test]
    fn consumes_tokens_as_frames_arrive_and_renders_citations() {
        let stream = format!(
            "{}{}{}",
            token_frame("Hel"),
            token_frame("lo [E1]."),
            "event: citation\ndata: {\"citations\":[{\"label\":\"E1\",\"evidence_id\":\"ev-1\",\"kind\":\"original_text\",\"role\":\"original_text\",\"derived_from\":null,\"locator\":\"PDF p.1 para.1\",\"text_preview\":\"preview\"}],\"verified\":false}\n\n",
        );
        let mut stdout = FlushRecorder::default();

        consume_ask_sse(Cursor::new(stream), &mut stdout).unwrap();

        assert_eq!(stdout.flushes, 2);
        let output = String::from_utf8(stdout.bytes).unwrap();
        assert!(output.starts_with("Hello [E1]."));
        assert!(output.contains("Citations:"));
        assert!(output.contains("[E1] evidence=ev-1"));
    }

    #[test]
    fn consumes_split_utf8_code_points_without_replacement() {
        let stream = token_frame("streamed 你好 π");
        let split = stream
            .find("你")
            .expect("test stream contains multibyte text")
            + 1;
        let chunks = vec![
            stream.as_bytes()[..split].to_vec(),
            stream.as_bytes()[split..].to_vec(),
        ];
        let mut stdout = FlushRecorder::default();

        consume_ask_sse(ChunkedReader::new(chunks), &mut stdout).unwrap();

        let output = String::from_utf8(stdout.bytes).unwrap();
        assert_eq!(output, "streamed 你好 π");
        assert!(!output.contains('\u{fffd}'));
    }

    #[test]
    fn error_event_returns_cli_error() {
        let stream = "event: error\ndata: {\"status\":500,\"error\":\"model failed\"}\n\n";
        let mut stdout = Vec::new();

        let error = consume_ask_sse(Cursor::new(stream), &mut stdout).unwrap_err();

        assert_eq!(error.exit_code(), 1);
        assert!(error.to_string().contains("HTTP 500"));
        assert!(stdout.is_empty());
    }

    #[test]
    fn consumes_task_wait_events_and_renders_terminal_summary() {
        let stream = task_wait_stream(
            r#"{"id":"task-1","kind":"ask","status":"succeeded","created_at":"1","updated_at":"2","started_at":"1","finished_at":"2","request":{"question_chars":4},"result":{"citation_count":1},"error":null}"#,
            r#"[{"sequence":2,"task_id":"task-1","event_type":"phase","message":"retrieval complete","payload":{"result_count":1},"created_at":"2"}]"#,
            r#"[{"sequence":1,"task_id":"task-1","phase":"retrieval","started_at":"1","duration_ms":8,"metadata":{"result_count":1}}]"#,
            true,
        );
        let mut stdout = Vec::new();

        consume_task_sse(Cursor::new(stream), &mut stdout).unwrap();

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("[2] phase: retrieval complete"));
        assert!(output.contains("Task: task-1"));
        assert!(output.contains("retrieval 8ms"));
        assert!(!output.contains("Raw answer"));
    }

    #[test]
    fn consumes_task_wait_tick_and_renders_live_progress() {
        let stream = task_wait_stream(
            r#"{"id":"task-1","kind":"ask","status":"running","created_at":"1","updated_at":"2","started_at":"1","finished_at":null,"request":{"question_chars":4},"result":null,"error":null,"progress":{"phase":{"name":"chat","started_at":"1","elapsed_ms":2000},"counters":[{"name":"chat_bytes_streamed","completed":12}],"endpoints":[{"name":"chat","calls":1,"latest_latency_ms":2000,"first_token_latency_ms":300,"p50_latency_ms":2000,"p95_latency_ms":2000}],"active_worker_kind":"ask","recent_status":"streaming"}}"#,
            "[]",
            "[]",
            false,
        );
        let mut stdout = Vec::new();

        consume_task_sse(Cursor::new(stream), &mut stdout).unwrap();

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("Task task-1 status=running"));
        assert!(output.contains("progress: phase=chat elapsed=2000ms"));
        assert!(output.contains("chat.first_token=300ms"));
        assert!(!output.contains("Task: task-1"));
    }

    #[derive(Default)]
    struct FlushRecorder {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for FlushRecorder {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    struct ChunkedReader {
        chunks: VecDeque<Vec<u8>>,
    }

    impl ChunkedReader {
        fn new(chunks: Vec<Vec<u8>>) -> Self {
            Self {
                chunks: chunks.into(),
            }
        }
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(mut chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            let len = chunk.len().min(buf.len());
            buf[..len].copy_from_slice(&chunk[..len]);
            if len < chunk.len() {
                let rest = chunk.split_off(len);
                self.chunks.push_front(rest);
            }
            Ok(len)
        }
    }
}
