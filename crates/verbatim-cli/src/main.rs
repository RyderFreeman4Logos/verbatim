use std::env;
use std::io::Write;
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use verbatim_core::config::Config;

const SUPPORTED_COMMANDS: &[&str] = &["source", "ingest", "ask", "evidence", "config", "daemon"];

fn main() -> ExitCode {
    let mut stderr = std::io::stderr();
    match run(env::args().skip(1), &mut std::io::stdout(), &mut stderr) {
        Ok(code) => ExitCode::from(code),
        Err(code) => ExitCode::from(code),
    }
}

fn run<I, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> Result<u8, u8>
where
    I: IntoIterator,
    I::Item: Into<String>,
    W: Write,
    E: Write,
{
    run_with_ask_client(args, stdout, stderr, &HttpAskClient)
}

fn run_with_ask_client<I, W, E, C>(
    args: I,
    stdout: &mut W,
    stderr: &mut E,
    ask_client: &C,
) -> Result<u8, u8>
where
    I: IntoIterator,
    I::Item: Into<String>,
    W: Write,
    E: Write,
    C: AskClient,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            write_help(stdout).map_err(|_| 1)?;
            Ok(0)
        }
        Some("-V" | "--version") => {
            writeln!(stdout, "verbatim {}", env!("CARGO_PKG_VERSION")).map_err(|_| 1)?;
            Ok(0)
        }
        Some("ask") => run_ask(&args[1..], stdout, stderr, ask_client),
        Some(command) if SUPPORTED_COMMANDS.contains(&command) => {
            writeln!(
                stderr,
                "verbatim {command}: CLI thin-client command is not implemented in this MVP. Use verbatim-daemon's REST API directly until issue #14 implements the thin client."
            )
            .map_err(|_| 1)?;
            Err(2)
        }
        Some(command) => {
            writeln!(stderr, "unknown verbatim command: {command}").map_err(|_| 1)?;
            write_help(stderr).map_err(|_| 1)?;
            Err(2)
        }
    }
}

fn run_ask<W, E, C>(
    args: &[String],
    stdout: &mut W,
    stderr: &mut E,
    ask_client: &C,
) -> Result<u8, u8>
where
    W: Write,
    E: Write,
    C: AskClient,
{
    let Some(command) = parse_ask_args(args).map_err(|message| {
        let _ = writeln!(stderr, "verbatim ask: {message}");
        let _ = write_ask_help(stderr);
        2
    })?
    else {
        write_ask_help(stdout).map_err(|_| 1)?;
        return Ok(0);
    };

    let request = AskRequest {
        question: command.question,
        source_id: command.source_id,
        show_retrieval: command.show_retrieval,
    };
    let response = ask_client.ask(&request).map_err(|message| {
        let _ = writeln!(stderr, "verbatim ask: {message}");
        1
    })?;

    write_ask_response(stdout, &response).map_err(|_| 1)?;
    Ok(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AskCommand {
    question: String,
    source_id: Option<String>,
    show_retrieval: bool,
}

fn parse_ask_args(args: &[String]) -> Result<Option<AskCommand>, String> {
    let mut question_parts = Vec::new();
    let mut source_id = None;
    let mut show_retrieval = false;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--show-retrieval" => show_retrieval = true,
            "-s" | "--source-id" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("{arg} requires a source id"))?;
                source_id = Some(value.clone());
            }
            value if value.starts_with("--source-id=") => {
                source_id = Some(value.trim_start_matches("--source-id=").to_string());
            }
            value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
            value => question_parts.push(value.to_string()),
        }
        index += 1;
    }

    if question_parts.is_empty() {
        return Err("missing question".into());
    }

    Ok(Some(AskCommand {
        question: question_parts.join(" "),
        source_id,
        show_retrieval,
    }))
}

trait AskClient {
    fn ask(&self, request: &AskRequest) -> Result<AskResponse, String>;
}

struct HttpAskClient;

impl AskClient for HttpAskClient {
    fn ask(&self, request: &AskRequest) -> Result<AskResponse, String> {
        let config = Config::load().map_err(|error| format!("{error:#}"))?;
        let url = ask_url(&config.daemon.bind);
        let response = reqwest::blocking::Client::new()
            .post(url)
            .json(request)
            .send()
            .map_err(|error| format!("failed to call daemon: {error:#}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(format!("daemon returned HTTP {status}: {body}"));
        }

        response
            .json::<AskResponse>()
            .map_err(|error| format!("daemon returned invalid ask response: {error:#}"))
    }
}

fn ask_url(bind: &str) -> String {
    let base = if bind.starts_with("http://") || bind.starts_with("https://") {
        bind.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", bind.trim_end_matches('/'))
    };
    format!("{base}/api/ask")
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct AskRequest {
    question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_id: Option<String>,
    show_retrieval: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct AskResponse {
    answer: String,
    #[serde(default)]
    citations: Vec<CitationResponse>,
    verified: bool,
    #[serde(default)]
    retrieval: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct CitationResponse {
    label: String,
    evidence_id: String,
    kind: String,
    derived_from: Option<String>,
    locator: String,
    text_preview: String,
}

fn write_ask_response<W>(writer: &mut W, response: &AskResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "{}", response.answer)?;
    if !response.citations.is_empty() {
        writeln!(writer)?;
        writeln!(writer, "Citations:")?;
        for citation in &response.citations {
            let derived = citation
                .derived_from
                .as_ref()
                .map(|id| format!(" derived_from={id}"))
                .unwrap_or_default();
            writeln!(
                writer,
                "  [{}] evidence={} kind={} locator={}{}",
                citation.label, citation.evidence_id, citation.kind, citation.locator, derived
            )?;
        }
    }

    if let Some(debug) = &response.retrieval {
        write_retrieval_debug(writer, debug)?;
    }

    Ok(())
}

fn write_retrieval_debug<W>(writer: &mut W, debug: &Value) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer)?;
    writeln!(writer, "Retrieval Debug")?;
    write_stage_hits(writer, "BM25 hits", debug.get("bm25_hits"))?;
    write_stage_hits(writer, "Dense hits", debug.get("dense_hits"))?;
    write_fused_hits(writer, debug.get("rrf_fused_hits"))?;
    write_graph_hits(writer, debug.get("graph_expanded_hits"))?;
    write_reranker(writer, debug.get("reranker"))?;
    write_final_pack(writer, debug.get("final_evidence_pack"))?;
    Ok(())
}

fn write_stage_hits<W>(writer: &mut W, title: &str, hits: Option<&Value>) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer)?;
    writeln!(writer, "{title}:")?;
    let Some(items) = hits.and_then(Value::as_array) else {
        return writeln!(writer, "  (none)");
    };
    if items.is_empty() {
        return writeln!(writer, "  (none)");
    }
    for hit in items {
        writeln!(
            writer,
            "  {}. chunk={} source={} score={} evidence={}",
            value_usize(hit, "rank"),
            value_string(hit, "chunk_id"),
            value_string(hit, "source_id"),
            value_score(hit, "score"),
            value_string_list(hit.get("evidence_ids")),
        )?;
    }
    Ok(())
}

fn write_fused_hits<W>(writer: &mut W, hits: Option<&Value>) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer)?;
    writeln!(writer, "RRF fused hits:")?;
    let Some(items) = hits.and_then(Value::as_array) else {
        return writeln!(writer, "  (none)");
    };
    if items.is_empty() {
        return writeln!(writer, "  (none)");
    }
    for hit in items {
        writeln!(
            writer,
            "  {}. chunk={} source={} score={} dense_rank={} bm25_rank={} evidence={}",
            value_usize(hit, "rank"),
            value_string(hit, "chunk_id"),
            value_string(hit, "source_id"),
            value_score(hit, "score"),
            value_string(hit, "dense_rank"),
            value_string(hit, "bm25_rank"),
            value_string_list(hit.get("evidence_ids")),
        )?;
    }
    Ok(())
}

fn write_graph_hits<W>(writer: &mut W, hits: Option<&Value>) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer)?;
    writeln!(writer, "Graph-expanded hits:")?;
    let Some(items) = hits.and_then(Value::as_array) else {
        return writeln!(writer, "  (none)");
    };
    if items.is_empty() {
        return writeln!(writer, "  (none)");
    }
    for hit in items {
        writeln!(
            writer,
            "  {}. expanded={} seed={} hop={} score={} path={}",
            value_usize(hit, "result_rank"),
            value_string(hit, "expanded_chunk_id"),
            value_string(hit, "seed_chunk_id"),
            value_usize(hit, "hop_distance"),
            value_score(hit, "score"),
            graph_path(hit.get("path")),
        )?;
    }
    Ok(())
}

fn write_reranker<W>(writer: &mut W, reranker: Option<&Value>) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer)?;
    writeln!(writer, "Reranker:")?;
    let Some(reranker) = reranker else {
        return writeln!(writer, "  skipped");
    };
    let status = value_string(reranker, "status");
    let reason = value_string(reranker, "reason");
    if reason.is_empty() {
        writeln!(writer, "  {status}")?;
    } else {
        writeln!(writer, "  {status}: {reason}")?;
    }
    let scores = reranker
        .get("scores")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for score in scores {
        writeln!(
            writer,
            "  {}. chunk={} score={}",
            value_usize(score, "rank"),
            value_string(score, "chunk_id"),
            value_score(score, "score"),
        )?;
    }
    Ok(())
}

fn write_final_pack<W>(writer: &mut W, pack: Option<&Value>) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer)?;
    writeln!(writer, "Final evidence pack:")?;
    let Some(items) = pack.and_then(Value::as_array) else {
        return writeln!(writer, "  (none)");
    };
    if items.is_empty() {
        return writeln!(writer, "  (none)");
    }
    for item in items {
        let locator = item
            .get("locator")
            .map(|locator| value_string(locator, "display"))
            .unwrap_or_default();
        writeln!(
            writer,
            "  {} chunk={} evidence={} role={} locator={}",
            value_string(item, "label"),
            value_string(item, "chunk_id"),
            value_string(item, "evidence_id"),
            value_string(item, "role"),
            locator,
        )?;
    }
    Ok(())
}

fn graph_path(path: Option<&Value>) -> String {
    let Some(steps) = path.and_then(Value::as_array) else {
        return String::new();
    };
    steps
        .iter()
        .map(|step| {
            format!(
                "{}:{}:{}->{}",
                value_string(step, "edge_type"),
                value_string(step, "direction"),
                value_string(step, "from_node_id"),
                value_string(step, "to_node_id")
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn value_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn value_usize(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| value.try_into().ok())
        .unwrap_or_default()
}

fn value_score(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_f64)
        .map(|score| format!("{score:.4}"))
        .unwrap_or_default()
}

fn value_string_list(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

fn write_help<W>(writer: &mut W) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "verbatim {}\n\nUSAGE:\n    verbatim <COMMAND>\n\nCOMMANDS:\n    source     Manage sources (thin client pending #14)\n    ingest     Trigger ingestion (thin client pending #14)\n    ask        Ask the daemon\n    evidence   Inspect evidence (thin client pending #14)\n    config     Inspect or update config (thin client pending #14)\n    daemon     Manage daemon process/API (thin client pending #14)\n\nOPTIONS:\n    -h, --help       Print help\n    -V, --version    Print version\n\nUse `verbatim ask --help` for ask options. Other installable CLI commands intentionally fail explicitly until the thin daemon client is implemented.",
        env!("CARGO_PKG_VERSION")
    )
}

fn write_ask_help<W>(writer: &mut W) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "verbatim ask\n\nUSAGE:\n    verbatim ask [OPTIONS] <QUESTION>\n\nOPTIONS:\n    -s, --source-id <SOURCE_ID>    Restrict retrieval to one source\n        --show-retrieval          Show retrieval provenance and ranking debug output\n    -h, --help                    Print help"
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn version_prints_package_version() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run(["--version"], &mut stdout, &mut stderr).unwrap();

        assert_eq!(code, 0);
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("verbatim {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn non_ask_documented_commands_fail_explicitly_until_thin_client_exists() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run(["source", "list"], &mut stdout, &mut stderr).unwrap_err();

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        let error = String::from_utf8(stderr).unwrap();
        assert!(error.contains("verbatim source"));
        assert!(error.contains("not implemented"));
        assert!(error.contains("#14"));
    }

    #[test]
    fn ask_show_retrieval_posts_flag_and_renders_debug_sections() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let client = MockAskClient::new(AskResponse {
            answer: "Answer [E1].".into(),
            citations: vec![CitationResponse {
                label: "E1".into(),
                evidence_id: "ev-1".into(),
                kind: "original_text".into(),
                derived_from: None,
                locator: "/tmp/doc.txt L1".into(),
                text_preview: "short preview".into(),
            }],
            verified: false,
            retrieval: Some(sample_debug_json()),
        });

        let code = run_with_ask_client(
            [
                "ask",
                "--show-retrieval",
                "-s",
                "src-1",
                "What",
                "is",
                "cited?",
            ],
            &mut stdout,
            &mut stderr,
            &client,
        )
        .unwrap();

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(
            client.last_request.borrow().as_ref().unwrap(),
            &AskRequest {
                question: "What is cited?".into(),
                source_id: Some("src-1".into()),
                show_retrieval: true,
            }
        );
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("Answer [E1]."));
        assert!(output.contains("Retrieval Debug"));
        assert!(output.contains("BM25 hits:"));
        assert!(output.contains("Dense hits:"));
        assert!(output.contains("RRF fused hits:"));
        assert!(output.contains("Graph-expanded hits:"));
        assert!(output.contains("Reranker:"));
        assert!(output.contains("Final evidence pack:"));
        assert!(!output.contains("secret full raw source text"));
    }

    #[test]
    fn ask_without_show_retrieval_keeps_output_clean() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let client = MockAskClient::new(AskResponse {
            answer: "Answer [E1].".into(),
            citations: Vec::new(),
            verified: false,
            retrieval: None,
        });

        let code =
            run_with_ask_client(["ask", "What is cited?"], &mut stdout, &mut stderr, &client)
                .unwrap();

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert!(
            !client
                .last_request
                .borrow()
                .as_ref()
                .unwrap()
                .show_retrieval
        );
        let output = String::from_utf8(stdout).unwrap();
        assert_eq!(output, "Answer [E1].\n");
    }

    #[test]
    fn unknown_commands_fail_instead_of_being_ignored() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run(["unknown"], &mut stdout, &mut stderr).unwrap_err();

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        let error = String::from_utf8(stderr).unwrap();
        assert!(error.contains("unknown verbatim command: unknown"));
        assert!(error.contains("USAGE:"));
    }

    struct MockAskClient {
        response: AskResponse,
        last_request: RefCell<Option<AskRequest>>,
    }

    impl MockAskClient {
        fn new(response: AskResponse) -> Self {
            Self {
                response,
                last_request: RefCell::new(None),
            }
        }
    }

    impl AskClient for MockAskClient {
        fn ask(&self, request: &AskRequest) -> Result<AskResponse, String> {
            self.last_request.replace(Some(request.clone()));
            Ok(self.response.clone())
        }
    }

    fn sample_debug_json() -> Value {
        serde_json::json!({
            "bm25_hits": [
                {
                    "rank": 1,
                    "chunk_id": "chunk-1",
                    "source_id": "src-1",
                    "score": 4.2,
                    "evidence_ids": ["ev-1"]
                }
            ],
            "dense_hits": [
                {
                    "rank": 1,
                    "chunk_id": "chunk-1",
                    "source_id": "src-1",
                    "score": 0.9,
                    "evidence_ids": ["ev-1"]
                }
            ],
            "rrf_fused_hits": [
                {
                    "rank": 1,
                    "chunk_id": "chunk-1",
                    "source_id": "src-1",
                    "score": 0.03,
                    "dense_rank": 1,
                    "bm25_rank": 1,
                    "evidence_ids": ["ev-1"]
                }
            ],
            "graph_expanded_hits": [
                {
                    "result_rank": 2,
                    "seed_chunk_id": "chunk-1",
                    "expanded_chunk_id": "chunk-2",
                    "hop_distance": 1,
                    "score": 0.01,
                    "path": [
                        {
                            "edge_type": "next",
                            "direction": "outgoing",
                            "from_node_id": "node-1",
                            "to_node_id": "node-2"
                        }
                    ]
                }
            ],
            "reranker": {
                "status": "skipped",
                "reason": "disabled",
                "scores": []
            },
            "final_evidence_pack": [
                {
                    "label": "E1",
                    "chunk_id": "chunk-1",
                    "evidence_id": "ev-1",
                    "role": "original_text",
                    "locator": {
                        "display": "/tmp/doc.txt L1"
                    }
                }
            ]
        })
    }
}
