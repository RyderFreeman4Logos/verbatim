use std::env;
use std::io::Write;
use std::process::ExitCode;

use clap::{error::ErrorKind, ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use verbatim_core::api::{AskRequest, ReindexRequest, RetrieveRequest};

mod client;
mod local;
mod render;
mod sse;

use client::{CliError, DaemonClient, HttpDaemonClient};
use local::{LocalActions, RealLocalActions};

fn main() -> ExitCode {
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    match run(env::args().skip(1), &mut stdout, &mut stderr) {
        Ok(code) | Err(code) => ExitCode::from(code),
    }
}

fn run<I, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> Result<u8, u8>
where
    I: IntoIterator,
    I::Item: Into<String>,
    W: Write,
    E: Write,
{
    let client = HttpDaemonClient::new();
    let local = RealLocalActions;
    run_with(args, stdout, stderr, &client, &local)
}

fn run_with<I, W, E, C, L>(
    args: I,
    stdout: &mut W,
    stderr: &mut E,
    client: &C,
    local: &L,
) -> Result<u8, u8>
where
    I: IntoIterator,
    I::Item: Into<String>,
    W: Write,
    E: Write,
    C: DaemonClient,
    L: LocalActions,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    if args.is_empty() {
        let mut command = Cli::command();
        command.write_help(stdout).map_err(|_| 1)?;
        writeln!(stdout).map_err(|_| 1)?;
        return Ok(0);
    }

    let argv = std::iter::once("verbatim".to_string())
        .chain(args)
        .collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(error) => {
            let code = clap_exit_code(&error);
            if code == 0 {
                write!(stdout, "{}", error.render()).map_err(|_| 1)?;
                return Ok(0);
            }
            write!(stderr, "{}", error.render()).map_err(|_| 1)?;
            return Err(code);
        }
    };

    match dispatch(cli, stdout, client, local) {
        Ok(code) => Ok(code),
        Err(error) => {
            writeln!(stderr, "{error}").map_err(|_| 1)?;
            Err(error.exit_code())
        }
    }
}

fn clap_exit_code(error: &clap::Error) -> u8 {
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
        _ => 2,
    }
}

fn rerank_override(rerank: bool, no_rerank: bool) -> Option<bool> {
    match (rerank, no_rerank) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn write_retrieve_with_format<W>(
    stdout: &mut W,
    response: &verbatim_core::api::RetrieveResponse,
    format: RetrieveFormat,
) -> std::io::Result<()>
where
    W: Write,
{
    match format {
        RetrieveFormat::Markdown => render::write_retrieve_response(stdout, response),
        RetrieveFormat::Json => render::write_retrieve_json(stdout, response),
    }
}

fn parse_nonzero_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("must be a positive integer: {error}"))?;
    if parsed == 0 {
        return Err("must be greater than zero".into());
    }
    Ok(parsed)
}

fn dispatch<W, C, L>(cli: Cli, stdout: &mut W, client: &C, local: &L) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
    L: LocalActions,
{
    match cli.command {
        Commands::Source { command } => run_source(command, stdout, client),
        Commands::Ingest {
            source_id,
            force,
            background,
            embedding_profile,
            vectors_only,
        } => {
            if background {
                let response = client.submit_ingest_task(
                    source_id.as_deref(),
                    force,
                    embedding_profile.as_deref(),
                    vectors_only,
                )?;
                render::write_task_created(stdout, &response)?;
            } else {
                let response = client.ingest(
                    source_id.as_deref(),
                    force,
                    embedding_profile.as_deref(),
                    vectors_only,
                )?;
                render::write_ingest(stdout, &response)?;
            }
            Ok(0)
        }
        Commands::Reindex {
            source_id,
            all,
            stale,
            force,
            background,
            embedding_profile,
            vectors_only,
        } => {
            let request = ReindexRequest {
                source_id,
                all,
                stale,
                force,
                embedding_profile_id: embedding_profile,
                vectors_only,
            };
            if background {
                let response = client.submit_reindex_task(&request)?;
                render::write_task_created(stdout, &response)?;
            } else {
                let response = client.reindex(&request)?;
                render::write_reindex(stdout, &response)?;
            }
            Ok(0)
        }
        Commands::Ask {
            question,
            source_id,
            embedding_profile,
            show_retrieval,
            context_only,
            no_generate,
            format,
            background,
        } => {
            let context_only = context_only || no_generate;
            let question = question.join(" ");
            if context_only {
                if background {
                    return Err(CliError::Api(
                        "--background is not supported with --context-only".into(),
                    ));
                }
                let format = format.unwrap_or(RetrieveFormat::Markdown);
                let request = RetrieveRequest {
                    question,
                    source_id,
                    embedding_profile_id: embedding_profile,
                    limit: None,
                    page_size: None,
                    page: None,
                    fast: false,
                    rerank: None,
                    dense_top_k: None,
                    bm25_top_k: None,
                    rerank_top_n: None,
                    include_debug: show_retrieval,
                    include_locator: format == RetrieveFormat::Json,
                };
                let response = client.retrieve(&request)?;
                write_retrieve_with_format(stdout, &response, format)?;
                return Ok(0);
            }

            if format.is_some() {
                return Err(CliError::Api(
                    "--format is only supported with --context-only or --no-generate".into(),
                ));
            }

            let request = AskRequest {
                question,
                source_id,
                embedding_profile_id: embedding_profile,
                show_retrieval,
                context_only: false,
            };
            if background {
                let response = client.submit_ask_task(&request)?;
                render::write_task_created(stdout, &response)?;
            } else {
                client.ask(&request, stdout)?;
                writeln!(stdout)?;
            }
            Ok(0)
        }
        Commands::Retrieve {
            question,
            source_id,
            embedding_profile,
            limit,
            page_size,
            page,
            fast,
            rerank,
            no_rerank,
            dense_top_k,
            bm25_top_k,
            rerank_top_n,
            show_debug,
            show_locator,
            format,
        } => {
            let include_locator = show_locator || format == RetrieveFormat::Json;
            let request = RetrieveRequest {
                question: question.join(" "),
                source_id,
                embedding_profile_id: embedding_profile,
                limit,
                page_size,
                page,
                fast,
                rerank: rerank_override(rerank, no_rerank),
                dense_top_k,
                bm25_top_k,
                rerank_top_n,
                include_debug: show_debug,
                include_locator,
            };
            let response = client.retrieve(&request)?;
            write_retrieve_with_format(stdout, &response, format)?;
            Ok(0)
        }
        Commands::Evidence { eid } => {
            let evidence = client.get_evidence(&eid)?;
            render::write_evidence(stdout, &evidence)?;
            Ok(0)
        }
        Commands::Config { command } => run_config(command, stdout, client, local),
        Commands::Daemon { command } => run_daemon(command, stdout, client, local),
        Commands::Task { command } => run_task(command, stdout, client),
    }
}

fn run_source<W, C>(command: SourceCommand, stdout: &mut W, client: &C) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
{
    match command {
        SourceCommand::Add { path } => {
            let response = client.add_source(&path)?;
            writeln!(stdout, "Added source: {}", response.id)?;
        }
        SourceCommand::List => {
            let sources = client.list_sources()?;
            render::write_sources(stdout, &sources)?;
        }
        SourceCommand::Inspect { id } => {
            let source = client.get_source(&id)?;
            render::write_source(stdout, &source)?;
        }
        SourceCommand::Remove { id } => {
            client.remove_source(&id)?;
            writeln!(stdout, "Removed source: {id}")?;
        }
        SourceCommand::Check => {
            let response = client.check_sources()?;
            render::write_check_stale(stdout, &response)?;
        }
    }
    Ok(0)
}

fn run_config<W, C, L>(
    command: ConfigCommand,
    stdout: &mut W,
    client: &C,
    local: &L,
) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
    L: LocalActions,
{
    match command {
        ConfigCommand::Init => {
            let path = local.config_init()?;
            local::write_config_init(stdout, &path)?;
        }
        ConfigCommand::Show => {
            let config = client.get_config()?;
            render::write_config(stdout, &config)?;
        }
        ConfigCommand::Validate => {
            let path = local.config_validate()?;
            local::write_config_validate(stdout, &path)?;
        }
    }
    Ok(0)
}

fn run_daemon<W, C, L>(
    command: DaemonCommand,
    stdout: &mut W,
    client: &C,
    local: &L,
) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
    L: LocalActions,
{
    match command {
        DaemonCommand::Start => local.daemon_start(),
        DaemonCommand::Status => {
            let health = client.health()?;
            render::write_health(stdout, &health)?;
            Ok(0)
        }
        DaemonCommand::Install { force } => {
            let path = local.daemon_install(force)?;
            local::write_daemon_install(stdout, &path)?;
            Ok(0)
        }
    }
}

fn run_task<W, C>(command: TaskCommand, stdout: &mut W, client: &C) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
{
    match command {
        TaskCommand::Show { task_id } => {
            let response = client.get_task(&task_id)?;
            render::write_task_summary(stdout, &response.task, &response.spans)?;
        }
        TaskCommand::Events { task_id, after } => {
            let response = client.get_task_events(&task_id, after)?;
            render::write_task_events(stdout, &response.events)?;
        }
        TaskCommand::Wait { task_id, after } => {
            client.wait_task(&task_id, after, stdout)?;
        }
        TaskCommand::Cancel { task_id } => {
            let response = client.cancel_task(&task_id)?;
            render::write_task_summary(stdout, &response.task, &response.spans)?;
        }
    }
    Ok(0)
}

#[derive(Debug, Parser)]
#[command(
    name = "verbatim",
    version,
    about = "Grounded document Q&A with traceable citations"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Manage sources through the daemon API.
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// Trigger ingestion through the daemon API.
    Ingest {
        /// Optional source id. Omit to ingest all pending/stale sources.
        source_id: Option<String>,
        /// Re-ingest all sources, including already indexed sources.
        #[arg(long)]
        force: bool,
        /// Build vectors for this embedding profile from existing chunks.
        #[arg(long = "embedding-profile")]
        embedding_profile: Option<String>,
        /// Build only profile vectors/indexes without re-parsing sources.
        #[arg(long)]
        vectors_only: bool,
        /// Queue ingest as a persistent daemon task and return immediately.
        #[arg(long)]
        background: bool,
    },
    /// Rebuild derived indexes for existing sources without adding sources.
    Reindex {
        /// Reindex one existing source id.
        #[arg(long = "source-id", conflicts_with_all = ["all", "stale"])]
        source_id: Option<String>,
        /// Reindex all existing sources.
        #[arg(long, conflicts_with_all = ["source_id", "stale"])]
        all: bool,
        /// Reindex sources reported stale by source check.
        #[arg(long, conflicts_with_all = ["source_id", "all"])]
        stale: bool,
        /// Force all-source reindex. Redundant with --all.
        #[arg(long)]
        force: bool,
        /// Build vectors for this embedding profile from existing chunks.
        #[arg(long = "embedding-profile")]
        embedding_profile: Option<String>,
        /// Build only profile vectors/indexes without re-parsing sources.
        #[arg(long)]
        vectors_only: bool,
        /// Queue reindex as a persistent daemon task and return immediately.
        #[arg(long)]
        background: bool,
    },
    /// Generate an answer through chat, or return a context pack with --context-only.
    Ask {
        /// Restrict retrieval to one source.
        #[arg(short = 's', long = "source-id")]
        source_id: Option<String>,
        /// Use this embedding profile for retrieval.
        #[arg(long = "embedding-profile")]
        embedding_profile: Option<String>,
        /// Show retrieval provenance and ranking debug output.
        #[arg(long)]
        show_retrieval: bool,
        /// Return a retrieval context pack instead of invoking chat generation.
        #[arg(long = "context-only", action = ArgAction::SetTrue)]
        context_only: bool,
        /// Alias for --context-only.
        #[arg(long = "no-generate", action = ArgAction::SetTrue)]
        no_generate: bool,
        /// Context-only output format. JSON includes structured locator/provenance fields.
        #[arg(long, value_enum)]
        format: Option<RetrieveFormat>,
        /// Queue ask as a persistent daemon task and return immediately.
        #[arg(long)]
        background: bool,
        /// Question text.
        #[arg(required = true, num_args = 1..)]
        question: Vec<String>,
    },
    /// Retrieve a compact context/evidence pack without invoking chat generation.
    Retrieve {
        /// Restrict retrieval to one source.
        #[arg(short = 's', long = "source-id")]
        source_id: Option<String>,
        /// Use this embedding profile for retrieval.
        #[arg(long = "embedding-profile")]
        embedding_profile: Option<String>,
        /// Maximum evidence/context entries to consider before pagination.
        #[arg(long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Evidence/context entries per page. Use 1 for agent-sized pages.
        #[arg(long, value_parser = parse_nonzero_usize)]
        page_size: Option<usize>,
        /// 1-based page number.
        #[arg(long, value_parser = parse_nonzero_usize)]
        page: Option<usize>,
        /// Use a faster retrieval preset: lower top-k and no rerank unless overridden.
        #[arg(long)]
        fast: bool,
        /// Enable reranking for this request even if the config default is off.
        #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_rerank")]
        rerank: bool,
        /// Disable reranking for this request even if the config default is on.
        #[arg(long = "no-rerank", action = ArgAction::SetTrue)]
        no_rerank: bool,
        /// Override dense vector candidate count for this request.
        #[arg(long = "dense-top-k", value_parser = parse_nonzero_usize)]
        dense_top_k: Option<usize>,
        /// Override BM25 candidate count for this request.
        #[arg(long = "bm25-top-k", value_parser = parse_nonzero_usize)]
        bm25_top_k: Option<usize>,
        /// Override reranker top-n. Use 0 to disable reranking.
        #[arg(long = "rerank-top-n")]
        rerank_top_n: Option<usize>,
        /// Include retrieval stage debug metadata in the response.
        #[arg(long = "show-debug")]
        show_debug: bool,
        /// Include structured locator/provenance fields in the response.
        #[arg(long = "show-locator")]
        show_locator: bool,
        /// Output format. JSON includes structured locator/provenance fields.
        #[arg(long, value_enum, default_value = "markdown")]
        format: RetrieveFormat,
        /// Question text.
        #[arg(required = true, num_args = 1..)]
        question: Vec<String>,
    },
    /// Inspect one evidence unit through the daemon API.
    Evidence {
        /// Evidence id.
        eid: String,
    },
    /// Inspect or update configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage daemon process/API helpers.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Inspect and wait for persistent daemon tasks.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SourceCommand {
    /// Add a source path.
    Add {
        /// File path to add.
        path: String,
    },
    /// List sources.
    List,
    /// Inspect one source.
    Inspect {
        /// Source id.
        id: String,
    },
    /// Remove one source.
    Remove {
        /// Source id.
        id: String,
    },
    /// Mark and list stale sources.
    Check,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Generate the default local config file.
    Init,
    /// Fetch redacted runtime config from the daemon.
    Show,
    /// Validate the local config file.
    Validate,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Start verbatim-daemon in the foreground.
    Start,
    /// Check daemon health through the daemon API.
    Status,
    /// Install the systemd user service.
    Install {
        /// Overwrite an existing service file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// Show task summary and phase spans.
    Show {
        /// Task id.
        task_id: String,
    },
    /// List bounded task events.
    Events {
        /// Task id.
        task_id: String,
        /// Only show events after this sequence.
        #[arg(long)]
        after: Option<i64>,
    },
    /// Wait for task events until the task reaches a terminal status.
    Wait {
        /// Task id.
        task_id: String,
        /// Only stream events after this sequence.
        #[arg(long)]
        after: Option<i64>,
    },
    /// Request best-effort task cancellation.
    Cancel {
        /// Task id.
        task_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RetrieveFormat {
    #[value(alias = "text")]
    Markdown,
    Json,
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use serde_json::Value;
    use verbatim_core::api::{
        AddSourceResponse, CheckStaleResponse, CitationResponse, ConfigResponse, EvidenceResponse,
        HealthResponse, IngestResponse, ReindexRequest, ReindexResponse, RetrieveControlsResponse,
        RetrieveRequest, RetrieveResponse, RetrieveResultResponse, RetrieveTimingResponse,
        SourceResponse, TaskCreatedResponse, TaskEventsResponse, TaskSummaryResponse,
    };
    use verbatim_core::task::{TaskEvent, TaskId, TaskKind, TaskSpan, TaskStatus, TaskSummary};
    use verbatim_core::types::SourceLocator;

    use super::*;

    #[test]
    fn version_prints_package_version() {
        let (code, stdout, stderr, _, _) = run_mock(["--version"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(stdout, format!("verbatim {}\n", env!("CARGO_PKG_VERSION")));
        assert!(stderr.is_empty());
    }

    #[test]
    fn help_is_available_for_every_command_path() {
        let cases: &[&[&str]] = &[
            &["--help"],
            &["source", "--help"],
            &["source", "add", "--help"],
            &["source", "list", "--help"],
            &["source", "inspect", "--help"],
            &["source", "remove", "--help"],
            &["source", "check", "--help"],
            &["ingest", "--help"],
            &["reindex", "--help"],
            &["ask", "--help"],
            &["retrieve", "--help"],
            &["evidence", "--help"],
            &["config", "--help"],
            &["config", "init", "--help"],
            &["config", "show", "--help"],
            &["config", "validate", "--help"],
            &["daemon", "--help"],
            &["daemon", "start", "--help"],
            &["daemon", "status", "--help"],
            &["daemon", "install", "--help"],
            &["task", "--help"],
            &["task", "show", "--help"],
            &["task", "events", "--help"],
            &["task", "wait", "--help"],
            &["task", "cancel", "--help"],
        ];

        for args in cases {
            let (code, stdout, stderr, _, _) = run_mock(args.iter().copied());
            assert_eq!(code.unwrap(), 0, "args: {args:?}");
            assert!(stdout.contains("Usage:"), "stdout: {stdout}");
            assert!(stderr.is_empty(), "stderr: {stderr}");
        }
    }

    #[test]
    fn ask_and_retrieve_help_distinguish_generation_from_context_only() {
        let (code, ask_help, stderr, _, _) = run_mock(["ask", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(ask_help.contains("Generate an answer through chat"));
        assert!(ask_help.contains("--context-only"));
        assert!(ask_help.contains("--no-generate"));
        assert!(ask_help.contains("--format"));

        let (code, retrieve_help, stderr, _, _) = run_mock(["retrieve", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(retrieve_help.contains("without invoking chat generation"));
        assert!(retrieve_help.contains("markdown"));
        assert!(retrieve_help.contains("json"));
    }

    #[test]
    fn source_add_and_list_call_daemon_client() {
        let (code, stdout, stderr, client, _) = run_mock(["source", "add", "/tmp/doc.pdf"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["add_source:/tmp/doc.pdf"]
        );
        assert!(stdout.contains("Added source: src-1"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock(["source", "list"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["list_sources"]);
        assert!(stdout.contains("Sources:"));
        assert!(stdout.contains("id=src-1"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn ingest_evidence_config_and_status_call_daemon_client() {
        let (code, _, _, client, _) = run_mock(["ingest", "--force"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["ingest:None:true:None:false"]
        );

        let (code, stdout, _, client, _) = run_mock(["evidence", "ev-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["get_evidence:ev-1"]);
        assert!(stdout.contains("Evidence:"));

        let (code, stdout, _, client, _) = run_mock(["config", "show"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["get_config"]);
        assert!(stdout.contains("\"daemon\""));

        let (code, stdout, _, client, _) = run_mock(["daemon", "status"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["health"]);
        assert!(stdout.contains("Daemon status: ok"));
    }

    #[test]
    fn background_ask_and_ingest_submit_tasks() {
        let (code, stdout, stderr, client, _) = run_mock(["ingest", "--background", "--force"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["submit_ingest_task:None:true:None:false"]
        );
        assert!(stdout.contains("Task queued: task-1"));
        assert!(stdout.contains("verbatim task wait task-1"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) =
            run_mock(["ask", "--background", "-s", "src-1", "What", "is", "cited?"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.last_ask.borrow().as_ref().unwrap(),
            &AskRequest {
                question: "What is cited?".into(),
                source_id: Some("src-1".into()),
                embedding_profile_id: None,
                show_retrieval: false,
                context_only: false,
            }
        );
        assert_eq!(client.calls.borrow().as_slice(), ["submit_ask_task"]);
        assert!(stdout.contains("Task queued: task-1"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn task_commands_call_daemon_client() {
        let (code, stdout, _, client, _) = run_mock(["task", "show", "task-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["get_task:task-1"]);
        assert!(stdout.contains("Task: task-1"));
        assert!(stdout.contains("spans:"));

        let (code, stdout, _, client, _) = run_mock(["task", "events", "task-1", "--after", "3"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["get_task_events:task-1:Some(3)"]
        );
        assert!(stdout.contains("[4] phase: retrieval complete"));

        let (code, _, _, client, _) = run_mock(["task", "wait", "task-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["wait_task:task-1:None"]);

        let (code, stdout, _, client, _) = run_mock(["task", "cancel", "task-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["cancel_task:task-1"]);
        assert!(stdout.contains("status: cancelled"));
    }

    #[test]
    fn ask_show_retrieval_remains_plumbed_and_rendered() {
        let (code, stdout, stderr, client, _) = run_mock([
            "ask",
            "--show-retrieval",
            "-s",
            "src-1",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(
            client.last_ask.borrow().as_ref().unwrap(),
            &AskRequest {
                question: "What is cited?".into(),
                source_id: Some("src-1".into()),
                embedding_profile_id: None,
                show_retrieval: true,
                context_only: false,
            }
        );
        assert!(stdout.contains("Answer [E1]."));
        assert!(stdout.contains("Retrieval Debug"));
        assert!(stdout.contains("Final evidence pack:"));
        assert!(!stdout.contains("secret full raw source text"));
    }

    #[test]
    fn retrieve_context_pack_uses_retrieve_api_not_ask_generation() {
        let (code, stdout, stderr, client, _) = run_mock([
            "retrieve",
            "--page-size",
            "1",
            "--page",
            "2",
            "--limit",
            "3",
            "--fast",
            "--no-rerank",
            "-s",
            "src-1",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["retrieve"]);
        assert!(client.last_ask.borrow().is_none());
        assert_eq!(
            client.last_retrieve.borrow().as_ref().unwrap(),
            &RetrieveRequest {
                question: "What is cited?".into(),
                source_id: Some("src-1".into()),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(2),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                include_debug: false,
                include_locator: false,
            }
        );
        assert!(stdout.contains("Context pack: task-1"));
        assert!(stdout.contains("[0] E1 score=0.0310"));
        assert!(stdout.contains("snippet: compact cited text"));
    }

    #[test]
    fn retrieve_json_requests_structured_locator_and_debug() {
        let (code, stdout, stderr, client, _) = run_mock([
            "retrieve",
            "--format",
            "json",
            "--show-debug",
            "--rerank",
            "--dense-top-k",
            "5",
            "--bm25-top-k",
            "7",
            "--rerank-top-n",
            "2",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.rerank, Some(true));
        assert_eq!(request.dense_top_k, Some(5));
        assert_eq!(request.bm25_top_k, Some(7));
        assert_eq!(request.rerank_top_n, Some(2));
        assert!(request.include_debug);
        assert!(request.include_locator);
        assert!(stdout.contains("\"structured_locator\""));
        assert!(stdout.contains("\"debug\""));
    }

    #[test]
    fn retrieve_markdown_show_debug_renders_retrieval_debug() {
        let (code, stdout, stderr, client, _) = run_mock([
            "retrieve",
            "--show-debug",
            "--format",
            "markdown",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["retrieve"]);
        assert!(
            client
                .last_retrieve
                .borrow()
                .as_ref()
                .unwrap()
                .include_debug
        );
        assert!(stdout.contains("Context pack: task-1"));
        assert!(stdout.contains("Retrieval Debug"));
        assert!(stdout.contains("Final evidence pack:"));
    }

    #[test]
    fn ask_context_only_renders_markdown_context_pack_without_generation() {
        let (code, stdout, stderr, client, _) = run_mock([
            "ask",
            "--context-only",
            "--show-retrieval",
            "-s",
            "src-1",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["retrieve"]);
        assert!(client.last_ask.borrow().is_none());
        assert_eq!(
            client.last_retrieve.borrow().as_ref().unwrap(),
            &RetrieveRequest {
                question: "What is cited?".into(),
                source_id: Some("src-1".into()),
                embedding_profile_id: None,
                limit: None,
                page_size: None,
                page: None,
                fast: false,
                rerank: None,
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                include_debug: true,
                include_locator: false,
            }
        );
        assert!(stdout.contains("Context pack: task-1"));
        assert!(stdout.contains("snippet: compact cited text"));
        assert!(stdout.contains("Retrieval Debug"));
        assert!(stdout.contains("Final evidence pack:"));
    }

    #[test]
    fn ask_no_generate_json_requests_structured_context_pack() {
        let (code, stdout, stderr, client, _) = run_mock([
            "ask",
            "--no-generate",
            "--format",
            "json",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["retrieve"]);
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert!(request.include_locator);
        assert!(!request.include_debug);
        assert!(stdout.contains("\"results\""));
        assert!(stdout.contains("\"structured_locator\""));
    }

    #[test]
    fn embedding_profile_flags_are_plumbed() {
        let (code, _, stderr, client, _) = run_mock([
            "ingest",
            "src-1",
            "--embedding-profile",
            "alt",
            "--vectors-only",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["ingest:Some(\"src-1\"):false:Some(\"alt\"):true"]
        );
        assert!(stderr.is_empty());

        let (code, _, stderr, client, _) =
            run_mock(["ask", "--embedding-profile", "alt", "What", "is", "cited?"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.last_ask.borrow().as_ref().unwrap(),
            &AskRequest {
                question: "What is cited?".into(),
                source_id: None,
                embedding_profile_id: Some("alt".into()),
                show_retrieval: false,
                context_only: false,
            }
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn reindex_flags_are_plumbed() {
        let (code, stdout, stderr, client, _) = run_mock([
            "reindex",
            "--source-id",
            "src-1",
            "--embedding-profile",
            "alt",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["reindex"]);
        assert_eq!(
            client.last_reindex.borrow().as_ref().unwrap(),
            &ReindexRequest {
                source_id: Some("src-1".into()),
                all: false,
                stale: false,
                force: false,
                embedding_profile_id: Some("alt".into()),
                vectors_only: false,
            }
        );
        assert!(stdout.contains("Reindexed 1 source(s)."));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock(["reindex", "--force"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["reindex"]);
        assert_eq!(
            client.last_reindex.borrow().as_ref().unwrap(),
            &ReindexRequest {
                source_id: None,
                all: false,
                stale: false,
                force: true,
                embedding_profile_id: None,
                vectors_only: false,
            }
        );
        assert!(stdout.contains("Reindexed 1 source(s)."));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) =
            run_mock(["reindex", "--all", "--vectors-only", "--background"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["submit_reindex_task"]);
        assert_eq!(
            client.last_reindex.borrow().as_ref().unwrap(),
            &ReindexRequest {
                source_id: None,
                all: true,
                stale: false,
                force: false,
                embedding_profile_id: None,
                vectors_only: true,
            }
        );
        assert!(stdout.contains("Task queued: task-1"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn daemon_unreachable_maps_to_exit_code_two() {
        let client = MockDaemonClient {
            health_error: Some(CliError::DaemonUnreachable(
                "could not reach daemon\nStart it with: systemctl --user start verbatim".into(),
            )),
            ..MockDaemonClient::default()
        };
        let local = MockLocalActions::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with(
            ["daemon", "status"],
            &mut stdout,
            &mut stderr,
            &client,
            &local,
        )
        .unwrap_err();

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(String::from_utf8(stderr)
            .unwrap()
            .contains("systemctl --user start verbatim"));
    }

    #[test]
    fn http_api_error_maps_to_exit_code_one() {
        let client = MockDaemonClient {
            list_error: Some(CliError::Api("daemon returned HTTP 500: body".into())),
            ..MockDaemonClient::default()
        };
        let local = MockLocalActions::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with(
            ["source", "list"],
            &mut stdout,
            &mut stderr,
            &client,
            &local,
        )
        .unwrap_err();

        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert!(String::from_utf8(stderr).unwrap().contains("HTTP 500"));
    }

    #[test]
    fn daemon_install_prints_generated_path_and_systemctl_commands() {
        let (code, stdout, stderr, _, local) = run_mock(["daemon", "install"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(local.calls.borrow().as_slice(), ["daemon_install:false"]);
        assert!(stdout.contains("Generated /tmp/verbatim.service"));
        assert!(stdout.contains("Run: systemctl --user daemon-reload"));
        assert!(stdout.contains("Run: systemctl --user enable --now verbatim"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn daemon_install_force_is_plumbed_to_local_action() {
        let (code, stdout, stderr, _, local) = run_mock(["daemon", "install", "--force"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(local.calls.borrow().as_slice(), ["daemon_install:true"]);
        assert!(stdout.contains("Generated /tmp/verbatim.service"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn daemon_install_help_documents_force() {
        let (code, stdout, stderr, _, _) = run_mock(["daemon", "install", "--help"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stdout.contains("--force"));
        assert!(stdout.contains("Overwrite an existing service file"));
        assert!(stderr.is_empty());
    }

    fn run_mock<I>(
        args: I,
    ) -> (
        Result<u8, u8>,
        String,
        String,
        MockDaemonClient,
        MockLocalActions,
    )
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with(args, &mut stdout, &mut stderr, &client, &local);
        (
            code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
            client,
            local,
        )
    }

    #[derive(Default)]
    struct MockDaemonClient {
        calls: RefCell<Vec<String>>,
        last_ask: RefCell<Option<AskRequest>>,
        last_retrieve: RefCell<Option<RetrieveRequest>>,
        last_reindex: RefCell<Option<ReindexRequest>>,
        list_error: Option<CliError>,
        health_error: Option<CliError>,
    }

    impl DaemonClient for MockDaemonClient {
        fn add_source(&self, path: &str) -> client::CliResult<AddSourceResponse> {
            self.calls.borrow_mut().push(format!("add_source:{path}"));
            Ok(AddSourceResponse { id: "src-1".into() })
        }

        fn list_sources(&self) -> client::CliResult<Vec<SourceResponse>> {
            if let Some(error) = &self.list_error {
                return Err(clone_cli_error(error));
            }
            self.calls.borrow_mut().push("list_sources".into());
            Ok(vec![sample_source()])
        }

        fn get_source(&self, id: &str) -> client::CliResult<SourceResponse> {
            self.calls.borrow_mut().push(format!("get_source:{id}"));
            Ok(sample_source())
        }

        fn remove_source(&self, id: &str) -> client::CliResult<()> {
            self.calls.borrow_mut().push(format!("remove_source:{id}"));
            Ok(())
        }

        fn check_sources(&self) -> client::CliResult<CheckStaleResponse> {
            self.calls.borrow_mut().push("check_sources".into());
            Ok(CheckStaleResponse {
                stale: vec!["src-1".into()],
            })
        }

        fn ingest(
            &self,
            source_id: Option<&str>,
            force: bool,
            embedding_profile_id: Option<&str>,
            vectors_only: bool,
        ) -> client::CliResult<IngestResponse> {
            self.calls.borrow_mut().push(format!(
                "ingest:{source_id:?}:{force}:{embedding_profile_id:?}:{vectors_only}"
            ));
            Ok(IngestResponse { ingested: 1 })
        }

        fn reindex(&self, request: &ReindexRequest) -> client::CliResult<ReindexResponse> {
            self.calls.borrow_mut().push("reindex".into());
            self.last_reindex.replace(Some(request.clone()));
            Ok(ReindexResponse { reindexed: 1 })
        }

        fn submit_ask_task(&self, request: &AskRequest) -> client::CliResult<TaskCreatedResponse> {
            self.calls.borrow_mut().push("submit_ask_task".into());
            self.last_ask.replace(Some(request.clone()));
            Ok(TaskCreatedResponse {
                task_id: "task-1".into(),
            })
        }

        fn submit_ingest_task(
            &self,
            source_id: Option<&str>,
            force: bool,
            embedding_profile_id: Option<&str>,
            vectors_only: bool,
        ) -> client::CliResult<TaskCreatedResponse> {
            self.calls.borrow_mut().push(format!(
                "submit_ingest_task:{source_id:?}:{force}:{embedding_profile_id:?}:{vectors_only}"
            ));
            Ok(TaskCreatedResponse {
                task_id: "task-1".into(),
            })
        }

        fn submit_reindex_task(
            &self,
            request: &ReindexRequest,
        ) -> client::CliResult<TaskCreatedResponse> {
            self.calls.borrow_mut().push("submit_reindex_task".into());
            self.last_reindex.replace(Some(request.clone()));
            Ok(TaskCreatedResponse {
                task_id: "task-1".into(),
            })
        }

        fn get_task(&self, task_id: &str) -> client::CliResult<TaskSummaryResponse> {
            self.calls.borrow_mut().push(format!("get_task:{task_id}"));
            Ok(sample_task_response(TaskStatus::Succeeded))
        }

        fn get_task_events(
            &self,
            task_id: &str,
            after: Option<i64>,
        ) -> client::CliResult<TaskEventsResponse> {
            self.calls
                .borrow_mut()
                .push(format!("get_task_events:{task_id}:{after:?}"));
            Ok(TaskEventsResponse {
                events: vec![sample_task_event()],
            })
        }

        fn wait_task<W>(
            &self,
            task_id: &str,
            after: Option<i64>,
            _stdout: &mut W,
        ) -> client::CliResult<()>
        where
            W: Write,
        {
            self.calls
                .borrow_mut()
                .push(format!("wait_task:{task_id}:{after:?}"));
            Ok(())
        }

        fn cancel_task(&self, task_id: &str) -> client::CliResult<TaskSummaryResponse> {
            self.calls
                .borrow_mut()
                .push(format!("cancel_task:{task_id}"));
            Ok(sample_task_response(TaskStatus::Cancelled))
        }

        fn ask<W>(&self, request: &AskRequest, stdout: &mut W) -> client::CliResult<()>
        where
            W: Write,
        {
            self.last_ask.replace(Some(request.clone()));
            write!(stdout, "Answer [E1].")?;
            render::write_citations(stdout, &[sample_citation()])?;
            if request.show_retrieval {
                render::write_retrieval_debug(stdout, &sample_debug_json())?;
            }
            Ok(())
        }

        fn retrieve(&self, request: &RetrieveRequest) -> client::CliResult<RetrieveResponse> {
            self.calls.borrow_mut().push("retrieve".into());
            self.last_retrieve.replace(Some(request.clone()));
            Ok(sample_retrieve_response(request))
        }

        fn get_evidence(&self, evidence_id: &str) -> client::CliResult<EvidenceResponse> {
            self.calls
                .borrow_mut()
                .push(format!("get_evidence:{evidence_id}"));
            Ok(sample_evidence())
        }

        fn get_config(&self) -> client::CliResult<ConfigResponse> {
            self.calls.borrow_mut().push("get_config".into());
            Ok(serde_json::json!({"daemon": {"bind": "127.0.0.1:7700"}}))
        }

        fn health(&self) -> client::CliResult<HealthResponse> {
            if let Some(error) = &self.health_error {
                return Err(clone_cli_error(error));
            }
            self.calls.borrow_mut().push("health".into());
            Ok(HealthResponse {
                status: "ok".into(),
            })
        }
    }

    #[derive(Default)]
    struct MockLocalActions {
        calls: RefCell<Vec<String>>,
    }

    impl LocalActions for MockLocalActions {
        fn config_init(&self) -> client::CliResult<PathBuf> {
            self.calls.borrow_mut().push("config_init".into());
            Ok(PathBuf::from("/tmp/config.toml"))
        }

        fn config_validate(&self) -> client::CliResult<PathBuf> {
            self.calls.borrow_mut().push("config_validate".into());
            Ok(PathBuf::from("/tmp/config.toml"))
        }

        fn daemon_start(&self) -> client::CliResult<u8> {
            self.calls.borrow_mut().push("daemon_start".into());
            Ok(0)
        }

        fn daemon_install(&self, force: bool) -> client::CliResult<PathBuf> {
            self.calls
                .borrow_mut()
                .push(format!("daemon_install:{force}"));
            Ok(PathBuf::from("/tmp/verbatim.service"))
        }
    }

    fn clone_cli_error(error: &CliError) -> CliError {
        match error {
            CliError::Api(message) => CliError::Api(message.clone()),
            CliError::DaemonUnreachable(message) => CliError::DaemonUnreachable(message.clone()),
            CliError::Io(_) => CliError::Api(error.to_string()),
        }
    }

    fn sample_source() -> SourceResponse {
        SourceResponse {
            id: "src-1".into(),
            path: "/tmp/doc.pdf".into(),
            status: "Pending".into(),
            hash: "hash".into(),
            parser_used: None,
            last_ingested_at: None,
            diagnostics: None,
        }
    }

    fn sample_evidence() -> EvidenceResponse {
        EvidenceResponse {
            id: "ev-1".into(),
            source_id: "src-1".into(),
            kind: "text".into(),
            derived_from: None,
            locator: "PDF p.1 para.1".into(),
            structured_locator: SourceLocator::Pdf {
                page: 1,
                paragraph: 1,
                bbox: None,
            },
            text: "quoted".into(),
            heading_path: Vec::new(),
            position: 0,
            image_artifact: None,
        }
    }

    fn sample_citation() -> CitationResponse {
        CitationResponse {
            label: "E1".into(),
            evidence_id: "ev-1".into(),
            kind: "original_text".into(),
            derived_from: None,
            locator: "PDF p.1 para.1".into(),
            text_preview: "preview".into(),
        }
    }

    fn sample_retrieve_response(request: &RetrieveRequest) -> RetrieveResponse {
        RetrieveResponse {
            task_id: "task-1".into(),
            query: request.question.clone(),
            source_id: request.source_id.clone(),
            embedding_profile_id: request
                .embedding_profile_id
                .clone()
                .unwrap_or_else(|| "default".into()),
            limit: request.limit.unwrap_or(12),
            page_size: request.page_size.unwrap_or(1),
            page: request.page.unwrap_or(1),
            total_results: 1,
            returned_results: 1,
            controls: RetrieveControlsResponse {
                fast: request.fast,
                rerank_enabled: request.rerank.unwrap_or(false),
                dense_top_k: request.dense_top_k.unwrap_or(20),
                bm25_top_k: request.bm25_top_k.unwrap_or(20),
                rrf_k: 60,
                rerank_top_n: request.rerank_top_n.unwrap_or(0),
            },
            timings: vec![RetrieveTimingResponse {
                phase: "retrieval".into(),
                duration_ms: 7,
            }],
            results: vec![RetrieveResultResponse {
                index: 0,
                rank: 1,
                label: "E1".into(),
                evidence_id: "ev-1".into(),
                source_id: "src-1".into(),
                source_path: Some("/tmp/doc.md".into()),
                chunk_id: "chunk-1".into(),
                kind: "text".into(),
                role: "original_text".into(),
                score: 0.031,
                locator: "/tmp/doc.md L1".into(),
                structured_locator: request.include_locator.then(|| SourceLocator::Document {
                    path_or_url: "/tmp/doc.md".into(),
                    line_start: 1,
                    line_end: None,
                }),
                provenance: None,
                derived_from: None,
                snippet: "compact cited text".into(),
            }],
            debug: request.include_debug.then(sample_typed_debug),
        }
    }

    fn sample_typed_debug() -> verbatim_core::types::RetrievalDebug {
        serde_json::from_value(sample_debug_json()).unwrap()
    }

    fn sample_task_response(status: TaskStatus) -> TaskSummaryResponse {
        TaskSummaryResponse {
            task: TaskSummary {
                id: TaskId("task-1".into()),
                kind: TaskKind::Ask,
                status,
                created_at: "1".into(),
                updated_at: "2".into(),
                started_at: Some("1".into()),
                finished_at: Some("2".into()),
                request: serde_json::json!({"question_chars": 14}),
                result: Some(serde_json::json!({"citation_count": 1})),
                error: None,
                queue_position: None,
                blocking_reason: None,
            },
            spans: vec![TaskSpan {
                sequence: 1,
                task_id: TaskId("task-1".into()),
                phase: "retrieval".into(),
                started_at: "1".into(),
                duration_ms: 7,
                metadata: serde_json::json!({"result_count": 1}),
            }],
        }
    }

    fn sample_task_event() -> TaskEvent {
        TaskEvent {
            sequence: 4,
            task_id: TaskId("task-1".into()),
            event_type: "phase".into(),
            message: "retrieval complete".into(),
            payload: serde_json::json!({"result_count": 1}),
            created_at: "2".into(),
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
            "graph_expanded_hits": [],
            "reranker": {
                "status": "skipped",
                "reason": "disabled",
                "scores": []
            },
            "final_evidence_pack": [
                {
                    "label": "E1",
                    "result_rank": 1,
                    "chunk_id": "chunk-1",
                    "score": 0.03,
                    "evidence_id": "ev-1",
                    "source_id": "src-1",
                    "role": "original_text",
                    "kind": "Text",
                    "locator": {
                        "display": "PDF p.1 para.1",
                        "structured": {
                            "type": "Document",
                            "path_or_url": "/tmp/doc.md",
                            "line_start": 1,
                            "line_end": null
                        }
                    },
                    "provenance": {
                        "origin": "seed",
                        "result_rank": 1,
                        "seed_rank": 1,
                        "seed_chunk_id": "chunk-1",
                        "seed_source_id": "src-1",
                        "hop_distance": 0,
                        "graph_path": []
                    }
                }
            ]
        })
    }
}
