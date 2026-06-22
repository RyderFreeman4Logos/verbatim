use std::env;
use std::io::Write;
use std::process::ExitCode;

use clap::{error::ErrorKind, CommandFactory, Parser, Subcommand};
use verbatim_core::api::AskRequest;

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

fn dispatch<W, C, L>(cli: Cli, stdout: &mut W, client: &C, local: &L) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
    L: LocalActions,
{
    match cli.command {
        Commands::Source { command } => run_source(command, stdout, client),
        Commands::Ingest { source_id, force } => {
            let response = client.ingest(source_id.as_deref(), force)?;
            render::write_ingest(stdout, &response)?;
            Ok(0)
        }
        Commands::Ask {
            question,
            source_id,
            show_retrieval,
        } => {
            let request = AskRequest {
                question: question.join(" "),
                source_id,
                show_retrieval,
            };
            client.ask(&request, stdout)?;
            writeln!(stdout)?;
            Ok(0)
        }
        Commands::Evidence { eid } => {
            let evidence = client.get_evidence(&eid)?;
            render::write_evidence(stdout, &evidence)?;
            Ok(0)
        }
        Commands::Config { command } => run_config(command, stdout, client, local),
        Commands::Daemon { command } => run_daemon(command, stdout, client, local),
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
    },
    /// Ask a question and stream the answer from the daemon.
    Ask {
        /// Restrict retrieval to one source.
        #[arg(short = 's', long = "source-id")]
        source_id: Option<String>,
        /// Show retrieval provenance and ranking debug output.
        #[arg(long)]
        show_retrieval: bool,
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use serde_json::Value;
    use verbatim_core::api::{
        AddSourceResponse, CheckStaleResponse, CitationResponse, ConfigResponse, EvidenceResponse,
        HealthResponse, IngestResponse, SourceResponse,
    };

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
            &["ask", "--help"],
            &["evidence", "--help"],
            &["config", "--help"],
            &["config", "init", "--help"],
            &["config", "show", "--help"],
            &["config", "validate", "--help"],
            &["daemon", "--help"],
            &["daemon", "start", "--help"],
            &["daemon", "status", "--help"],
            &["daemon", "install", "--help"],
        ];

        for args in cases {
            let (code, stdout, stderr, _, _) = run_mock(args.iter().copied());
            assert_eq!(code.unwrap(), 0, "args: {args:?}");
            assert!(stdout.contains("Usage:"), "stdout: {stdout}");
            assert!(stderr.is_empty(), "stderr: {stderr}");
        }
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
        assert_eq!(client.calls.borrow().as_slice(), ["ingest:None:true"]);

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
                show_retrieval: true,
            }
        );
        assert!(stdout.contains("Answer [E1]."));
        assert!(stdout.contains("Retrieval Debug"));
        assert!(stdout.contains("Final evidence pack:"));
        assert!(!stdout.contains("secret full raw source text"));
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
        ) -> client::CliResult<IngestResponse> {
            self.calls
                .borrow_mut()
                .push(format!("ingest:{source_id:?}:{force}"));
            Ok(IngestResponse { ingested: 1 })
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
        }
    }

    fn sample_evidence() -> EvidenceResponse {
        EvidenceResponse {
            id: "ev-1".into(),
            source_id: "src-1".into(),
            kind: "text".into(),
            derived_from: None,
            locator: "PDF p.1 para.1".into(),
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
                    "chunk_id": "chunk-1",
                    "evidence_id": "ev-1",
                    "role": "original_text",
                    "locator": {
                        "display": "PDF p.1 para.1"
                    }
                }
            ]
        })
    }
}
