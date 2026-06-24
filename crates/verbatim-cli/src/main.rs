use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{error::ErrorKind, ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
#[cfg(test)]
use verbatim_core::api::IndexGcResponse;
use verbatim_core::api::{
    AddCollectionRootRequest, AskRequest, CollectionFilterRequest, CollectionSyncPathRequest,
    CollectionSyncRequest, CollectionWatcherUpdateRequest, CreateCollectionRequest, IndexGcRequest,
    ReindexRequest, RetrieveRequest,
};

mod client;
mod local;
mod render;
mod sse;

use client::{CliError, DaemonClient, HttpDaemonClient, TaskWaitTimeout};
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

fn collection_filter_request(
    collection_names: Vec<String>,
    require_fresh: bool,
) -> CollectionFilterRequest {
    CollectionFilterRequest {
        collection_ids: Vec::new(),
        names: collection_names,
        require_fresh,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HumanDuration(Duration);

fn parse_task_wait_timeout(value: &str) -> Result<HumanDuration, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("must not be empty".into());
    }

    let number_end = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    if number_end == 0 {
        return Err("must start with a positive integer".into());
    }

    let amount = value[..number_end]
        .parse::<u64>()
        .map_err(|error| format!("must start with a positive integer: {error}"))?;
    if amount == 0 {
        return Err("must be greater than zero".into());
    }

    let unit = &value[number_end..];
    let multiplier = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err("use seconds or a duration suffix: s, m, h, d".into()),
    };
    let seconds = amount
        .checked_mul(multiplier)
        .ok_or_else(|| "duration is too large".to_string())?;

    Ok(HumanDuration(Duration::from_secs(seconds)))
}

fn task_wait_timeout_selection(
    timeout: Option<HumanDuration>,
    no_timeout: bool,
) -> TaskWaitTimeout {
    if no_timeout {
        TaskWaitTimeout::Unbounded
    } else if let Some(HumanDuration(duration)) = timeout {
        TaskWaitTimeout::Bounded(duration)
    } else {
        TaskWaitTimeout::ConfigDefault
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
        Commands::Collection { command } => run_collection(command, stdout, client),
        Commands::Index { command } => run_index(command, stdout, client),
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
            collection,
            require_fresh,
            embedding_profile,
            show_retrieval,
            context_only,
            no_generate,
            format,
            background,
        } => {
            let context_only = context_only || no_generate;
            let question = question.join(" ");
            let collection_filter = collection_filter_request(collection, require_fresh);
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
                    collection_filter,
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
                collection_filter,
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
            collection,
            require_fresh,
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
                collection_filter: collection_filter_request(collection, require_fresh),
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

fn run_collection<W, C>(
    command: CollectionCommand,
    stdout: &mut W,
    client: &C,
) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
{
    match command {
        CollectionCommand::Create {
            name,
            ignore_patterns,
        } => {
            let response = client.create_collection(&CreateCollectionRequest {
                name,
                ignore_patterns,
            })?;
            render::write_collection(stdout, &response)?;
        }
        CollectionCommand::AddRoot { name, path } => {
            let response = client.add_collection_root(
                &name,
                &AddCollectionRootRequest {
                    path: absolute_cli_path(&path)?.display().to_string(),
                },
            )?;
            render::write_collection(stdout, &response)?;
        }
        CollectionCommand::List => {
            let collections = client.list_collections()?;
            render::write_collections(stdout, &collections)?;
        }
        CollectionCommand::Get { name } => {
            let response = client.get_collection(&name)?;
            render::write_collection(stdout, &response)?;
        }
        CollectionCommand::Delete { name } => {
            client.delete_collection(&name)?;
            writeln!(stdout, "Deleted collection: {name}")?;
        }
        CollectionCommand::Sync {
            name,
            stdin,
            max_depth,
            paths,
        } => {
            let request = collection_sync_request(paths, stdin, max_depth)?;
            let response = client.sync_collection(&name, &request)?;
            render::write_collection_sync_report(stdout, &response.report)?;
        }
        CollectionCommand::Status { name } => {
            let response = client.collection_status(&name)?;
            render::write_collection_status(stdout, &response.status)?;
        }
        CollectionCommand::Watch { command } => {
            run_collection_watch(command, stdout, client)?;
        }
    }
    Ok(0)
}

fn run_collection_watch<W, C>(
    command: CollectionWatchCommand,
    stdout: &mut W,
    client: &C,
) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
{
    match command {
        CollectionWatchCommand::Enable {
            name,
            auto_index,
            no_auto_index,
        } => {
            let response = client.update_collection_watcher(
                &name,
                &CollectionWatcherUpdateRequest {
                    enabled: true,
                    auto_index_enabled: auto_index_setting(auto_index, no_auto_index),
                },
            )?;
            render::write_collection_watcher_status(stdout, &response.watcher)?;
        }
        CollectionWatchCommand::Disable { name } => {
            let response = client.update_collection_watcher(
                &name,
                &CollectionWatcherUpdateRequest {
                    enabled: false,
                    auto_index_enabled: None,
                },
            )?;
            render::write_collection_watcher_status(stdout, &response.watcher)?;
        }
        CollectionWatchCommand::Status { name } => {
            if let Some(name) = name {
                let response = client.collection_watcher_status(&name)?;
                render::write_collection_watcher_status(stdout, &response.watcher)?;
            } else {
                let response = client.list_collection_watcher_statuses()?;
                render::write_collection_watcher_statuses(stdout, &response.watchers)?;
            }
        }
    }
    Ok(0)
}

fn run_index<W, C>(command: IndexCommand, stdout: &mut W, client: &C) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
{
    match command {
        IndexCommand::Gc { dry_run } => {
            let response = client.index_gc(&IndexGcRequest { dry_run })?;
            render::write_index_gc(stdout, &response)?;
        }
    }
    Ok(0)
}

fn auto_index_setting(auto_index: bool, no_auto_index: bool) -> Option<bool> {
    match (auto_index, no_auto_index) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn collection_sync_request(
    paths: Vec<String>,
    read_stdin: bool,
    max_depth: Option<usize>,
) -> Result<CollectionSyncRequest, CliError> {
    let mut request_paths = paths
        .iter()
        .map(|path| {
            Ok(CollectionSyncPathRequest {
                path: absolute_cli_path(path)?.display().to_string(),
                logical_path: None,
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;

    if read_stdin {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        request_paths.extend(collection_sync_stdin_paths(&input)?);
    }

    Ok(CollectionSyncRequest {
        paths: request_paths,
        max_depth,
    })
}

fn collection_sync_stdin_paths(input: &str) -> Result<Vec<CollectionSyncPathRequest>, CliError> {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            Ok(CollectionSyncPathRequest {
                path: absolute_cli_path(line)?.display().to_string(),
                logical_path: Some(line.replace('\\', "/")),
            })
        })
        .collect()
}

fn absolute_cli_path(path: &str) -> Result<PathBuf, CliError> {
    let path = Path::new(path);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
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
        TaskCommand::Wait {
            task_id,
            after,
            timeout,
            no_timeout,
        } => {
            let timeout = task_wait_timeout_selection(timeout, no_timeout);
            client.wait_task(&task_id, after, timeout, stdout)?;
        }
        TaskCommand::Watch { task_id, after } => {
            client.wait_task(&task_id, after, TaskWaitTimeout::Unbounded, stdout)?;
        }
        TaskCommand::Cancel { task_id } => {
            let response = client.cancel_task(&task_id)?;
            render::write_task_summary(stdout, &response.task, &response.spans)?;
        }
        TaskCommand::Resume { task_id } => {
            let response = client.resume_task(&task_id)?;
            render::write_task_summary(stdout, &response.task, &response.spans)?;
        }
    }
    Ok(0)
}

const TOP_LEVEL_LONG_ABOUT: &str = "\
Verbatim indexes local documents through a long-running daemon and exposes a \
thin CLI for source management, ingestion, retrieval, citation-backed answers, \
and persistent background tasks.";

const TOP_LEVEL_AFTER_HELP: &str = r#"Examples:
  verbatim config init
  verbatim daemon install
  verbatim source add ./paper.pdf
  verbatim collection create articles
  verbatim ingest <source-id>
  verbatim index gc --dry-run
  verbatim retrieve --show-debug "What does the paper claim?"
  verbatim ask --source-id <source-id> "What supports the conclusion?"

Help:
  verbatim <command> --help
  verbatim source add --help
  verbatim task wait --help
"#;

const SOURCE_AFTER_HELP: &str = r#"Examples:
  verbatim source add ./paper.pdf
  verbatim source list
  verbatim source inspect <source-id>
  verbatim source check

Sources are daemon-registered paths. Add a source before ingesting it.
"#;

const SOURCE_ADD_AFTER_HELP: &str = r#"Examples:
  verbatim source add ./paper.pdf
  verbatim source add ./docs

The daemon stores the path and returns a stable source id for later ingest,
retrieve, ask, inspect, or remove commands.
"#;

const SOURCE_LIST_AFTER_HELP: &str = r#"Examples:
  verbatim source list

Use the printed source ids with ingest, retrieve, ask, and source inspect.
"#;

const SOURCE_INSPECT_AFTER_HELP: &str = r#"Examples:
  verbatim source inspect <source-id>

Inspect shows daemon metadata such as path, status, hash, parser, and diagnostics.
"#;

const SOURCE_REMOVE_AFTER_HELP: &str = r#"Examples:
  verbatim source remove <source-id>

Remove unregisters the source from the daemon catalog. It is not a shell rm for
the original file path.
"#;

const SOURCE_CHECK_AFTER_HELP: &str = r#"Examples:
  verbatim source check

Check hashes registered sources and reports stale ids that should be ingested or
reindexed.
"#;

const COLLECTION_AFTER_HELP: &str = r#"Examples:
  verbatim collection create articles
  verbatim collection add-root articles ../drafts/articles/articles
  verbatim collection sync articles
  fd -e md . ../drafts/articles/articles | verbatim collection sync articles --stdin

Collections materialize filesystem membership into the daemon catalog. Retrieval
does not rescan collection directories per request.
"#;

const COLLECTION_CREATE_AFTER_HELP: &str = r#"Examples:
  verbatim collection create articles
  verbatim collection create areskapitalon --ignore drafts/

Collection names are stable daemon identifiers. Ignore patterns are matched
against collection logical paths during sync.
"#;

const COLLECTION_ADD_ROOT_AFTER_HELP: &str = r#"Examples:
  verbatim collection add-root articles ../drafts/articles/articles
  verbatim collection add-root articles ./linked-articles

Roots may be files, directories, or symlinks. Sync follows symlinks with
canonical path loop checks and bounded traversal depth.
"#;

const COLLECTION_LIST_AFTER_HELP: &str = r#"Examples:
  verbatim collection list

List shows collection records and last sync timestamps.
"#;

const COLLECTION_GET_AFTER_HELP: &str = r#"Examples:
  verbatim collection get articles

Get shows persistent roots and materialized members for one collection.
"#;

const COLLECTION_DELETE_AFTER_HELP: &str = r#"Examples:
  verbatim collection delete articles
  verbatim collection remove articles

Delete removes the collection record and membership only; source records and
filesystem files are left intact.
"#;

const COLLECTION_SYNC_AFTER_HELP: &str = r#"Examples:
  verbatim collection sync articles
  fd -e md . ../drafts/articles/articles | verbatim collection sync articles --stdin
  fd 'Areskapitalon.*\.md' ../drafts/articles/articles | verbatim collection sync areskapitalon --stdin

Sync materializes membership from stored roots plus optional one-shot paths.
stdin lines preserve their text as collection logical paths.
"#;

const COLLECTION_STATUS_AFTER_HELP: &str = r#"Examples:
  verbatim collection status articles

Status reads the persisted sync summary and member count without scanning the
filesystem.
"#;

const COLLECTION_WATCH_AFTER_HELP: &str = r#"Examples:
  verbatim collection watch enable articles --auto-index
  verbatim collection watch enable articles --no-auto-index
  verbatim collection watch status
  verbatim collection watch status articles
  verbatim collection watch disable articles

Watcher commands call daemon API operations. The CLI does not watch the
filesystem itself.
"#;

const COLLECTION_WATCH_ENABLE_AFTER_HELP: &str = r#"Examples:
  verbatim collection watch enable articles
  verbatim collection watch enable articles --auto-index
  verbatim collection watch enable articles --no-auto-index

Enable persists the collection watcher setting in the daemon collection
registry. Auto-index is left unchanged unless a flag is provided.
"#;

const COLLECTION_WATCH_DISABLE_AFTER_HELP: &str = r#"Examples:
  verbatim collection watch disable articles

Disable stops future auto-maintenance for this collection after the daemon
refreshes its watch set.
"#;

const COLLECTION_WATCH_STATUS_AFTER_HELP: &str = r#"Examples:
  verbatim collection watch status
  verbatim collection watch status articles

Status reads daemon watcher state, including active roots, pending debounced
events, last sync diff, last task id, and last error.
"#;

const INDEX_AFTER_HELP: &str = r#"Examples:
  verbatim index gc --dry-run
  verbatim index gc

Index maintenance operates on daemon-managed index artifacts only.
"#;

const INDEX_GC_AFTER_HELP: &str = r#"Examples:
  verbatim index gc --dry-run
  verbatim index gc

GC removes old per-profile gen-* index generations and stale staging-*
directories according to [index_gc] policy. It does not delete sources, SQLite
data, embedding cache, or image artifacts.
"#;

const INGEST_AFTER_HELP: &str = r#"Examples:
  verbatim ingest <source-id>
  verbatim ingest --background <source-id>
  verbatim ingest --force
  verbatim ingest <source-id> --embedding-profile alt --vectors-only

Caveats:
  --force is only for all-source ingest. It is rejected when SOURCE_ID is set.
  --force cannot be combined with --vectors-only.
  --embedding-profile rebuilds vectors from existing chunks and requires
  --vectors-only. For normal parsing ingest, set [embedding].profile_id in the
  config instead.
"#;

const REINDEX_AFTER_HELP: &str = r#"Examples:
  verbatim reindex --source-id <source-id>
  verbatim reindex --all
  verbatim reindex --stale
  verbatim reindex --all --vectors-only
  verbatim reindex --source-id <source-id> --embedding-profile alt --vectors-only

Caveats:
  Choose at most one target: --source-id, --all, or --stale.
  --force is all-source reindex shorthand and is rejected with --source-id,
  --stale, or vector-only profile rebuilds.
  --embedding-profile selects a vector profile rebuild from existing chunks; use
  --vectors-only as the explicit profile rebuild mode in scripts.
"#;

const ASK_AFTER_HELP: &str = r#"Examples:
  verbatim ask "What does the report conclude?"
  verbatim ask --source-id <source-id> --show-retrieval "What supports it?"
  verbatim ask --collection articles "What evidence is relevant?"
  verbatim ask --context-only "What evidence is relevant?"
  verbatim ask --no-generate --format json "What evidence is relevant?"

Caveats:
  Normal ask invokes the configured chat model after retrieval.
  --context-only and --no-generate return retrieval context without chat
  generation; --background is not supported in that mode.
  --format only applies with --context-only or --no-generate.
"#;

const RETRIEVE_AFTER_HELP: &str = r#"Examples:
  verbatim retrieve "What does the report conclude?"
  verbatim retrieve --source-id <source-id> --page-size 1 "What supports it?"
  verbatim retrieve --collection articles "What evidence is relevant?"
  verbatim retrieve --collection articles --collection areskapitalon "What changed?"
  verbatim retrieve --show-debug --show-locator "What evidence is relevant?"
  verbatim retrieve --format json --show-debug "What evidence is relevant?"

Debugging:
  retrieve never invokes chat generation.
  It returns evidence context without invoking chat generation.
  --collection filters against materialized daemon membership and does not
  rescan collection roots during retrieve.
  --show-debug includes deterministic dense/BM25/RRF/rerank evidence ranking
  details for debugging and agent workflows.
  --show-locator and JSON output include structured locator/provenance fields
  when available.
"#;

const EVIDENCE_AFTER_HELP: &str = r#"Examples:
  verbatim evidence <evidence-id>

Evidence ids come from retrieve output, ask citations, and retrieval debug packs.
"#;

const CONFIG_AFTER_HELP: &str = r#"Examples:
  verbatim config init
  verbatim config validate
  verbatim config show

The default config path is ~/.config/verbatim/config.toml. config show fetches
redacted runtime config from the daemon.
"#;

const CONFIG_INIT_AFTER_HELP: &str = r#"Examples:
  verbatim config init
  $EDITOR ~/.config/verbatim/config.toml
  verbatim config validate

Initialize creates the local config file if it does not already exist.
"#;

const CONFIG_SHOW_AFTER_HELP: &str = r#"Examples:
  verbatim daemon status
  verbatim config show

Show reads the active daemon config view and redacts secret-like values.
"#;

const CONFIG_VALIDATE_AFTER_HELP: &str = r#"Examples:
  verbatim config validate

Validate checks the local config file before the daemon loads it.
"#;

const DAEMON_AFTER_HELP: &str = r#"Examples:
  verbatim daemon start
  verbatim daemon install
  systemctl --user daemon-reload
  systemctl --user enable --now verbatim
  verbatim daemon status

The CLI talks to the daemon HTTP API. Start the daemon before source, ingest,
retrieve, ask, evidence, config show, or task commands.
"#;

const DAEMON_START_AFTER_HELP: &str = r#"Examples:
  verbatim daemon start

Start runs verbatim-daemon in the foreground. For a persistent user service, use
verbatim daemon install and systemctl --user enable --now verbatim.
"#;

const DAEMON_STATUS_AFTER_HELP: &str = r#"Examples:
  verbatim daemon status

Status checks daemon health through the HTTP API and fails if the daemon cannot
be reached.
"#;

const DAEMON_INSTALL_AFTER_HELP: &str = r#"Examples:
  verbatim daemon install
  verbatim daemon install --force
  systemctl --user daemon-reload
  systemctl --user enable --now verbatim

Install writes ~/.config/systemd/user/verbatim.service, or the equivalent path
under XDG_CONFIG_HOME. Use --force only to replace an existing unit file.
"#;

const TASK_AFTER_HELP: &str = r#"Examples:
  verbatim task show <task-id>
  verbatim task events <task-id>
  verbatim task wait --timeout 25m <task-id>
  verbatim task cancel <task-id>
  verbatim task resume <task-id>

Task ids are returned by --background ingest/reindex/ask commands.
"#;

const TASK_SHOW_AFTER_HELP: &str = r#"Examples:
  verbatim task show <task-id>

Show prints the current task status, request/result summary, progress snapshot,
and phase spans.
"#;

const TASK_EVENTS_AFTER_HELP: &str = r#"Examples:
  verbatim task events <task-id>
  verbatim task events --after 42 <task-id>

Events are ordered by sequence. Use --after to resume from the last sequence you
already consumed.
"#;

const TASK_WAIT_AFTER_HELP: &str = r#"Examples:
  verbatim task wait --timeout 25m <task-id>
  verbatim task wait --no-timeout <task-id>
  verbatim task wait --after 42 <task-id>

Timeouts:
  --timeout caps only this CLI wait call.
  Without --timeout or --no-timeout, Verbatim uses cli.task_wait_timeout_seconds
  from config.
  This is separate from model timeout_seconds settings for embedding, rerank,
  chat, vision, and OCR requests.
"#;

const TASK_WATCH_AFTER_HELP: &str = r#"Examples:
  verbatim task watch <task-id>
  verbatim task watch --after 42 <task-id>

Watch is the unbounded wait form. Prefer task wait --timeout for scripts that
need a bounded CLI call.
"#;

const TASK_CANCEL_AFTER_HELP: &str = r#"Examples:
  verbatim task cancel <task-id>

Cancel is cooperative. Ingest batch parents also cancel queued batch children.
Use task show or task events to inspect the resulting terminal status.
"#;

const TASK_RESUME_AFTER_HELP: &str = r#"Examples:
  verbatim task resume <task-id>

Resume requeues a failed ingest/reindex task by the same task id when its
stored request metadata is executable. Ask/retrieve tasks are not resumable.
"#;

#[derive(Debug, Parser)]
#[command(
    name = "verbatim",
    version,
    about = "Grounded document retrieval and Q&A with traceable citations",
    long_about = TOP_LEVEL_LONG_ABOUT,
    after_help = TOP_LEVEL_AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Manage sources through the daemon API.
    #[command(
        about = "Manage daemon-registered document sources.",
        after_help = SOURCE_AFTER_HELP
    )]
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// Manage filesystem-backed source collections through the daemon API.
    #[command(
        about = "Manage materialized filesystem collections.",
        after_help = COLLECTION_AFTER_HELP
    )]
    Collection {
        #[command(subcommand)]
        command: CollectionCommand,
    },
    /// Maintain daemon-managed retrieval index artifacts.
    #[command(
        about = "Inspect or clean daemon-managed retrieval index artifacts.",
        after_help = INDEX_AFTER_HELP
    )]
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Trigger ingestion through the daemon API.
    #[command(
        about = "Parse sources and build retrieval indexes through the daemon.",
        after_help = INGEST_AFTER_HELP
    )]
    Ingest {
        /// Ingest this source only. Omit to ingest all pending/stale sources.
        #[arg(value_name = "SOURCE_ID")]
        source_id: Option<String>,
        /// Re-ingest all sources, including already indexed sources.
        ///
        /// Only valid when SOURCE_ID is omitted; rejected with --vectors-only.
        #[arg(long)]
        force: bool,
        /// Build vectors for this profile from existing chunks.
        ///
        /// Requires --vectors-only. For normal parsing ingest, set
        /// [embedding].profile_id in the config.
        #[arg(long = "embedding-profile")]
        embedding_profile: Option<String>,
        /// Build only profile vectors/indexes without re-parsing sources.
        ///
        /// Use this with --embedding-profile for profile rebuilds.
        #[arg(long)]
        vectors_only: bool,
        /// Queue ingest as a persistent daemon task and return immediately.
        #[arg(long)]
        background: bool,
    },
    /// Rebuild derived indexes for existing sources without adding sources.
    #[command(
        about = "Rebuild derived indexes for already registered sources.",
        after_help = REINDEX_AFTER_HELP
    )]
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
        /// Force all-source reindex. Rejected with --source-id, --stale, or vector-only rebuilds.
        #[arg(long)]
        force: bool,
        /// Build vectors for this profile from existing chunks.
        ///
        /// Selects a vector profile rebuild; use --vectors-only for explicit
        /// profile rebuild scripts.
        #[arg(long = "embedding-profile")]
        embedding_profile: Option<String>,
        /// Build only profile vectors/indexes without re-parsing sources.
        #[arg(long)]
        vectors_only: bool,
        /// Queue reindex as a persistent daemon task and return immediately.
        #[arg(long)]
        background: bool,
    },
    /// Generate a cited answer, or return a context pack with --context-only.
    #[command(
        about = "Generate a cited answer, or return retrieval context without generation.",
        after_help = ASK_AFTER_HELP
    )]
    Ask {
        /// Restrict retrieval to one source.
        #[arg(short = 's', long = "source-id")]
        source_id: Option<String>,
        /// Restrict retrieval to this materialized collection. Repeat for union semantics.
        #[arg(long = "collection", value_name = "NAME")]
        collection: Vec<String>,
        /// Fail instead of returning warning metadata for stale collection membership.
        #[arg(long = "require-fresh")]
        require_fresh: bool,
        /// Use this embedding profile for retrieval.
        #[arg(long = "embedding-profile")]
        embedding_profile: Option<String>,
        /// Show retrieval provenance and ranking debug output.
        #[arg(long)]
        show_retrieval: bool,
        /// Return a retrieval context pack instead of invoking chat generation.
        ///
        /// This mode uses retrieve semantics and cannot be combined with
        /// --background.
        #[arg(long = "context-only", action = ArgAction::SetTrue)]
        context_only: bool,
        /// Alias for --context-only; no chat model is invoked.
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
    #[command(
        about = "Retrieve a ranked evidence/context pack without chat generation.",
        after_help = RETRIEVE_AFTER_HELP
    )]
    Retrieve {
        /// Restrict retrieval to one source.
        #[arg(short = 's', long = "source-id")]
        source_id: Option<String>,
        /// Restrict retrieval to this materialized collection. Repeat for union semantics.
        #[arg(long = "collection", value_name = "NAME")]
        collection: Vec<String>,
        /// Fail instead of returning warning metadata for stale collection membership.
        #[arg(long = "require-fresh")]
        require_fresh: bool,
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
        /// Include deterministic retrieval stage debug metadata in the response.
        ///
        /// Useful for evidence/provenance debugging and agent workflows.
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
    #[command(
        about = "Inspect one evidence unit by stable evidence id.",
        after_help = EVIDENCE_AFTER_HELP
    )]
    Evidence {
        /// Evidence id.
        #[arg(value_name = "EVIDENCE_ID")]
        eid: String,
    },
    /// Inspect or update configuration.
    #[command(
        about = "Initialize, validate, or inspect Verbatim configuration.",
        after_help = CONFIG_AFTER_HELP
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage daemon process/API helpers.
    #[command(
        about = "Start, check, or install the Verbatim daemon.",
        after_help = DAEMON_AFTER_HELP
    )]
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Inspect and wait for persistent daemon tasks.
    #[command(
        about = "Inspect, stream, wait for, or cancel persistent daemon tasks.",
        after_help = TASK_AFTER_HELP
    )]
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SourceCommand {
    /// Add a source path.
    #[command(
        about = "Register a file or directory path as a source.",
        after_help = SOURCE_ADD_AFTER_HELP
    )]
    Add {
        /// File or directory path to register with the daemon.
        #[arg(value_name = "PATH")]
        path: String,
    },
    /// List sources.
    #[command(
        about = "List daemon-registered sources.",
        after_help = SOURCE_LIST_AFTER_HELP
    )]
    List,
    /// Inspect one source.
    #[command(
        about = "Inspect metadata and diagnostics for one source.",
        after_help = SOURCE_INSPECT_AFTER_HELP
    )]
    Inspect {
        /// Source id.
        #[arg(value_name = "SOURCE_ID")]
        id: String,
    },
    /// Remove one source.
    #[command(
        about = "Remove one source from the daemon catalog.",
        after_help = SOURCE_REMOVE_AFTER_HELP
    )]
    Remove {
        /// Source id.
        #[arg(value_name = "SOURCE_ID")]
        id: String,
    },
    /// Mark and list stale sources.
    #[command(
        about = "Check registered sources for stale hashes.",
        after_help = SOURCE_CHECK_AFTER_HELP
    )]
    Check,
}

#[derive(Debug, Subcommand)]
enum CollectionCommand {
    /// Create a collection record.
    #[command(
        about = "Create a collection record.",
        after_help = COLLECTION_CREATE_AFTER_HELP
    )]
    Create {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
        /// Collection-level ignore pattern. Repeatable.
        #[arg(long = "ignore", value_name = "PATTERN")]
        ignore_patterns: Vec<String>,
    },
    /// Add a persistent filesystem root to a collection.
    #[command(
        about = "Add a persistent filesystem root to a collection.",
        after_help = COLLECTION_ADD_ROOT_AFTER_HELP
    )]
    AddRoot {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
        /// File, directory, or symlink path.
        #[arg(value_name = "PATH")]
        path: String,
    },
    /// List collections.
    #[command(
        about = "List collection records.",
        after_help = COLLECTION_LIST_AFTER_HELP
    )]
    List,
    /// Inspect one collection, including materialized roots and members.
    #[command(
        about = "Inspect one collection.",
        after_help = COLLECTION_GET_AFTER_HELP
    )]
    Get {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Delete one collection record and its membership.
    #[command(
        alias = "remove",
        about = "Delete one collection record and its membership.",
        after_help = COLLECTION_DELETE_AFTER_HELP
    )]
    Delete {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Sync materialized collection membership from roots and optional path inputs.
    #[command(
        about = "Sync materialized collection membership.",
        after_help = COLLECTION_SYNC_AFTER_HELP
    )]
    Sync {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
        /// Read newline-delimited file or directory paths from stdin.
        #[arg(long)]
        stdin: bool,
        /// Override safe traversal depth for this sync.
        #[arg(long = "max-depth", value_parser = parse_nonzero_usize)]
        max_depth: Option<usize>,
        /// Extra one-shot file, directory, or symlink paths for this sync.
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Show persisted collection sync status without rescanning the filesystem.
    #[command(
        about = "Show persisted collection sync status.",
        after_help = COLLECTION_STATUS_AFTER_HELP
    )]
    Status {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Manage daemon-side collection watcher maintenance.
    #[command(
        about = "Enable, disable, or inspect daemon-side collection watcher maintenance.",
        after_help = COLLECTION_WATCH_AFTER_HELP
    )]
    Watch {
        #[command(subcommand)]
        command: CollectionWatchCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CollectionWatchCommand {
    /// Enable watcher maintenance for one collection.
    #[command(
        about = "Enable watcher maintenance for one collection.",
        after_help = COLLECTION_WATCH_ENABLE_AFTER_HELP
    )]
    Enable {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
        /// Enable auto-indexing queued from watcher maintenance.
        #[arg(long = "auto-index", action = ArgAction::SetTrue, conflicts_with = "no_auto_index")]
        auto_index: bool,
        /// Disable auto-indexing while still refreshing materialized membership.
        #[arg(long = "no-auto-index", action = ArgAction::SetTrue)]
        no_auto_index: bool,
    },
    /// Disable watcher maintenance for one collection.
    #[command(
        about = "Disable watcher maintenance for one collection.",
        after_help = COLLECTION_WATCH_DISABLE_AFTER_HELP
    )]
    Disable {
        /// Collection name.
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Show daemon-side watcher status.
    #[command(
        about = "Show daemon-side watcher status.",
        after_help = COLLECTION_WATCH_STATUS_AFTER_HELP
    )]
    Status {
        /// Collection name. Omit to list every collection watcher status.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    /// Garbage collect old index generations and stale staging directories.
    #[command(
        about = "Garbage collect old index generations and stale staging directories.",
        after_help = INDEX_GC_AFTER_HELP
    )]
    Gc {
        /// Show what would be removed without deleting anything.
        #[arg(long = "dry-run", action = ArgAction::SetTrue)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Generate the default local config file.
    #[command(
        about = "Create the default local config file.",
        after_help = CONFIG_INIT_AFTER_HELP
    )]
    Init,
    /// Fetch redacted runtime config from the daemon.
    #[command(
        about = "Show the daemon's redacted runtime config.",
        after_help = CONFIG_SHOW_AFTER_HELP
    )]
    Show,
    /// Validate the local config file.
    #[command(
        about = "Validate the local config file before daemon use.",
        after_help = CONFIG_VALIDATE_AFTER_HELP
    )]
    Validate,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Start verbatim-daemon in the foreground.
    #[command(
        about = "Start verbatim-daemon in the foreground.",
        after_help = DAEMON_START_AFTER_HELP
    )]
    Start,
    /// Check daemon health through the daemon API.
    #[command(
        about = "Check daemon health through the HTTP API.",
        after_help = DAEMON_STATUS_AFTER_HELP
    )]
    Status,
    /// Install the systemd user service.
    #[command(
        about = "Install the systemd user service unit.",
        after_help = DAEMON_INSTALL_AFTER_HELP
    )]
    Install {
        /// Overwrite an existing service file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// Show task summary and phase spans.
    #[command(
        about = "Show task status, progress, result, and spans.",
        after_help = TASK_SHOW_AFTER_HELP
    )]
    Show {
        /// Task id.
        #[arg(value_name = "TASK_ID")]
        task_id: String,
    },
    /// List bounded task events.
    #[command(
        about = "List task events with optional sequence resume.",
        after_help = TASK_EVENTS_AFTER_HELP
    )]
    Events {
        /// Task id.
        #[arg(value_name = "TASK_ID")]
        task_id: String,
        /// Only show events after this sequence.
        #[arg(long)]
        after: Option<i64>,
    },
    /// Wait for task events until the task reaches a terminal status.
    #[command(
        about = "Wait until a task reaches a terminal status.",
        after_help = TASK_WAIT_AFTER_HELP
    )]
    Wait {
        /// Task id.
        #[arg(value_name = "TASK_ID")]
        task_id: String,
        /// Only stream events after this sequence.
        #[arg(long)]
        after: Option<i64>,
        /// Cap only this task wait; model timeout_seconds do not cap task streams.
        ///
        /// Plain numbers are seconds; suffixes: s, m, h, d.
        #[arg(long, value_name = "DURATION", value_parser = parse_task_wait_timeout, conflicts_with = "no_timeout")]
        timeout: Option<HumanDuration>,
        /// Wait without a CLI/config/default timeout.
        #[arg(long = "no-timeout", action = ArgAction::SetTrue)]
        no_timeout: bool,
    },
    /// Watch task progress until the task reaches a terminal status.
    #[command(
        about = "Watch task progress without a CLI wait timeout.",
        after_help = TASK_WATCH_AFTER_HELP
    )]
    Watch {
        /// Task id.
        #[arg(value_name = "TASK_ID")]
        task_id: String,
        /// Only stream events after this sequence.
        #[arg(long)]
        after: Option<i64>,
    },
    /// Request cooperative task cancellation.
    #[command(
        about = "Request cooperative cancellation for a task.",
        after_help = TASK_CANCEL_AFTER_HELP
    )]
    Cancel {
        /// Task id.
        #[arg(value_name = "TASK_ID")]
        task_id: String,
    },
    /// Resume a failed resumable task by id.
    #[command(
        about = "Resume a failed ingest/reindex task by the same task id.",
        after_help = TASK_RESUME_AFTER_HELP
    )]
    Resume {
        /// Task id.
        #[arg(value_name = "TASK_ID")]
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
        AddCollectionRootRequest, AddSourceResponse, CheckStaleResponse, CitationResponse,
        CollectionResponse, CollectionStatusResponse, CollectionSyncRequest,
        CollectionSyncResponse, CollectionWatcherResponse, CollectionWatcherStatus,
        CollectionWatcherUpdateRequest, CollectionWatchersStatusResponse, ConfigResponse,
        CreateCollectionRequest, EvidenceResponse, HealthResponse, IngestResponse, ReindexRequest,
        ReindexResponse, RetrieveControlsResponse, RetrieveRequest, RetrieveResponse,
        RetrieveResultResponse, RetrieveTimingResponse, SourceResponse, TaskCreatedResponse,
        TaskEventsResponse, TaskSummaryResponse, COLLECTION_CLI_API_PARITY,
    };
    use verbatim_core::collection::{
        CollectionRecord, CollectionRoot, CollectionRootKind, CollectionStatus,
        CollectionSyncReport,
    };
    use verbatim_core::config::ConfigReloadMetadata;
    use verbatim_core::task::{
        TaskEndpointSummary, TaskEvent, TaskId, TaskKind, TaskProgressSnapshot, TaskSpan,
        TaskStatus, TaskSummary,
    };
    use verbatim_core::types::{SourceId, SourceLocator};

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
            &["collection", "--help"],
            &["collection", "create", "--help"],
            &["collection", "add-root", "--help"],
            &["collection", "list", "--help"],
            &["collection", "get", "--help"],
            &["collection", "delete", "--help"],
            &["collection", "remove", "--help"],
            &["collection", "sync", "--help"],
            &["collection", "status", "--help"],
            &["collection", "watch", "--help"],
            &["collection", "watch", "enable", "--help"],
            &["collection", "watch", "disable", "--help"],
            &["collection", "watch", "status", "--help"],
            &["index", "--help"],
            &["index", "gc", "--help"],
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
            &["task", "watch", "--help"],
            &["task", "cancel", "--help"],
            &["task", "resume", "--help"],
        ];

        for args in cases {
            let (code, stdout, stderr, _, _) = run_mock(args.iter().copied());
            assert_eq!(code.unwrap(), 0, "args: {args:?}");
            assert!(stdout.contains("Usage:"), "stdout: {stdout}");
            assert!(stderr.is_empty(), "stderr: {stderr}");
        }
    }

    #[test]
    fn help_examples_are_available_for_every_command_path() {
        let cases: &[&[&str]] = &[
            &["--help"],
            &["source", "--help"],
            &["source", "add", "--help"],
            &["source", "list", "--help"],
            &["source", "inspect", "--help"],
            &["source", "remove", "--help"],
            &["source", "check", "--help"],
            &["collection", "--help"],
            &["collection", "create", "--help"],
            &["collection", "add-root", "--help"],
            &["collection", "list", "--help"],
            &["collection", "get", "--help"],
            &["collection", "delete", "--help"],
            &["collection", "remove", "--help"],
            &["collection", "sync", "--help"],
            &["collection", "status", "--help"],
            &["collection", "watch", "--help"],
            &["collection", "watch", "enable", "--help"],
            &["collection", "watch", "disable", "--help"],
            &["collection", "watch", "status", "--help"],
            &["index", "--help"],
            &["index", "gc", "--help"],
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
            &["task", "watch", "--help"],
            &["task", "cancel", "--help"],
            &["task", "resume", "--help"],
        ];

        for args in cases {
            let (code, stdout, stderr, _, _) = run_mock(args.iter().copied());
            assert_eq!(code.unwrap(), 0, "args: {args:?}");
            assert!(
                stdout.contains("Examples:"),
                "missing examples for args {args:?}: {stdout}"
            );
            assert!(stderr.is_empty(), "stderr: {stderr}");
        }
    }

    #[test]
    fn collection_cli_api_parity_inventory_matches_clap_commands() {
        let mut actual = collection_leaf_command_paths_from_clap();
        actual.sort();
        let mut expected = COLLECTION_CLI_API_PARITY
            .iter()
            .map(|entry| entry.cli_path)
            .filter(|path| !path.contains(" <name>"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        expected.sort();

        assert_eq!(actual, expected);
        assert!(COLLECTION_CLI_API_PARITY.iter().all(|entry| entry
            .endpoint
            .path_template()
            .starts_with("/api/collections")));
    }

    #[test]
    fn top_level_help_points_to_readme_quick_start_commands() {
        let (code, help, stderr, _, _) = run_mock(["--help"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(help.contains("long-running daemon"));
        assert!(help.contains("verbatim config init"));
        assert!(help.contains("verbatim daemon install"));
        assert!(help.contains("verbatim source add ./paper.pdf"));
        assert!(help.contains("verbatim retrieve --show-debug"));
        assert!(help.contains("verbatim task wait --help"));
    }

    #[test]
    fn command_group_help_mentions_relationships_and_examples() {
        let (code, source_help, stderr, _, _) = run_mock(["source", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(source_help.contains("Sources are daemon-registered paths"));
        assert!(source_help.contains("verbatim source add ./paper.pdf"));

        let (code, collection_help, stderr, _, _) = run_mock(["collection", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(collection_help.contains("materialize filesystem membership"));
        assert!(collection_help.contains("verbatim collection add-root articles"));

        let (code, config_help, stderr, _, _) = run_mock(["config", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(config_help.contains("~/.config/verbatim/config.toml"));
        assert!(config_help.contains("redacted runtime config"));

        let (code, daemon_help, stderr, _, _) = run_mock(["daemon", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(daemon_help.contains("systemctl --user enable --now verbatim"));
        assert!(daemon_help.contains("Start the daemon before source"));

        let (code, task_help, stderr, _, _) = run_mock(["task", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(task_help.contains("Task ids are returned by --background"));
        assert!(task_help.contains("verbatim task wait --timeout 25m"));
        assert!(task_help.contains("verbatim task resume"));
    }

    #[test]
    fn ingest_help_documents_force_and_embedding_profile_caveats() {
        let (code, help, stderr, _, _) = run_mock(["ingest", "--help"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(help.contains("verbatim ingest --force"));
        assert!(help.contains("--force is only for all-source ingest"));
        assert!(help.contains("rejected when SOURCE_ID is set"));
        assert!(help.contains("--force cannot be combined with --vectors-only"));
        assert!(help.contains("--embedding-profile rebuilds vectors"));
        assert!(help.contains("--vectors-only"));
        assert!(help.contains("set [embedding].profile_id"));
    }

    #[test]
    fn retrieve_help_documents_debug_locator_and_generation_caveats() {
        let (code, help, stderr, _, _) = run_mock(["retrieve", "--help"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(help.contains("retrieve never invokes chat generation"));
        assert!(help.contains("--show-debug includes deterministic"));
        assert!(help.contains("dense/BM25/RRF/rerank"));
        assert!(help.contains("--show-locator"));
        assert!(help.contains("structured locator/provenance"));
        assert!(help.contains("verbatim retrieve --format json --show-debug"));
    }

    #[test]
    fn ask_and_retrieve_help_distinguish_generation_from_context_only() {
        let (code, ask_help, stderr, _, _) = run_mock(["ask", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(ask_help.contains("Generate a cited answer"));
        assert!(ask_help.contains("Normal ask invokes the configured chat model"));
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
    fn task_wait_help_mentions_timeout_controls_and_examples() {
        let (code, help, stderr, _, _) = run_mock(["task", "wait", "--help"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(help.contains("--timeout <DURATION>"));
        assert!(help.contains("--no-timeout"));
        assert!(help.contains("verbatim task wait --timeout 25m"));
        assert!(help.contains("verbatim task wait --no-timeout"));
        assert!(help.contains("model timeout_seconds do not cap task streams"));
        assert!(help.contains("cli.task_wait_timeout_seconds"));
        assert!(help.contains("separate from model timeout_seconds"));
    }

    #[test]
    fn task_wait_timeout_parser_accepts_seconds_and_duration_suffixes() {
        assert_eq!(
            parse_task_wait_timeout("1500").unwrap().0,
            Duration::from_secs(1500)
        );
        assert_eq!(
            parse_task_wait_timeout("25m").unwrap().0,
            Duration::from_secs(1500)
        );
        assert_eq!(
            parse_task_wait_timeout("2h").unwrap().0,
            Duration::from_secs(7200)
        );
        assert!(parse_task_wait_timeout("0s").is_err());
        assert!(parse_task_wait_timeout("10ms").is_err());
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
    fn index_gc_dry_run_calls_daemon_and_reports_plan() {
        let (code, stdout, stderr, client, _) = run_mock(["index", "gc", "--dry-run"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["index_gc:true"]);
        assert!(stdout.contains("Index GC dry-run"));
        assert!(stdout.contains("planned: 1 artifact(s), 2.0KiB approximate reclaimable"));
        assert!(stdout.contains("Planned removals:"));
        assert!(stdout.contains("kind=generation profile=default generation=1"));
        assert!(stdout.contains("Skipped:"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn index_gc_apply_calls_daemon_and_reports_removed_artifacts() {
        let (code, stdout, stderr, client, _) = run_mock(["index", "gc"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["index_gc:false"]);
        assert!(stdout.contains("Index GC:"));
        assert!(stdout.contains("removed: 1 artifact(s), 2.0KiB reclaimed"));
        assert!(stdout.contains("Removed artifacts:"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn collection_commands_call_daemon_client() {
        let (code, stdout, stderr, client, _) =
            run_mock(["collection", "create", "articles", "--ignore", "drafts/"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["create_collection"]);
        assert_eq!(
            client
                .last_collection_create
                .borrow()
                .as_ref()
                .unwrap()
                .ignore_patterns,
            vec!["drafts/".to_string()]
        );
        assert!(stdout.contains("Collection:"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) =
            run_mock(["collection", "add-root", "articles", "/tmp/articles"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["add_collection_root:articles"]
        );
        assert_eq!(
            client.last_collection_root.borrow().as_ref().unwrap().path,
            "/tmp/articles"
        );
        assert!(stdout.contains("Roots:"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock(["collection", "list"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["list_collections"]);
        assert!(stdout.contains("Collections:"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock(["collection", "get", "articles"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["get_collection:articles"]
        );
        assert!(stdout.contains("Members:"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock([
            "collection",
            "sync",
            "articles",
            "--max-depth",
            "7",
            "/tmp/articles/one.md",
        ]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["sync_collection:articles"]
        );
        let request = client.last_collection_sync.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.max_depth, Some(7));
        assert_eq!(request.paths[0].path, "/tmp/articles/one.md");
        assert!(stdout.contains("Synced 1 member"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock(["collection", "status", "articles"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["collection_status:articles"]
        );
        assert!(stdout.contains("Collection status:"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock(["collection", "remove", "articles"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["delete_collection:articles"]
        );
        assert!(stdout.contains("Deleted collection: articles"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn collection_watch_commands_call_daemon_client() {
        let (code, stdout, stderr, client, _) = run_mock(["collection", "watch", "status"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["list_collection_watcher_statuses"]
        );
        assert!(stdout.contains("Collection watchers:"));
        assert!(stdout.contains("name=articles"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) =
            run_mock(["collection", "watch", "status", "articles"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["collection_watcher_status:articles"]
        );
        assert!(stdout.contains("Collection watcher:"));
        assert!(stdout.contains("last_task_id: task-1"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) = run_mock([
            "collection",
            "watch",
            "enable",
            "articles",
            "--no-auto-index",
        ]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["update_collection_watcher:articles"]
        );
        assert_eq!(
            client.last_watcher_update.borrow().as_ref().unwrap(),
            &CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(false),
            }
        );
        assert!(stdout.contains("watch_enabled: true"));
        assert!(stdout.contains("auto_index_enabled: false"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr, client, _) =
            run_mock(["collection", "watch", "disable", "articles"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.last_watcher_update.borrow().as_ref().unwrap(),
            &CollectionWatcherUpdateRequest {
                enabled: false,
                auto_index_enabled: None,
            }
        );
        assert!(stdout.contains("watch_enabled: false"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn collection_stdin_path_builder_preserves_logical_lines() {
        let paths = collection_sync_stdin_paths("../drafts/articles/articles/Areskapitalon.md\n\n")
            .unwrap();

        assert_eq!(paths.len(), 1);
        assert!(paths[0]
            .path
            .ends_with("../drafts/articles/articles/Areskapitalon.md"));
        assert_eq!(
            paths[0].logical_path.as_deref(),
            Some("../drafts/articles/articles/Areskapitalon.md")
        );
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
                collection_filter: CollectionFilterRequest::default(),
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
        assert!(stdout.contains("phase=embedding"));
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
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["wait_task:task-1:None:config"]
        );

        let (code, _, _, client, _) = run_mock(["task", "wait", "--timeout", "25m", "task-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["wait_task:task-1:None:bounded(1500s)"]
        );

        let (code, _, _, client, _) = run_mock(["task", "wait", "--no-timeout", "task-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["wait_task:task-1:None:unbounded"]
        );

        let (code, _, _, client, _) = run_mock(["task", "watch", "task-1", "--after", "4"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["wait_task:task-1:Some(4):unbounded"]
        );

        let (code, stdout, _, client, _) = run_mock(["task", "cancel", "task-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["cancel_task:task-1"]);
        assert!(stdout.contains("status: cancelled"));

        let (code, stdout, _, client, _) = run_mock(["task", "resume", "task-1"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["resume_task:task-1"]);
        assert!(stdout.contains("Task: task-1"));
    }

    #[test]
    fn task_wait_timeout_and_no_timeout_conflict() {
        let (code, stdout, stderr, _, _) =
            run_mock(["task", "wait", "--timeout", "1s", "--no-timeout", "task-1"]);

        assert_eq!(code.unwrap_err(), 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("--timeout"));
        assert!(stderr.contains("--no-timeout"));
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
                collection_filter: CollectionFilterRequest::default(),
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
                collection_filter: CollectionFilterRequest::default(),
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
    fn retrieve_and_ask_collection_flags_are_plumbed() {
        let (code, _, stderr, client, _) = run_mock([
            "retrieve",
            "--collection",
            "articles",
            "--collection",
            "areskapitalon",
            "--require-fresh",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(
            request.collection_filter.names,
            vec!["articles".to_string(), "areskapitalon".to_string()]
        );
        assert!(request.collection_filter.require_fresh);

        let (code, _, stderr, client, _) =
            run_mock(["ask", "--collection", "articles", "What", "is", "cited?"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        let request = client.last_ask.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(
            request.collection_filter.names,
            vec!["articles".to_string()]
        );
        assert!(!request.collection_filter.require_fresh);
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
                collection_filter: CollectionFilterRequest::default(),
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
                collection_filter: CollectionFilterRequest::default(),
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
        last_collection_create: RefCell<Option<CreateCollectionRequest>>,
        last_collection_root: RefCell<Option<AddCollectionRootRequest>>,
        last_collection_sync: RefCell<Option<CollectionSyncRequest>>,
        last_watcher_update: RefCell<Option<CollectionWatcherUpdateRequest>>,
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

        fn create_collection(
            &self,
            request: &CreateCollectionRequest,
        ) -> client::CliResult<CollectionResponse> {
            self.calls.borrow_mut().push("create_collection".into());
            self.last_collection_create.replace(Some(request.clone()));
            Ok(sample_collection_response())
        }

        fn add_collection_root(
            &self,
            name: &str,
            request: &AddCollectionRootRequest,
        ) -> client::CliResult<CollectionResponse> {
            self.calls
                .borrow_mut()
                .push(format!("add_collection_root:{name}"));
            self.last_collection_root.replace(Some(request.clone()));
            Ok(sample_collection_response())
        }

        fn list_collections(&self) -> client::CliResult<Vec<CollectionRecord>> {
            self.calls.borrow_mut().push("list_collections".into());
            Ok(vec![sample_collection_record()])
        }

        fn get_collection(&self, name: &str) -> client::CliResult<CollectionResponse> {
            self.calls
                .borrow_mut()
                .push(format!("get_collection:{name}"));
            Ok(sample_collection_response())
        }

        fn delete_collection(&self, name: &str) -> client::CliResult<()> {
            self.calls
                .borrow_mut()
                .push(format!("delete_collection:{name}"));
            Ok(())
        }

        fn sync_collection(
            &self,
            name: &str,
            request: &CollectionSyncRequest,
        ) -> client::CliResult<CollectionSyncResponse> {
            self.calls
                .borrow_mut()
                .push(format!("sync_collection:{name}"));
            self.last_collection_sync.replace(Some(request.clone()));
            Ok(CollectionSyncResponse {
                report: sample_collection_sync_report(),
            })
        }

        fn collection_status(&self, name: &str) -> client::CliResult<CollectionStatusResponse> {
            self.calls
                .borrow_mut()
                .push(format!("collection_status:{name}"));
            Ok(CollectionStatusResponse {
                status: CollectionStatus {
                    collection: sample_collection_record(),
                    root_count: 1,
                    member_count: 1,
                },
            })
        }

        fn list_collection_watcher_statuses(
            &self,
        ) -> client::CliResult<CollectionWatchersStatusResponse> {
            self.calls
                .borrow_mut()
                .push("list_collection_watcher_statuses".into());
            Ok(CollectionWatchersStatusResponse {
                watchers: vec![sample_collection_watcher_status()],
            })
        }

        fn collection_watcher_status(
            &self,
            name: &str,
        ) -> client::CliResult<CollectionWatcherResponse> {
            self.calls
                .borrow_mut()
                .push(format!("collection_watcher_status:{name}"));
            Ok(CollectionWatcherResponse {
                collection: sample_collection_record(),
                watcher: sample_collection_watcher_status(),
            })
        }

        fn update_collection_watcher(
            &self,
            name: &str,
            request: &CollectionWatcherUpdateRequest,
        ) -> client::CliResult<CollectionWatcherResponse> {
            self.calls
                .borrow_mut()
                .push(format!("update_collection_watcher:{name}"));
            self.last_watcher_update.replace(Some(request.clone()));
            Ok(CollectionWatcherResponse {
                collection: sample_collection_record(),
                watcher: CollectionWatcherStatus {
                    watch_enabled: request.enabled,
                    auto_index_enabled: request.auto_index_enabled.unwrap_or(true),
                    ..sample_collection_watcher_status()
                },
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

        fn index_gc(&self, request: &IndexGcRequest) -> client::CliResult<IndexGcResponse> {
            self.calls
                .borrow_mut()
                .push(format!("index_gc:{}", request.dry_run));
            Ok(sample_index_gc_response(request.dry_run))
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
            timeout: TaskWaitTimeout,
            _stdout: &mut W,
        ) -> client::CliResult<()>
        where
            W: Write,
        {
            self.calls.borrow_mut().push(format!(
                "wait_task:{task_id}:{after:?}:{}",
                task_wait_timeout_label(timeout)
            ));
            Ok(())
        }

        fn cancel_task(&self, task_id: &str) -> client::CliResult<TaskSummaryResponse> {
            self.calls
                .borrow_mut()
                .push(format!("cancel_task:{task_id}"));
            Ok(sample_task_response(TaskStatus::Cancelled))
        }

        fn resume_task(&self, task_id: &str) -> client::CliResult<TaskSummaryResponse> {
            self.calls
                .borrow_mut()
                .push(format!("resume_task:{task_id}"));
            Ok(sample_task_response(TaskStatus::Queued))
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
            Ok(ConfigResponse {
                config: serde_json::json!({"daemon": {"bind": "127.0.0.1:7700"}}),
                reload: ConfigReloadMetadata {
                    active_config_path: "/tmp/config.toml".into(),
                    loaded_at: "1".into(),
                    last_reload_at: None,
                    last_reload_error: None,
                    last_applied_reload_safe_keys: Vec::new(),
                    last_restart_required_keys: Vec::new(),
                },
            })
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
            CliError::TaskWaitTimedOut { timeout } => {
                CliError::TaskWaitTimedOut { timeout: *timeout }
            }
        }
    }

    fn task_wait_timeout_label(timeout: TaskWaitTimeout) -> String {
        match timeout {
            TaskWaitTimeout::ConfigDefault => "config".into(),
            TaskWaitTimeout::Bounded(duration) => format!("bounded({}s)", duration.as_secs()),
            TaskWaitTimeout::Unbounded => "unbounded".into(),
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

    fn sample_collection_record() -> CollectionRecord {
        CollectionRecord {
            name: "articles".into(),
            ignore_patterns: vec!["drafts/".into()],
            watch_enabled: true,
            auto_index_enabled: true,
            created_at: "1".into(),
            updated_at: "2".into(),
            last_synced_at: Some("3".into()),
            last_sync: Some(sample_collection_sync_report()),
        }
    }

    fn sample_collection_watcher_status() -> CollectionWatcherStatus {
        CollectionWatcherStatus {
            collection_name: "articles".into(),
            watch_enabled: true,
            auto_index_enabled: true,
            active: true,
            ignored_by_config: false,
            watched_root_count: 1,
            pending_event_count: 0,
            last_event_at: Some("4".into()),
            last_sync_at: Some("5".into()),
            last_error: None,
            last_added: 1,
            last_removed: 0,
            last_unchanged: 2,
            last_task_id: Some("task-1".into()),
        }
    }

    fn sample_collection_response() -> CollectionResponse {
        CollectionResponse {
            collection: sample_collection_record(),
            roots: vec![CollectionRoot {
                collection_name: "articles".into(),
                path: PathBuf::from("/tmp/articles"),
                canonical_path: Some(PathBuf::from("/tmp/articles")),
                kind: CollectionRootKind::Directory,
                added_at: "1".into(),
                updated_at: "2".into(),
            }],
            members: vec![verbatim_core::collection::CollectionMember {
                collection_name: "articles".into(),
                source_id: SourceId("src-1".into()),
                logical_path: "one.md".into(),
                source_path: PathBuf::from("/tmp/articles/one.md"),
                updated_at: "3".into(),
            }],
        }
    }

    fn sample_collection_sync_report() -> CollectionSyncReport {
        CollectionSyncReport {
            member_count: 1,
            added: 1,
            removed: 0,
            unchanged: 0,
            scanned_roots: 1,
            max_depth: 32,
            skipped: Vec::new(),
        }
    }

    fn sample_index_gc_response(dry_run: bool) -> IndexGcResponse {
        let entry = verbatim_core::index_gc::IndexGcPlanEntry {
            path: PathBuf::from("/tmp/verbatim/indexes/profiles/default/gen-1"),
            kind: verbatim_core::index_gc::IndexGcArtifactKind::Generation,
            profile_id: Some("default".into()),
            generation: Some(1),
            approximate_bytes: 2048,
            reason: "older than current generation 3 plus 1 retained previous generation(s)".into(),
        };
        let skipped = verbatim_core::index_gc::IndexGcSkippedEntry {
            path: PathBuf::from("/tmp/verbatim/indexes/staging-1-fresh"),
            kind: Some(verbatim_core::index_gc::IndexGcArtifactKind::Staging),
            profile_id: None,
            generation: None,
            reason: "staging directory age 0s is below stale threshold 86400s".into(),
        };
        IndexGcResponse {
            dry_run,
            policy: verbatim_core::index_gc::IndexGcConfig {
                retain_previous_generations: 1,
                stale_staging_seconds: 86_400,
            },
            plan: verbatim_core::index_gc::IndexGcPlan {
                entries: vec![entry.clone()],
                skipped: vec![skipped],
                approximate_reclaim_bytes: entry.approximate_bytes,
            },
            apply: if dry_run {
                verbatim_core::index_gc::IndexGcApplyReport::default()
            } else {
                verbatim_core::index_gc::IndexGcApplyReport {
                    removed: vec![entry.clone()],
                    reclaimed_bytes: entry.approximate_bytes,
                }
            },
        }
    }

    fn collection_leaf_command_paths_from_clap() -> Vec<String> {
        let command = Cli::command();
        let collection = command
            .find_subcommand("collection")
            .expect("collection command exists");
        let mut paths = Vec::new();
        collect_leaf_command_paths(collection, "collection", &mut paths);
        paths
    }

    fn collect_leaf_command_paths(command: &clap::Command, prefix: &str, paths: &mut Vec<String>) {
        let subcommands = command.get_subcommands().collect::<Vec<_>>();
        if subcommands.is_empty() {
            paths.push(prefix.to_string());
            return;
        }

        for subcommand in subcommands {
            let path = format!("{prefix} {}", subcommand.get_name());
            collect_leaf_command_paths(subcommand, &path, paths);
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
            collections: Vec::new(),
            locator: "PDF p.1 para.1".into(),
            text_preview: "preview".into(),
        }
    }

    fn sample_retrieve_response(request: &RetrieveRequest) -> RetrieveResponse {
        RetrieveResponse {
            task_id: "task-1".into(),
            query: request.question.clone(),
            source_id: request.source_id.clone(),
            collection_filter: None,
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
                collections: Vec::new(),
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
                progress: Some(
                    TaskProgressSnapshot::phase("embedding")
                        .with_counter("embedding_vectors", 4, Some(8))
                        .with_endpoint(TaskEndpointSummary::single_call("embedding", 12)),
                ),
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
