use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{error::ErrorKind, ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use verbatim_core::api::{
    AddCollectionRootRequest, AskRequest, CollectionFilterRequest, CollectionSyncPathRequest,
    CollectionSyncRequest, CollectionWatcherUpdateRequest, CreateCollectionRequest, IndexGcRequest,
    IndexProfileDeleteRequest, ReindexRequest, RetrieveRequest, VectorJsonCleanupRequest,
};
#[cfg(test)]
use verbatim_core::api::{
    ChunkingProfileStatusResponse, EmbeddingCapabilityStatusResponse, IndexGcResponse,
    IndexProfileDeleteResponse, IndexStatusResponse, VectorJsonCleanupResponse,
};

mod auth;
mod client;
mod local;
mod render;
mod source_cli;
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

    match dispatch(cli, stdout, stderr, client, local) {
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

fn ask_context_controls_present(
    limit: Option<usize>,
    page_size: Option<usize>,
    page: Option<usize>,
) -> bool {
    limit.is_some() || page_size.is_some() || page.is_some()
}

fn collection_filter_request(
    collection_names: Vec<String>,
    require_fresh: bool,
    allow_stale: bool,
) -> CollectionFilterRequest {
    let require_fresh = if collection_names.is_empty() {
        require_fresh
    } else {
        require_fresh || !allow_stale
    };
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
        RetrieveFormat::Snippets => render::write_retrieve_snippets(stdout, response),
        RetrieveFormat::Tsv => render::write_retrieve_tsv(stdout, response),
        RetrieveFormat::Csv => render::write_retrieve_csv(stdout, response),
    }
}

fn write_retrieve_debug_output<W>(
    stderr: &mut W,
    response: &verbatim_core::api::RetrieveResponse,
    verbose: bool,
) -> std::io::Result<()>
where
    W: Write,
{
    if verbose {
        render::write_retrieve_debug_response(stderr, response)
    } else {
        render::write_retrieve_debug_summary(stderr, response)
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

fn dispatch<W, E, C, L>(
    cli: Cli,
    stdout: &mut W,
    stderr: &mut E,
    client: &C,
    local: &L,
) -> Result<u8, CliError>
where
    W: Write,
    E: Write,
    C: DaemonClient,
    L: LocalActions,
{
    match cli.command {
        Commands::Source { command } => source_cli::run_source(command, stdout, client),
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
            allow_stale,
            embedding_profile,
            show_retrieval,
            context_only,
            no_generate,
            limit,
            page_size,
            page,
            format,
            background,
        } => {
            let context_only = context_only || no_generate;
            let question = question.join(" ");
            let collection_filter =
                collection_filter_request(collection, require_fresh, allow_stale);
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
                    limit,
                    page_size,
                    page,
                    fast: false,
                    rerank: None,
                    dense_top_k: None,
                    bm25_top_k: None,
                    rerank_top_n: None,
                    bypass_cache: false,
                    include_debug: show_retrieval,
                    include_debug_packs: false,
                    include_locator: format == RetrieveFormat::Json,
                    passage: false,
                };
                let response = client.retrieve(&request)?;
                if show_retrieval {
                    write_retrieve_debug_output(stderr, &response, false)?;
                }
                if format == RetrieveFormat::Json && show_retrieval {
                    render::write_retrieve_json_without_debug(stdout, &response)?;
                } else {
                    write_retrieve_with_format(stdout, &response, format)?;
                }
                return Ok(0);
            }

            if ask_context_controls_present(limit, page_size, page) {
                return Err(CliError::Api(
                    "--limit, --page-size, and --page are only supported with --context-only or --no-generate".into(),
                ));
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
                limit: None,
                page_size: None,
                page: None,
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
            allow_stale,
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
            no_cache,
            show_debug,
            verbose,
            show_locator,
            format,
            text_only,
            passage,
        } => {
            let format = if text_only {
                RetrieveFormat::Snippets
            } else {
                format.unwrap_or(RetrieveFormat::Markdown)
            };
            let include_locator = show_locator || show_debug || format == RetrieveFormat::Json;
            let request = RetrieveRequest {
                question: question.join(" "),
                source_id,
                collection_filter: collection_filter_request(
                    collection,
                    require_fresh,
                    allow_stale,
                ),
                embedding_profile_id: embedding_profile,
                limit,
                page_size,
                page,
                fast,
                rerank: rerank_override(rerank, no_rerank),
                dense_top_k,
                bm25_top_k,
                rerank_top_n,
                bypass_cache: no_cache,
                include_debug: show_debug,
                include_debug_packs: show_debug && verbose,
                include_locator,
                passage,
            };
            let response = client.retrieve(&request)?;
            if show_debug {
                write_retrieve_debug_output(stderr, &response, verbose)?;
            }
            if format == RetrieveFormat::Json && show_debug {
                render::write_retrieve_json_without_debug(stdout, &response)?;
            } else {
                write_retrieve_with_format(stdout, &response, format)?;
            }
            Ok(0)
        }
        Commands::Resolve { reference, format } => {
            run_resolve(&reference, format, stdout)?;
            Ok(0)
        }
        Commands::Evidence { eid } => {
            let evidence = client.get_evidence(&eid)?;
            render::write_evidence(stdout, &evidence)?;
            Ok(0)
        }
        Commands::Config { command } => run_config(command, stdout, client, local),
        Commands::Daemon { command } => run_daemon(command, stdout, client, local),
        Commands::Task { command } => run_task(command, stdout, client, local),
    }
}

/// Run the `resolve` command: parse a canonical reference and display its
/// normalized form.
fn run_resolve<W>(
    reference: &str,
    format: Option<ResolveFormat>,
    stdout: &mut W,
) -> Result<(), CliError>
where
    W: Write,
{
    let registry = verbatim_core::profiles::ProfileRegistry::new();
    let parsed = registry.try_parse(reference).ok_or_else(|| {
        CliError::Api(format!(
            "could not parse \"{reference}\" as a canonical reference"
        ))
    })?;

    // Build normalized key from start components
    let normalized: String = parsed
        .start
        .iter()
        .map(|c| c.value.to_lowercase().replace(' ', ""))
        .collect::<Vec<_>>()
        .join(":");

    match format {
        Some(ResolveFormat::Json) => {
            let end_display = parsed.end.as_ref().map(|end| {
                end.iter()
                    .map(|c| c.value.clone())
                    .collect::<Vec<_>>()
                    .join(":")
            });
            writeln!(
                stdout,
                "{}",
                serde_json::json!({
                    "profile": parsed.profile_id,
                    "raw": parsed.raw,
                    "display": parsed.display,
                    "normalized": normalized,
                    "start": parsed.start.iter().map(|c| serde_json::json!({
                        "level": c.level,
                        "value": c.value,
                        "ordinal": c.ordinal,
                    })).collect::<Vec<_>>(),
                    "end": end_display,
                })
            )?;
        }
        Some(ResolveFormat::Text) | None => {
            writeln!(stdout, "display:   {}", parsed.display)?;
            writeln!(stdout, "normalized: {}", normalized)?;
            writeln!(stdout, "profile:   {}", parsed.profile_id)?;
        }
    }

    Ok(())
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
            render::write_collection_root_summary(stdout, &response)?;
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
        IndexCommand::Status => {
            let response = client.index_status()?;
            render::write_index_status(stdout, &response)?;
        }
        IndexCommand::Gc { dry_run } => {
            let response = client.index_gc(&IndexGcRequest { dry_run })?;
            render::write_index_gc(stdout, &response)?;
        }
        IndexCommand::DeleteProfile {
            profile_id,
            dry_run,
            confirm,
            allow_active,
        } => {
            let response = client.index_delete_profile(&IndexProfileDeleteRequest {
                profile_id,
                dry_run,
                confirm,
                allow_active,
            })?;
            render::write_index_profile_delete(stdout, &response)?;
        }
        IndexCommand::VectorJsonCleanup {
            dry_run,
            execute,
            confirm,
        } => {
            let response = client.vector_json_cleanup(&VectorJsonCleanupRequest {
                dry_run: dry_run || !execute,
                confirm,
            })?;
            render::write_vector_json_cleanup(stdout, &response)?;
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
        ConfigCommand::Show { full } => {
            let config = client.get_config()?;
            if full {
                render::write_config(stdout, &config)?;
            } else {
                render::write_config_compact(stdout, &config)?;
            }
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
        DaemonCommand::Status {
            auto_start,
            details,
        } => {
            let health = daemon_status_health(client, local, auto_start)?;
            if details {
                render::write_health(stdout, &health)?;
            } else {
                render::write_health_compact(stdout, &health)?;
            }
            Ok(daemon_status_exit_code(&health))
        }
        DaemonCommand::Install { force } => {
            let path = local.daemon_install(force)?;
            local::write_daemon_install(stdout, &path)?;
            Ok(0)
        }
    }
}

fn daemon_status_exit_code(health: &verbatim_core::api::HealthResponse) -> u8 {
    if health.readiness.retrieval_ready {
        0
    } else {
        1
    }
}

fn daemon_status_health<C, L>(
    client: &C,
    local: &L,
    auto_start: bool,
) -> Result<verbatim_core::api::HealthResponse, CliError>
where
    C: DaemonClient,
    L: LocalActions,
{
    match client.health() {
        Ok(health) => Ok(health),
        Err(CliError::DaemonUnreachable(original)) => {
            let should_auto_start = if auto_start {
                true
            } else {
                local.daemon_idle_exit_auto_start_on_cli()?
            };
            if !should_auto_start {
                return Err(CliError::DaemonUnreachable(original));
            }
            if let Err(start_error) = local.daemon_start_user_service() {
                return Err(CliError::DaemonUnreachable(format!(
                    "could not reach verbatim daemon; auto-start failed\n\
                     original failure: {original}\n\
                     auto-start failure: {start_error}\n\
                     Start it manually with: systemctl --user start verbatim"
                )));
            }
            client.health().map_err(|retry_error| match retry_error {
                CliError::DaemonUnreachable(retry) => CliError::DaemonUnreachable(format!(
                    "started verbatim.service but could not reach verbatim daemon on retry\n\
                     original failure: {original}\n\
                     retry failure: {retry}\n\
                     Start it manually with: systemctl --user start verbatim"
                )),
                other => other,
            })
        }
        Err(error) => Err(error),
    }
}

fn run_task<W, C, L>(
    command: TaskCommand,
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
        TaskCommand::List { details } => {
            let response = client.list_tasks()?;
            let history = local.load_task_list_history().ok().flatten();
            let update = render::write_task_list(
                stdout,
                &response,
                details,
                history.as_ref(),
                local.now_millis(),
            )?;
            match update {
                render::TaskListHistoryUpdate::Store(history) => {
                    let _ = local.store_task_list_history(&history);
                }
                render::TaskListHistoryUpdate::Clear => {
                    let _ = local.clear_task_list_history();
                }
            }
        }
        TaskCommand::Show { task_id } => {
            let response = client.get_task(&task_id)?;
            render::write_task_summary(stdout, &response.task, &response.spans)?;
        }
        TaskCommand::Profile { task_id, format } => {
            let response = client.get_task_profile(&task_id)?;
            match format {
                TaskProfileFormat::Human => render::write_task_profile(stdout, &response)?,
                TaskProfileFormat::Json => render::write_task_profile_json(stdout, &response)?,
            }
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
  verbatim source list --details
  verbatim source list --limit 10 --details
  verbatim source list --status Indexed --details

Default output is compact (total count + small preview).
Pass --details for per-source path and full metadata.
Use --limit N (N >= 1) to control preview/detail count.
Filter with --status <status> (Pending, Indexed, Stale).
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

const SOURCE_RELOCATE_AFTER_HELP: &str = r#"Examples:
  verbatim source relocate <source-id> /srv/verbatim/renamed.md
NEW_PATH must be visible to the daemon host, and the file content must be unchanged.
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
  verbatim index delete-profile old-profile --dry-run
  verbatim index delete-profile old-profile --confirm
  verbatim index vector-json-cleanup --dry-run
  verbatim index vector-json-cleanup --execute --confirm

Index maintenance operates through the daemon. Vector JSON cleanup clears only
legacy JSON payload copies for rows that already have a valid vector BLOB.
"#;

const INDEX_STATUS_AFTER_HELP: &str = r#"Examples:
  verbatim index status

Show the active embedding profile capability and chunking status for the
current daemon state.
"#;

const INDEX_GC_AFTER_HELP: &str = r#"Examples:
  verbatim index gc --dry-run
  verbatim index gc

GC removes old per-profile gen-* index generations and stale staging-*
directories according to [index_gc] policy. It does not delete sources, SQLite
data, embedding cache, or image artifacts.
"#;

const INDEX_DELETE_PROFILE_AFTER_HELP: &str = r#"Examples:
  verbatim index delete-profile old-profile --dry-run
  verbatim index delete-profile old-profile --confirm
  verbatim index delete-profile old-profile --confirm --allow-active

Deletes profile-scoped vector/index/cache/status metadata and published HNSW
artifacts for an obsolete embedding profile. It preserves sources, chunks,
evidence, collections, and lexical SQLite FTS/BM25 data. Active profile deletion
is refused unless --allow-active is passed.
"#;

const INDEX_VECTOR_JSON_CLEANUP_AFTER_HELP: &str = r#"Examples:
  verbatim index vector-json-cleanup --dry-run
  verbatim index vector-json-cleanup --execute --confirm

New vector writes store compact BLOB payloads and leave vector_json empty.
Legacy JSON-only rows remain readable and are never deleted or cleared by this
cleanup. Dry-run reports eligible, json_only, missing_blob, and malformed_blob
counts for chunk_vectors and embedding_cache without mutating SQLite.

Before --execute, stop write-heavy ingest/reindex work and back up
~/.local/share/verbatim/verbatim.db plus its WAL/SHM files. Execute is opt-in
and transactional: it clears JSON payloads only for rows with a valid BLOB and
skips JSON-only or malformed-BLOB rows. SQLite may not return disk space to the
filesystem until a VACUUM or rebuild-table maintenance step is run.
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
  verbatim ask --context-only --page 2 --page-size 5 "What evidence is relevant?"
  verbatim ask --no-generate --format json "What evidence is relevant?"

Caveats:
  Normal ask invokes the configured chat model after retrieval.
  --context-only and --no-generate return retrieval context without chat
  generation; --background is not supported in that mode.
  --limit, --page-size, and --page only apply with --context-only or
  --no-generate.
  --format only applies with --context-only or --no-generate.
"#;

const RETRIEVE_AFTER_HELP: &str = r#"Examples:
  verbatim retrieve "What does the report conclude?"
  verbatim retrieve --format snippets "What supports it?"
  verbatim retrieve --text-only "What supports it?"
  verbatim retrieve --format tsv "What supports it?"
  verbatim retrieve --format csv "What supports it?"
  verbatim retrieve --source-id <source-id> --page-size 1 "What supports it?"
  verbatim retrieve --collection articles "What evidence is relevant?"
  verbatim retrieve --collection articles --collection areskapitalon "What changed?"
  verbatim retrieve --show-debug "What evidence is relevant?"
  verbatim retrieve --show-debug --verbose "What evidence is relevant?"
  verbatim retrieve --show-locator "What evidence is relevant?"
  verbatim retrieve --format json --show-debug "What evidence is relevant?"

Debugging:
  retrieve never invokes chat generation.
  It returns evidence context without invoking chat generation.
  Default markdown is compact: rank, score, citation, and snippet only.
  Scores are ranked chunk-level scores; canonical multi-locator compact
  snippets show a chunk-internal support unit for the query.
  snippets/text-only omit headers and debug metadata; TSV/CSV emit fixed
  columns: rank, score, citation, collection, source, locator, snippet.
  --collection filters against materialized daemon membership and does not
  rescan collection roots during retrieve.
  --show-debug writes a compact JSON retrieval diagnostic summary with local
  stage spans to stderr.
  --show-debug --verbose writes the full task diagnostics, engine controls,
  timing, locators, internal evidence metadata, and deterministic
  dense/BM25/RRF/rerank ranking details and local stage spans to stderr.
  JSON output retains structured locator/provenance fields and full evidence
  identifiers for evidence lookups, but retrieval debug diagnostics stay on stderr.
"#;

const EVIDENCE_AFTER_HELP: &str = r#"Examples:
  verbatim evidence <evidence-id>

Evidence ids come from retrieve --show-debug --verbose, retrieve --format json,
ask citations, and retrieval debug packs.
"#;

const RESOLVE_AFTER_HELP: &str = r#"Examples:
  verbatim resolve "John 3:16"
  verbatim resolve "1 Cor 13:4-7" --format json
  verbatim resolve "Gen 1:31-2:3"

Parses and normalizes canonical references using known source profiles
(Bible, etc.). The output normalized key can be used to look up the same
passage in any version (ESV, NIV, CUV, etc.) — it is version-independent.

Currently supported: Bible (66-book Protestant canon with abbreviations).
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
  verbatim config show
  verbatim config show --full

Default output is compact (key daemon/embedding/rerank/idle settings).
Pass --full for the complete redacted JSON config.
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
  verbatim daemon status --auto-start
  verbatim daemon status --details

Default output is compact (status + RSS + tasks + idle flags).
Pass --details for full multi-line health output (resources, reclaim, exit).
With --auto-start, status starts the installed user systemd service
with `systemctl --user start verbatim` and retries health once.
This is not systemd socket activation.
"#;

const DAEMON_INSTALL_AFTER_HELP: &str = r#"Examples:
  verbatim daemon install
  verbatim daemon install --force
  systemctl --user daemon-reload
  systemctl --user enable --now verbatim

Install writes ~/.config/systemd/user/verbatim.service, or the equivalent path
under XDG_CONFIG_HOME. Use --force only to replace an existing unit file.
Idle exit can recover with explicit CLI auto-start for status or run
`systemctl --user start verbatim` manually; no socket unit is installed.
"#;

const TASK_AFTER_HELP: &str = r#"Examples:
  verbatim task list
  verbatim task show <task-id>
  verbatim task profile <task-id> --format json
  verbatim task events <task-id>
  verbatim task wait --timeout 25m <task-id>
  verbatim task cancel <task-id>
  verbatim task resume <task-id>

Task ids are returned by --background ingest/reindex/ask commands.
Use task profile to read stored completed-task diagnostics by id without
rerunning the original task work.
"#;

const TASK_LIST_AFTER_HELP: &str = r#"Examples:
  verbatim task list
  verbatim task list --details

List shows an aggregate active task queue summary by default so a large backlog
stays readable. The daemon may also report active total plateau explanations,
bounded turnover/backfill, waiting reason buckets, and stale running tasks. Use
--details to print bounded per-task rows, including current ingest stage and
elapsed stage time when available.
"#;

const TASK_SHOW_AFTER_HELP: &str = r#"Examples:
  verbatim task show <task-id>

Show prints the current task status, request/result summary, progress snapshot,
and bounded phase spans such as ingest stage timings.
"#;

const TASK_PROFILE_AFTER_HELP: &str = r#"Examples:
  verbatim task profile <task-id>
  verbatim task profile <task-id> --format json

Profile reads a persisted task profile by task id. It is a side-effect-free
diagnostic query for completed tasks with stored profiles; legacy/no-profile
tasks and incomplete tasks return unavailable errors.

It does not rerun the original task or redo retrieval, embedding, BM25/dense
search, rerank, chat/generation, citation verifier, or evidence expansion
work.

The default output is compact human-readable text; use --format json for
machine-readable tooling output.
"#;

const TASK_EVENTS_AFTER_HELP: &str = r#"Examples:
  verbatim task events <task-id>
  verbatim task events --after 42 <task-id>

Events are ordered by sequence. Use --after to resume from the last sequence you
already consumed. Progress events include current ingest stage and elapsed time
without source text or vector payloads.
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
        /// Allow retrieval from stale collection membership or stale member indexes.
        #[arg(long = "allow-stale", action = ArgAction::SetTrue, conflicts_with = "require_fresh")]
        allow_stale: bool,
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
        /// Maximum evidence/context entries to consider before pagination.
        ///
        /// Only applies with --context-only or --no-generate.
        #[arg(long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Evidence/context entries per page. Use 1 for agent-sized pages.
        ///
        /// Only applies with --context-only or --no-generate.
        #[arg(long, value_parser = parse_nonzero_usize)]
        page_size: Option<usize>,
        /// 1-based context page number.
        ///
        /// Only applies with --context-only or --no-generate.
        #[arg(long, value_parser = parse_nonzero_usize)]
        page: Option<usize>,
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
        /// Allow retrieval from stale collection membership or stale member indexes.
        #[arg(long = "allow-stale", action = ArgAction::SetTrue, conflicts_with = "require_fresh")]
        allow_stale: bool,
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
        /// Bypass remote vLLM prefix cache for benchmark-style retrieval requests.
        #[arg(long = "no-cache", action = ArgAction::SetTrue)]
        no_cache: bool,
        /// Include deterministic retrieval stage debug metadata in the response.
        ///
        /// Useful for evidence/provenance debugging and agent workflows.
        #[arg(long = "show-debug")]
        show_debug: bool,
        /// Emit the full retrieval debug dump instead of the compact summary.
        #[arg(long, action = ArgAction::SetTrue, requires = "show_debug")]
        verbose: bool,
        /// Include structured locator/provenance fields in the response.
        #[arg(long = "show-locator")]
        show_locator: bool,
        /// Group retrieved evidence from the same chunk as passage blocks before pagination.
        #[arg(long = "passage", action = ArgAction::SetTrue)]
        passage: bool,
        /// Output format. JSON includes structured locator/provenance fields.
        #[arg(long, value_enum)]
        format: Option<RetrieveFormat>,
        /// Alias for --format snippets. Omits headers and debug metadata.
        #[arg(long = "text-only", action = ArgAction::SetTrue, conflicts_with = "format")]
        text_only: bool,
        /// Question text.
        #[arg(required = true, num_args = 1..)]
        question: Vec<String>,
    },
    /// Resolve a canonical reference (e.g., "John 3:16") to its normalized form.
    #[command(
        about = "Parse and normalize a canonical reference (Bible verse, etc.).",
        after_help = RESOLVE_AFTER_HELP
    )]
    Resolve {
        /// The reference string to resolve (e.g., "John 3:16", "1 Cor 13:4-7").
        #[arg(value_name = "REFERENCE")]
        reference: String,
        /// Output format: text (default) or json.
        #[arg(long, value_enum)]
        format: Option<ResolveFormat>,
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
    List {
        /// Show full details (path, status) for every source.
        ///
        /// Default output is a compact summary suitable for agent/LLM consumption.
        /// Pass --details for the full listing humans are used to.
        #[arg(long)]
        details: bool,

        /// Maximum number of sources to show (with --details or in default preview).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,

        /// Filter sources by status (e.g. Indexed, Pending, Error).
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
    },
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
    /// Relocate one externally renamed source without changing its identity.
    #[command(after_help = SOURCE_RELOCATE_AFTER_HELP)]
    Relocate {
        /// Source id whose stored path no longer exists.
        #[arg(value_name = "SOURCE_ID")]
        id: String,
        /// New content-identical file path visible to the daemon host.
        #[arg(value_name = "NEW_PATH")]
        new_path: String,
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
    /// Show active embedding profile capability and chunking status.
    #[command(
        about = "Show active embedding profile capability and chunking status.",
        after_help = INDEX_STATUS_AFTER_HELP
    )]
    Status,
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
    /// Delete obsolete embedding profile index metadata and artifacts.
    #[command(
        name = "delete-profile",
        about = "Delete obsolete embedding profile index metadata and artifacts.",
        after_help = INDEX_DELETE_PROFILE_AFTER_HELP
    )]
    DeleteProfile {
        /// Embedding profile id to delete.
        #[arg(value_name = "PROFILE_ID")]
        profile_id: String,
        /// Show what would be removed without deleting anything.
        #[arg(long = "dry-run", action = ArgAction::SetTrue)]
        dry_run: bool,
        /// Required for non-dry-run deletion.
        #[arg(long = "confirm", action = ArgAction::SetTrue)]
        confirm: bool,
        /// Permit deleting the daemon's active embedding profile and clear resident vectors.
        #[arg(long = "allow-active", action = ArgAction::SetTrue)]
        allow_active: bool,
    },
    /// Clear legacy vector_json payload copies when valid BLOB vectors exist.
    #[command(
        name = "vector-json-cleanup",
        about = "Dry-run or execute cleanup of legacy vector_json payload copies.",
        after_help = INDEX_VECTOR_JSON_CLEANUP_AFTER_HELP
    )]
    VectorJsonCleanup {
        /// Show counts without modifying SQLite. This is also the default when --execute is omitted.
        #[arg(long = "dry-run", action = ArgAction::SetTrue, conflicts_with = "execute")]
        dry_run: bool,
        /// Clear eligible JSON payloads. Requires --confirm.
        #[arg(long = "execute", action = ArgAction::SetTrue, requires = "confirm")]
        execute: bool,
        /// Required with --execute after taking a backup.
        #[arg(long = "confirm", action = ArgAction::SetTrue)]
        confirm: bool,
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
    Show {
        /// Show the full config JSON (default is compact summary).
        #[arg(long)]
        full: bool,
    },
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
    Status {
        /// Start the installed user systemd service and retry health once if unreachable.
        #[arg(long)]
        auto_start: bool,
        /// Show full multi-line health output (resources, idle reclaim, idle exit).
        #[arg(long)]
        details: bool,
    },
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
    /// List active queued/running tasks with aggregate progress.
    #[command(
        about = "List active queued/running tasks with aggregate progress.",
        after_help = TASK_LIST_AFTER_HELP
    )]
    List {
        /// Show bounded per-task detail rows after the aggregate summary.
        #[arg(long, alias = "verbose", action = ArgAction::SetTrue)]
        details: bool,
    },
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
    /// Read a persisted task profile without rerunning task work.
    #[command(
        about = "Read persisted task profile diagnostics without rerunning work.",
        after_help = TASK_PROFILE_AFTER_HELP
    )]
    Profile {
        /// Completed task id.
        #[arg(value_name = "TASK_ID")]
        task_id: String,
        /// Output format.
        #[arg(long, value_enum, default_value_t = TaskProfileFormat::Human)]
        format: TaskProfileFormat,
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
    Snippets,
    Tsv,
    Csv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ResolveFormat {
    #[value(alias = "text")]
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TaskProfileFormat {
    Human,
    Json,
}

#[cfg(test)]
static CONFIG_INIT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
#[test]
fn index_status_help_includes_examples() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let code = run(["index", "status", "--help"], &mut stdout, &mut stderr);
    let help = String::from_utf8(stdout).unwrap();

    assert_eq!(code.unwrap(), 0);
    assert!(stderr.is_empty());
    assert!(help.contains("Usage:"));
    assert!(help.contains("Examples:"));
    assert!(help.contains("verbatim index status"));
    assert!(help.contains("-h, --help"));
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[path = "config_template_tests.rs"]
    mod config_template_tests;
    #[path = "issue_332_cli_relocation_tests.rs"]
    mod issue_332_cli_relocation_tests;

    use serde_json::Value;
    use verbatim_core::api::{
        AddCollectionRootRequest, AddCollectionRootResponse, AddSourceResponse, AuditReceipt,
        AuditReceiptResult, CheckStaleResponse, CitationResponse, CollectionResponse,
        CollectionStatusResponse, CollectionSyncRequest, CollectionSyncResponse,
        CollectionWatcherResponse, CollectionWatcherStatus, CollectionWatcherUpdateRequest,
        CollectionWatchersStatusResponse, ConfigResponse, CreateCollectionRequest,
        EvidenceResponse, HealthResponse, IdleExitActivitySnapshot, IdleExitHealth,
        IdleReclaimActivitySnapshot, IdleReclaimBackendResult, IdleReclaimCycleResult,
        IdleReclaimHealth, IngestResponse, ReadinessHealth, ReindexRequest, ReindexResponse,
        RetrieveControlsResponse, RetrieveRequest, RetrieveResponse, RetrieveResultResponse,
        RetrieveTimingResponse, SourceResponse, TaskCreatedResponse, TaskEmbeddingWaitAggregate,
        TaskEventsResponse, TaskListAggregate, TaskListResponse, TaskProfileResponse,
        TaskQueueTurnover, TaskQueueTurnoverWindow, TaskReasonBucket, TaskStaleRunningAggregate,
        TaskSummaryResponse, AUDIT_RECEIPT_VERSION, COLLECTION_CLI_API_PARITY,
    };
    use verbatim_core::collection::{
        CollectionRecord, CollectionRoot, CollectionRootKind, CollectionStatus,
        CollectionSyncReport,
    };
    use verbatim_core::config::ConfigReloadMetadata;
    use verbatim_core::task::{
        IngestTaskStage, TaskEndpointSummary, TaskEvent, TaskId, TaskKind, TaskProfile,
        TaskProgressSnapshot, TaskSpan, TaskStatus, TaskSummary,
    };
    use verbatim_core::types::{
        RetrievalDenseVectorPath, RetrievalRerankStatus, SourceId, SourceLocator,
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
            &["index", "vector-json-cleanup", "--help"],
            &["ingest", "--help"],
            &["reindex", "--help"],
            &["ask", "--help"],
            &["retrieve", "--help"],
            &["evidence", "--help"],
            &["config", "--help"],
            &["config", "init", "--help"],
            &["config", "show", "--help"],
            &["config", "validate", "--help"],
            &["index", "status", "--help"],
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
            &["index", "vector-json-cleanup", "--help"],
            &["ingest", "--help"],
            &["reindex", "--help"],
            &["ask", "--help"],
            &["retrieve", "--help"],
            &["evidence", "--help"],
            &["config", "--help"],
            &["config", "init", "--help"],
            &["config", "show", "--help"],
            &["config", "validate", "--help"],
            &["index", "status", "--help"],
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
    fn daemon_help_mentions_idle_exit_activation() {
        let (code, status_help, status_stderr, _, _) = run_mock(["daemon", "status", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(status_stderr.is_empty());
        assert!(status_help.contains("--auto-start"));
        assert!(status_help.contains("systemctl --user start verbatim"));
        assert!(status_help.contains("not systemd socket activation"));

        let (code, install_help, install_stderr, _, _) = run_mock(["daemon", "install", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(install_stderr.is_empty());
        assert!(install_help.contains("Idle exit"));
        assert!(install_help.contains("systemctl --user start verbatim"));
        assert!(install_help.contains("no socket unit is installed"));
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
        assert!(COLLECTION_CLI_API_PARITY.iter().all(|entry| {
            entry
                .endpoint
                .path_template()
                .starts_with("/api/collections")
        }));
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
        let normalized_task_help = task_help.replace('\n', " ");
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(task_help.contains("Task ids are returned by --background"));
        assert!(task_help.contains("read stored completed-task diagnostics by id"));
        assert!(
            task_help.contains("Read persisted task profile diagnostics without rerunning work")
        );
        assert!(normalized_task_help.contains("without rerunning the original task work"));
        assert!(task_help.contains("verbatim task wait --timeout 25m"));
        assert!(task_help.contains("verbatim task profile"));
        assert!(task_help.contains("verbatim task resume"));
    }

    #[test]
    fn task_profile_help_documents_json_and_no_rerun_contract() {
        let (code, help, stderr, _, _) = run_mock(["task", "profile", "--help"]);
        let normalized_help = help.replace('\n', " ");

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(help.contains("verbatim task profile <task-id> --format json"));
        assert!(help.contains("--format"));
        assert!(help.contains("reads a persisted task profile by task id"));
        assert!(help.contains("side-effect-free"));
        assert!(help.contains("completed tasks with stored profiles"));
        assert!(help.contains("legacy/no-profile"));
        assert!(help.contains("incomplete tasks return unavailable errors"));
        assert!(help.contains("does not rerun the original task"));
        assert!(help.contains("does not rerun"));
        assert!(help.contains("retrieval"));
        assert!(help.contains("embedding"));
        assert!(normalized_help.contains("BM25/dense search"));
        assert!(help.contains("rerank"));
        assert!(help.contains("chat/generation"));
        assert!(help.contains("citation verifier"));
        assert!(help.contains("evidence expansion"));
        assert!(help.contains("compact human-readable text"));
        assert!(help.contains("--format json"));
        assert!(normalized_help.contains("machine-readable tooling output"));
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
        assert!(help.contains("Default markdown is compact"));
        assert!(help.contains("Scores are ranked chunk-level scores"));
        assert!(help.contains("chunk-internal support unit"));
        assert!(help.contains("snippets/text-only omit headers"));
        assert!(help.contains("compact JSON retrieval diagnostic summary with local"));
        assert!(help.contains("--show-debug --verbose writes the full task diagnostics"));
        assert!(help.contains("local stage spans"));
        assert!(help.contains("dense/BM25/RRF/rerank"));
        assert!(help.contains("--show-locator"));
        assert!(help.contains("structured locator/provenance"));
        assert!(help.contains("verbatim retrieve --format snippets"));
        assert!(help.contains("verbatim retrieve --format tsv"));
        assert!(help.contains("verbatim retrieve --format csv"));
        assert!(help.contains("--text-only"));
        assert!(!help.contains("--quiet"));
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
        assert!(ask_help.contains("--limit"));
        assert!(ask_help.contains("--page-size"));
        assert!(ask_help.contains("--page"));
        assert!(ask_help.contains("only apply with --context-only or"));
        assert!(ask_help.contains("--format"));

        let (code, retrieve_help, stderr, _, _) = run_mock(["retrieve", "--help"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(retrieve_help.contains("without invoking chat generation"));
        assert!(retrieve_help.contains("markdown"));
        assert!(retrieve_help.contains("json"));
        assert!(retrieve_help.contains("snippets"));
        assert!(retrieve_help.contains("tsv"));
        assert!(retrieve_help.contains("csv"));
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
    fn task_list_help_documents_plateau_metadata_terms() {
        let (code, help, stderr, _, _) = run_mock(["task", "list", "--help"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(help.contains("active total"));
        assert!(help.contains("turnover/backfill"));
        assert!(help.contains("waiting reason"));
        assert!(help.contains("stale running"));
    }

    #[test]
    fn vector_json_cleanup_help_documents_safety_contract() {
        let (code, help, stderr, _, _) = run_mock(["index", "vector-json-cleanup", "--help"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(help.contains("--dry-run"));
        assert!(help.contains("--execute"));
        assert!(help.contains("--confirm"));
        assert!(help.contains("back up"));
        assert!(help.contains("eligible, json_only, missing_blob, and malformed_blob"));
        assert!(help.contains("Legacy JSON-only rows remain readable"));
        assert!(help.contains("never deleted or cleared"));
        assert!(help.contains("VACUUM or rebuild-table"));
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
    fn source_check_reports_profile_status_diagnostics() {
        let (code, stdout, stderr, client, _) = run_mock(["source", "check"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["check_sources"]);
        assert!(stdout.contains("Stale sources:"));
        assert!(stdout.contains("src-1"));
        assert!(stdout.contains("Embedding profile:"));
        assert!(stdout.contains("served_model: text-embedding-3-small@2026-06"));
        assert!(stdout.contains("chunking:"));
        assert!(stdout.contains("embedding_input_budget_tokens: 7168"));
        assert!(stdout
            .contains("context window grew from 4096 to 8192; reindex is optional for quality"));
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
    fn index_status_calls_daemon_and_reports_capability_fingerprint() {
        let (code, stdout, stderr, client, _) = run_mock(["index", "status"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["index_status"]);
        assert!(stdout.contains("Index status:"));
        assert!(stdout.contains("active_profile_id: openai:text-embedding-3-small"));
        assert!(stdout.contains("served_model: text-embedding-3-small@2026-06"));
        assert!(stdout.contains("quantization: fp16"));
        assert!(stdout.contains("embedding_input_budget_tokens: 7168"));
        assert!(stdout
            .contains("context window grew from 4096 to 8192; reindex is optional for quality"));
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
    fn index_vector_json_cleanup_dry_run_calls_daemon_and_reports_counts() {
        let (code, stdout, stderr, client, _) =
            run_mock(["index", "vector-json-cleanup", "--dry-run"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["vector_json_cleanup:true:false"]
        );
        assert!(stdout.contains("Vector JSON cleanup dry-run"));
        assert!(stdout
            .contains("chunk_vectors: eligible=2 json_only=3 missing_blob=4 malformed_blob=5"));
        assert!(stdout
            .contains("embedding_cache: eligible=6 json_only=7 missing_blob=8 malformed_blob=9"));
        assert!(stdout.contains("No SQLite rows were modified."));
        assert!(stdout.contains("VACUUM or rebuild-table"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn index_vector_json_cleanup_execute_requires_confirm_and_reports_cleared() {
        let (code, _, stderr, client, _) = run_mock(["index", "vector-json-cleanup", "--execute"]);

        assert!(code.is_err());
        assert!(stderr.contains("required"));
        assert!(client.calls.borrow().is_empty());

        let (code, stdout, stderr, client, _) =
            run_mock(["index", "vector-json-cleanup", "--execute", "--confirm"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["vector_json_cleanup:false:true"]
        );
        assert!(stdout.contains("Vector JSON cleanup complete"));
        assert!(stdout.contains("cleared=2"));
        assert!(stdout.contains("cleared=6"));
        assert!(stdout.contains("JSON-only and malformed-BLOB rows were skipped"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn index_delete_profile_dry_run_calls_daemon_and_reports_plan() {
        let (code, stdout, stderr, client, _) =
            run_mock(["index", "delete-profile", "old-profile", "--dry-run"]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["index_delete_profile:old-profile:true:false:false"]
        );
        assert_eq!(
            client.last_index_profile_delete.borrow().as_ref().unwrap(),
            &IndexProfileDeleteRequest {
                profile_id: "old-profile".into(),
                dry_run: true,
                confirm: false,
                allow_active: false,
            }
        );
        assert!(stdout.contains("Index profile delete dry-run"));
        assert!(stdout.contains("profile: old-profile"));
        assert!(stdout.contains("planned sqlite rows: chunk_vectors=2 embedding_cache=1"));
        assert!(stdout.contains("artifact would remove"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn index_delete_profile_apply_requires_confirm_flag_payload() {
        let (code, stdout, stderr, client, _) = run_mock([
            "index",
            "delete-profile",
            "old-profile",
            "--confirm",
            "--allow-active",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["index_delete_profile:old-profile:false:true:true"]
        );
        assert_eq!(
            client.last_index_profile_delete.borrow().as_ref().unwrap(),
            &IndexProfileDeleteRequest {
                profile_id: "old-profile".into(),
                dry_run: false,
                confirm: true,
                allow_active: true,
            }
        );
        assert!(stdout.contains("Index profile delete complete"));
        assert!(stdout.contains("removed sqlite rows: chunk_vectors=2 embedding_cache=1"));
        assert!(stdout.contains("Removed artifact directories:"));
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
        assert!(stdout.contains("Collection root:"));
        assert!(stdout.contains("action: added"));
        assert!(stdout.contains("collection: articles"));
        assert!(stdout.contains("path: /tmp/articles"));
        assert!(stdout.contains("kind: directory"));
        assert!(stdout.contains("roots: 1"));
        assert!(stdout.contains("members: 1"));
        assert!(!stdout.contains("Members:"));
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
    fn collection_add_root_existing_output_is_bounded_for_large_membership() {
        let client = MockDaemonClient::default();
        client
            .collection_root_response
            .replace(Some(AddCollectionRootResponse {
                collection_name: "articles".into(),
                root: sample_collection_root(),
                root_count: 1,
                member_count: 2_250,
                added: false,
            }));
        let local = MockLocalActions::default();

        let (code, stdout, stderr) = run_mock_with(
            ["collection", "add-root", "articles", "/tmp/articles"],
            &client,
            &local,
        );

        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["add_collection_root:articles"]
        );
        assert!(stdout.contains("Collection root:"));
        assert!(stdout.contains("action: already_present"));
        assert!(stdout.contains("collection: articles"));
        assert!(stdout.contains("path: /tmp/articles"));
        assert!(stdout.contains("kind: directory"));
        assert!(stdout.contains("roots: 1"));
        assert!(stdout.contains("members: 2250"));
        assert!(!stdout.contains("Members:"));
        assert!(!stdout.contains("logical="));
        assert!(!stdout.contains("/tmp/articles/member-2249.md"));
        assert!(stdout.len() < 512, "stdout was {} bytes", stdout.len());
        assert!(
            stdout.lines().count() <= 8,
            "stdout was {} lines:\n{stdout}",
            stdout.lines().count()
        );
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
        assert!(stdout.contains("daemon.bind="));

        let (code, stdout, _, client, _) = run_mock(["daemon", "status"]);
        assert_eq!(code.unwrap(), 0);
        assert_eq!(client.calls.borrow().as_slice(), ["health"]);
        assert!(stdout.contains("ok"));
        assert!(!stdout.contains("Idle reclaim:"));
    }

    #[test]
    fn daemon_status_displays_idle_reclaim_health_when_present() {
        let client = MockDaemonClient::default();
        client.health_response.replace(Some(HealthResponse {
            status: "ok".into(),
            readiness: ReadinessHealth::ready(),
            memory_budget: Default::default(),
            resources: Vec::new(),
            idle_reclaim: Some(IdleReclaimHealth {
                enabled: true,
                sqlite_shrink_memory: true,
                malloc_trim: true,
                currently_idle: true,
                eligible: false,
                skip_reason: Some("min_interval_not_reached".into()),
                idle_for_millis: 12_000,
                idle_timeout_millis: 10_000,
                min_interval_millis: 60_000,
                next_eligible_in_millis: Some(48_000),
                active: IdleReclaimActivitySnapshot::default(),
                last_result: Some(IdleReclaimCycleResult {
                    attempted_at_unix_ms: 100,
                    finished_at_unix_ms: 110,
                    status: "succeeded".into(),
                    skip_reason: None,
                    sqlite: IdleReclaimBackendResult {
                        status: "succeeded".into(),
                        attempted: true,
                        success_count: 2,
                        failure_count: 0,
                        last_error: None,
                    },
                    allocator: IdleReclaimBackendResult {
                        status: "succeeded_no_release".into(),
                        attempted: true,
                        success_count: 1,
                        failure_count: 0,
                        last_error: None,
                    },
                }),
                last_attempt_result: None,
            }),
            idle_exit: None,
            sqlite_durability: None,
        }));
        let local = MockLocalActions::default();

        let (code, stdout, stderr) =
            run_mock_with(["daemon", "status", "--details"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("Idle reclaim: enabled=true idle=true eligible=false"));
        assert!(stdout.contains("skip=min_interval_not_reached"));
        assert!(stdout.contains("last: status=succeeded attempted_at_unix_ms=100"));
        assert!(stdout.contains("sqlite: status=succeeded attempted=true success=2 failure=0"));
        assert!(stdout.contains("allocator: status=succeeded_no_release attempted=true success=1"));
    }

    #[test]
    fn daemon_status_returns_nonzero_while_retrieval_is_starting() {
        let client = MockDaemonClient::default();
        client.health_response.replace(Some(HealthResponse {
            status: "ok".into(),
            readiness: ReadinessHealth::starting(
                "orphan_recovery",
                Some("recovering previous running ingest tasks".into()),
            ),
            memory_budget: Default::default(),
            resources: Vec::new(),
            idle_reclaim: None,
            idle_exit: None,
            sqlite_durability: None,
        }));
        let local = MockLocalActions::default();

        let (code, stdout, stderr) =
            run_mock_with(["daemon", "status", "--details"], &client, &local);

        assert_eq!(code.unwrap(), 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("Readiness: starting"));
        assert!(stdout.contains("process_alive=true"));
        assert!(stdout.contains("retrieval_ready=false"));
        assert!(stdout.contains("startup_phase=orphan_recovery"));
        assert!(stdout.contains("degraded_reason=recovering previous running ingest tasks"));
    }

    #[test]
    fn retrieve_starting_error_is_clear_and_not_unreachable() {
        let client = MockDaemonClient {
            retrieve_error: Some(CliError::Api(
                "verbatim daemon is starting; retrieval is not ready \
                 (startup_phase=orphan_recovery; degraded_reason=recovering previous running ingest tasks)"
                    .into(),
            )),
            ..MockDaemonClient::default()
        };
        let local = MockLocalActions::default();

        let (code, stdout, stderr) = run_mock_with(["retrieve", "question"], &client, &local);

        assert_eq!(code.unwrap_err(), 1);
        assert!(stdout.is_empty());
        assert!(stderr.contains("verbatim daemon is starting"));
        assert!(stderr.contains("retrieval is not ready"));
        assert!(stderr.contains("startup_phase=orphan_recovery"));
        assert!(!stderr.contains("could not reach"));
    }

    #[test]
    fn daemon_status_displays_last_reclaim_attempt_after_skipped_decision() {
        let client = MockDaemonClient::default();
        client.health_response.replace(Some(HealthResponse {
            status: "ok".into(),
            readiness: ReadinessHealth::ready(),
            memory_budget: Default::default(),
            resources: Vec::new(),
            idle_reclaim: Some(IdleReclaimHealth {
                enabled: true,
                sqlite_shrink_memory: true,
                malloc_trim: true,
                currently_idle: true,
                eligible: false,
                skip_reason: Some("min_interval_not_reached".into()),
                idle_for_millis: 12_000,
                idle_timeout_millis: 10_000,
                min_interval_millis: 60_000,
                next_eligible_in_millis: Some(48_000),
                active: IdleReclaimActivitySnapshot::default(),
                last_result: Some(IdleReclaimCycleResult {
                    attempted_at_unix_ms: 200,
                    finished_at_unix_ms: 200,
                    status: "skipped".into(),
                    skip_reason: Some("min_interval_not_reached".into()),
                    sqlite: IdleReclaimBackendResult::skipped(),
                    allocator: IdleReclaimBackendResult::skipped(),
                }),
                last_attempt_result: Some(IdleReclaimCycleResult {
                    attempted_at_unix_ms: 100,
                    finished_at_unix_ms: 110,
                    status: "succeeded".into(),
                    skip_reason: None,
                    sqlite: IdleReclaimBackendResult {
                        status: "succeeded".into(),
                        attempted: true,
                        success_count: 2,
                        failure_count: 0,
                        last_error: None,
                    },
                    allocator: IdleReclaimBackendResult {
                        status: "succeeded_no_release".into(),
                        attempted: true,
                        success_count: 1,
                        failure_count: 0,
                        last_error: None,
                    },
                }),
            }),
            idle_exit: None,
            sqlite_durability: None,
        }));
        let local = MockLocalActions::default();

        let (code, stdout, stderr) =
            run_mock_with(["daemon", "status", "--details"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("last: status=skipped attempted_at_unix_ms=200"));
        assert!(stdout.contains("last attempt: status=succeeded attempted_at_unix_ms=100"));
        assert!(stdout.contains("sqlite: status=succeeded attempted=true success=2 failure=0"));
        assert!(stdout.contains("allocator: status=succeeded_no_release attempted=true success=1"));
    }

    #[test]
    fn daemon_status_renders_idle_exit() {
        let client = MockDaemonClient::default();
        client.health_response.replace(Some(HealthResponse {
            status: "ok".into(),
            readiness: ReadinessHealth::ready(),
            memory_budget: Default::default(),
            resources: Vec::new(),
            idle_reclaim: None,
            idle_exit: Some(IdleExitHealth {
                enabled: true,
                count_health_requests: false,
                allow_with_collection_watcher: false,
                auto_start_on_cli: true,
                currently_idle: false,
                eligible: false,
                skip_reason: Some("active_collection_watchers".into()),
                idle_for_millis: 12_000,
                timeout_millis: 10_000,
                last_activity_unix_ms: 90,
                deadline_unix_ms: 10_090,
                next_eligible_in_millis: Some(10_000),
                active: IdleExitActivitySnapshot {
                    watched_roots: 2,
                    pending_watcher_events: 1,
                    ..IdleExitActivitySnapshot::default()
                },
            }),
            sqlite_durability: None,
        }));
        let local = MockLocalActions::default();

        let (code, stdout, stderr) =
            run_mock_with(["daemon", "status", "--details"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("Idle exit: enabled=true idle=false eligible=false"));
        assert!(stdout.contains("deadline_unix_ms=10090"));
        assert!(stdout.contains("skip=active_collection_watchers"));
        assert!(stdout.contains("watched_roots=2 pending_watcher_events=1"));
        assert!(stdout.contains(
            "config: count_health_requests=false allow_with_collection_watcher=false auto_start_on_cli=true"
        ));
    }

    #[test]
    fn daemon_auto_start_retry() {
        let client = MockDaemonClient::default();
        client
            .health_errors
            .borrow_mut()
            .push(CliError::DaemonUnreachable(
                "original connection refused".into(),
            ));
        let local = MockLocalActions::default();

        let (code, stdout, stderr) =
            run_mock_with(["daemon", "status", "--auto-start"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stdout.contains("ok"));
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["health"]);
        assert_eq!(
            local.calls.borrow().as_slice(),
            ["daemon_start_user_service"]
        );

        let client = MockDaemonClient {
            health_error: Some(CliError::DaemonUnreachable("daemon stopped".into())),
            ..MockDaemonClient::default()
        };
        let local = MockLocalActions::default();
        let (code, stdout, stderr) = run_mock_with(["daemon", "status"], &client, &local);

        assert_eq!(code.unwrap_err(), 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("daemon stopped"));
        assert_eq!(
            local.calls.borrow().as_slice(),
            ["daemon_idle_exit_auto_start_on_cli"]
        );

        let client = MockDaemonClient::default();
        client
            .health_errors
            .borrow_mut()
            .push(CliError::DaemonUnreachable("daemon idle exited".into()));
        let local = MockLocalActions::default();
        local.idle_exit_auto_start_on_cli.replace(true);
        let (code, stdout, stderr) = run_mock_with(["daemon", "status"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stdout.contains("ok"));
        assert!(stderr.is_empty());
        assert_eq!(
            local.calls.borrow().as_slice(),
            [
                "daemon_idle_exit_auto_start_on_cli",
                "daemon_start_user_service"
            ]
        );

        let client = MockDaemonClient::default();
        client
            .health_errors
            .borrow_mut()
            .push(CliError::DaemonUnreachable("connection failed".into()));
        let local = MockLocalActions::default();
        local
            .daemon_user_service_error
            .replace(Some(CliError::Api("systemd unavailable".into())));
        let (code, stdout, stderr) =
            run_mock_with(["daemon", "status", "--auto-start"], &client, &local);

        assert_eq!(code.unwrap_err(), 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("connection failed"));
        assert!(stderr.contains("systemd unavailable"));
        assert!(stderr.contains("systemctl --user start verbatim"));
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
                limit: None,
                page_size: None,
                page: None,
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
        assert!(stdout.contains("estimated_logical_write_rows"));
        assert!(stdout.contains("source_ingest_commit"));

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
    fn task_profile_json_calls_daemon_and_writes_machine_readable_profile() {
        let (code, stdout, stderr, client, _) =
            run_mock(["task", "profile", "task-1", "--format", "json"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["get_task_profile:task-1"]
        );
        let parsed: Value = serde_json::from_str(&stdout).expect("profile stdout is JSON");
        assert_eq!(parsed["task_id"], "task-1");
        assert_eq!(parsed["task_kind"], "retrieve");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(
            parsed["schema_version"],
            verbatim_core::task::TASK_PROFILE_SCHEMA_VERSION
        );
        assert!(parsed["queue_wait_ms"].is_u64());
        assert!(parsed["total_wall_ms"].is_u64());
        assert_eq!(parsed["retrieve"]["dense"]["path"], "bm25_only");
        assert_eq!(parsed["retrieve"]["bm25"]["candidate_count"], 1);
        assert!(parsed["retrieve"]["rerank"]["input_count"].is_null());
    }

    #[test]
    fn task_profile_human_retrieve_output_is_compact_and_grouped() {
        let (code, stdout, stderr, client, _) = run_mock(["task", "profile", "task-1"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["get_task_profile:task-1"]
        );
        assert!(serde_json::from_str::<Value>(&stdout).is_err());
        assert!(stdout.contains("Task profile: task-1"));
        assert!(stdout.contains("Timing:"));
        assert!(stdout.contains("Controls:"));
        assert!(stdout.contains("Model endpoints:"));
        assert!(stdout.contains("Retrieval:"));
        assert!(stdout.contains("Rerank:"));
        assert!(stdout.contains("Evidence:"));
        assert!(stdout.contains("Display/output:"));
        assert!(stdout.contains("Resource queues:"));
        assert!(stdout.contains("kind: retrieve"));
        assert!(stdout.contains("dense: path=bm25_only"));
        assert!(stdout.contains("bm25: candidates=1 local=3ms"));
        assert!(stdout.contains("final=1"));
        assert!(!stdout.contains("question"));
        assert!(!stdout.contains("bm25_hits"));
        assert!(!stdout.contains("final_evidence_pack"));
    }

    #[test]
    fn task_profile_human_ask_output_includes_ask_sections_and_retrieval_summary() {
        let client = MockDaemonClient::default();
        client
            .task_profile_response
            .replace(Some(TaskProfileResponse {
                profile: sample_ask_task_profile("task-ask"),
            }));
        let local = MockLocalActions::default();

        let (code, stdout, stderr) =
            run_mock_with(["task", "profile", "task-ask"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(
            client.calls.borrow().as_slice(),
            ["get_task_profile:task-ask"]
        );
        assert!(stdout.contains("Task profile: task-ask"));
        assert!(stdout.contains("kind: ask"));
        assert!(stdout.contains("Retrieval:"));
        assert!(stdout.contains("Ask generation:"));
        assert!(stdout.contains("Ask verification:"));
        assert!(stdout.contains("Ask output:"));
        assert!(stdout.contains("status=succeeded"));
        assert!(stdout.contains("status=passed"));
        assert!(stdout.contains("retrieval_included=true"));
        assert!(!stdout.contains("prompt"));
        assert!(!stdout.contains("raw"));
    }

    #[test]
    fn task_profile_errors_go_to_stderr_with_nonzero_exit_and_empty_stdout() {
        for (task_id, message) in [
            ("missing", "task not found: missing"),
            (
                "queued",
                "task profile unavailable for incomplete task queued (status queued)",
            ),
            ("ingest", "task profile unsupported for ingest task: ingest"),
            (
                "legacy",
                "task profile unavailable for legacy/no-profile task: legacy",
            ),
            (
                "corrupt",
                "stored task profile JSON is malformed for task: corrupt",
            ),
        ] {
            let client = MockDaemonClient {
                task_profile_error: Some(CliError::Api(message.into())),
                ..MockDaemonClient::default()
            };
            let local = MockLocalActions::default();

            let (code, stdout, stderr) =
                run_mock_with(["task", "profile", task_id], &client, &local);

            assert_eq!(code.unwrap_err(), 1, "{task_id}");
            assert!(stdout.is_empty(), "{task_id}");
            assert!(stderr.contains(message), "{task_id}: {stderr}");
        }
    }

    #[test]
    fn task_profile_missing_task_id_is_clap_error_to_stderr() {
        let (code, stdout, stderr, client, _) = run_mock(["task", "profile"]);

        assert_eq!(code.unwrap_err(), 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("required"));
        assert!(stderr.contains("TASK_ID"));
        assert!(client.calls.borrow().is_empty());
    }

    #[test]
    fn task_list_defaults_to_aggregate_queue_progress() {
        let (code, stdout, stderr, client, _) = run_mock(["task", "list"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["list_tasks:active"]);
        assert!(stdout.contains("Task queue:"));
        assert!(!stdout.contains("Ingest queue:"));
        assert!(stdout.contains("0/4"));
        assert!(stdout.contains("0.0%"));
        assert!(stdout.contains("ETA --"));
        assert!(stdout.contains("Use `verbatim task list --details`"));
        assert!(!stdout.contains("task-run"));
        assert!(!stdout.contains("task-queued"));
    }

    #[test]
    fn task_list_details_renders_active_tasks_with_progress_bars() {
        let (code, stdout, stderr, client, _) = run_mock(["task", "list", "--details"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["list_tasks:active"]);
        assert!(stdout.contains("Task queue:"));
        assert!(stdout.contains("Active tasks:"));
        assert!(stdout.contains("task-run"));
        assert!(stdout.contains("running"));
        assert!(stdout.contains("[##########----------]  50%"));
        assert!(stdout.contains("embedding elapsed=0ms"));
        assert!(stdout.contains("embedding_vectors 4/8"));
        assert!(stdout.contains("task-queued"));
        assert!(stdout.contains("queue #12"));
        assert!(stdout.contains("waiting for 11 queued ingest task(s) ahead"));
        assert!(stdout.contains("task-unknown"));
        assert!(stdout.contains("[????????????????????]   --"));
        assert!(stdout.contains("tokens 42"));
        assert!(stdout.contains("task-done-counter"));
        assert!(stdout.contains("[####################] 100% (still running)"));
        assert!(stdout.contains("embedding complete"));
    }

    #[test]
    fn task_list_aggregate_eta_uses_previous_sample_history() {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();

        *local.now_millis.borrow_mut() = 1_000;
        let mut first = sample_task_list_response();
        first.total = 10;
        client.task_list_response.replace(Some(first));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("0/10"));
        assert!(stdout.contains("ETA --"));

        *local.now_millis.borrow_mut() = 301_000;
        let mut second = sample_task_list_response();
        second.total = 5;
        client.task_list_response.replace(Some(second));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);
        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("5/10"));
        assert!(stdout.contains("50.0%"));
        assert!(stdout.contains("ETA 5m"));

        let history = local.task_list_history.borrow().clone().unwrap();
        assert_eq!(history.baseline_total, 10);
        assert_eq!(history.previous_total, 5);
        assert_eq!(history.sampled_at_ms, 301_000);
        assert!(history.sampled_task_ids.contains(&"task-run".into()));
    }

    #[test]
    fn task_list_continues_history_when_total_decreases_beyond_sample_window() {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 301_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 842,
                previous_total: 842,
                sampled_at_ms: 1_000,
                sampled_task_ids: (0..32).map(|index| format!("old-task-{index}")).collect(),
                last_event_sequence: 0,
            }));
        let mut current_queue = sample_task_list_response();
        current_queue.total = 650;
        for (index, task) in current_queue.tasks.iter_mut().enumerate() {
            task.id = TaskId(format!("new-task-{index}"));
        }
        client.task_list_response.replace(Some(current_queue));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("192/842"));
        assert!(stdout.contains("22.8%"));
        assert!(stdout.contains("ETA 17m"));
        let history = local.task_list_history.borrow().clone().unwrap();
        assert_eq!(history.baseline_total, 842);
        assert_eq!(history.previous_total, 650);
        assert_eq!(history.sampled_at_ms, 301_000);
    }

    #[test]
    fn task_list_discards_eta_history_but_renders_daemon_turnover_when_ids_do_not_overlap() {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 301_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 10,
                previous_total: 5,
                sampled_at_ms: 1_000,
                sampled_task_ids: vec!["task-old".into()],
                last_event_sequence: 42,
            }));
        let mut current_queue = sample_task_list_response();
        current_queue.total = 5;
        current_queue.aggregate = Some(sample_task_list_aggregate(1, 1, 0, 0, 0));
        client.task_list_response.replace(Some(current_queue));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("0/5"));
        assert!(stdout.contains("ETA --"));
        let history = local.task_list_history.borrow().clone().unwrap();
        assert_eq!(history.baseline_total, 5);
        assert_eq!(history.previous_total, 5);
        assert_eq!(history.sampled_at_ms, 301_000);
        assert!(history.sampled_task_ids.contains(&"task-run".into()));
        assert!(
            stdout.contains("active total unchanged; recent turnover terminalized=1 backfilled=1")
        );
    }

    #[test]
    fn task_list_aggregate_history_resets_after_completion() {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 10,
                previous_total: 1,
                sampled_at_ms: 301_000,
                sampled_task_ids: vec!["task-run".into()],
                last_event_sequence: 0,
            }));
        client.task_list_response.replace(Some(TaskListResponse {
            total: 0,
            tasks: Vec::new(),
            aggregate: None,
        }));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(stdout, "No active tasks.\n");
        assert!(local.task_list_history.borrow().is_none());
    }

    #[test]
    fn task_list_plateau_eta_rendering() {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 301_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 10,
                previous_total: 4,
                sampled_at_ms: 1_000,
                sampled_task_ids: vec!["task-run".into()],
                last_event_sequence: 0,
            }));
        let mut response = sample_task_list_response();
        response.aggregate = Some(sample_task_list_aggregate(3, 3, 2, 125_000, 1));
        client.task_list_response.replace(Some(response));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(
            stdout.contains("active total unchanged; recent turnover terminalized=3 backfilled=3")
        );
        assert!(stdout.contains("embedding wait: 2 sampled active task(s), oldest 3m"));
        assert!(stdout.contains("reasons embedding_batch=1, embedding_throughput=1"));
        assert!(stdout.contains("stale running: 1 publish-complete task(s)"));
        assert!(stdout.contains("post_publish_cleanup=1"));

        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 301_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 4,
                previous_total: 4,
                sampled_at_ms: 1_000,
                sampled_task_ids: vec!["task-run".into()],
                last_event_sequence: 0,
            }));
        let mut response = sample_task_list_response();
        response.aggregate = Some(sample_task_list_aggregate(0, 0, 0, 0, 0));
        client.task_list_response.replace(Some(response));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout
            .contains("active total unchanged; no recent completions in last 1000 task events"));

        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 301_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 4,
                previous_total: 4,
                sampled_at_ms: 1_000,
                sampled_task_ids: vec!["task-run".into()],
                last_event_sequence: 0,
            }));
        client
            .task_list_response
            .replace(Some(sample_task_list_response()));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("Task queue:"));
        assert!(!stdout.contains("recent turnover"));
        assert!(!stdout.contains("embedding wait:"));
    }

    #[test]
    fn task_list_eta_stays_dash_on_backfill_plateau_without_monotonic_terminalized() {
        // Scenario: baseline had 10 tasks, previous sample had 8 active,
        // 2 completed and the watcher backfilled 2 more so the current
        // active total is still 8.  Without a daemon-side monotonic
        // terminalized counter, ETA cannot be reliably computed and stays "--".
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 61_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 10,
                previous_total: 8,
                sampled_at_ms: 1_000,
                sampled_task_ids: vec!["task-run".into()],
                last_event_sequence: 100,
            }));

        let mut response = sample_task_list_response();
        response.total = 8;
        response.tasks.truncate(4);
        response.aggregate = Some(sample_task_list_aggregate_with_event_sequence(
            2,   // terminalized
            2,   // backfilled
            0,   // embedding_waiting
            0,   // oldest_embedding_wait_ms
            0,   // publish_complete_running
            200, // event_sequence_ceiling
        ));
        client.task_list_response.replace(Some(response));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(
            stdout.contains("ETA --"),
            "expected ETA -- for plateau without monotonic terminalized counter, got: {stdout}"
        );
    }

    #[test]
    fn task_list_eta_fallback_stays_dash_when_no_terminalized_events() {
        // Plateau with overlapping IDs, advanced event sequence, but
        // recent_terminalized=0 — no tasks completed.  ETA should stay "--".
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 61_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 10,
                previous_total: 8,
                sampled_at_ms: 1_000,
                sampled_task_ids: vec!["task-run".into()],
                last_event_sequence: 100,
            }));
        let mut response = sample_task_list_response();
        response.total = 8;
        response.tasks.truncate(4);
        response.aggregate = Some(sample_task_list_aggregate_with_event_sequence(
            0,   // terminalized — no completions
            0,   // backfilled
            0,   // embedding_waiting
            0,   // oldest_embedding_wait_ms
            0,   // publish_complete_running
            200, // event_sequence_ceiling advanced (progress chatter only)
        ));
        client.task_list_response.replace(Some(response));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(
            stdout.contains("ETA --"),
            "expected ETA -- when no terminalized events, got: {stdout}"
        );
    }

    #[test]
    fn plateau_queue_integration() {
        let local = MockLocalActions::default();
        *local.now_millis.borrow_mut() = 301_000;
        local
            .task_list_history
            .replace(Some(render::TaskListAggregateHistory {
                baseline_total: 4,
                previous_total: 4,
                sampled_at_ms: 1_000,
                sampled_task_ids: vec!["task-run".into()],
                last_event_sequence: 0,
            }));
        let mut daemon_response = sample_task_list_response();
        daemon_response.aggregate = Some(sample_task_list_aggregate(1, 1, 1, 65_000, 1));
        let server =
            TaskListHttpServer::respond_json(serde_json::to_string(&daemon_response).unwrap());
        let client = HttpDaemonClient::with_base_url(server.base_url());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with(["task", "list"], &mut stdout, &mut stderr, &client, &local);
        let stdout = String::from_utf8(stdout).unwrap();
        let stderr = String::from_utf8(stderr).unwrap();

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(server
            .request()
            .starts_with("GET /api/tasks?status=active&limit=20 HTTP/1.1"));
        assert!(
            stdout.contains("active total unchanged; recent turnover terminalized=1 backfilled=1")
        );
        assert!(stdout.contains("embedding wait: 1 sampled active task(s), oldest 2m"));
        assert!(stdout.contains("stale running: 1 publish-complete task(s)"));
    }

    #[test]
    fn task_list_ignores_optional_history_cache_load_and_store_errors() {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        local.task_list_history_load_error.replace(true);
        local.task_list_history_store_error.replace(true);

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("Task queue:"));
        assert_eq!(
            local.calls.borrow().as_slice(),
            ["load_task_list_history", "store_task_list_history"]
        );
    }

    #[test]
    fn task_list_ignores_optional_history_cache_clear_errors() {
        let client = MockDaemonClient::default();
        let local = MockLocalActions::default();
        local.task_list_history_clear_error.replace(true);
        client.task_list_response.replace(Some(TaskListResponse {
            total: 0,
            tasks: Vec::new(),
            aggregate: None,
        }));

        let (code, stdout, stderr) = run_mock_with(["task", "list"], &client, &local);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(stdout, "No active tasks.\n");
        assert_eq!(
            local.calls.borrow().as_slice(),
            ["load_task_list_history", "clear_task_list_history"]
        );
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
                limit: None,
                page_size: None,
                page: None,
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
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }
        );
        assert!(stdout.contains("1. score=0.0310 [doc.md L1]"));
        assert!(stdout.contains("   compact cited text"));
        assert!(!stdout.contains("Context pack:"));
        assert!(!stdout.contains("task-1"));
        assert!(!stdout.contains("/tmp/doc.md"));
        assert!(!stdout.contains("evidence="));
        assert!(!stdout.contains("role="));
        assert!(!stdout.contains("kind="));
        assert!(!stdout.contains("source_path:"));
        assert!(!stdout.contains("controls:"));
        assert!(!stdout.contains("timing:"));
    }

    #[test]
    fn retrieve_json_requests_structured_locator_and_writes_debug_to_stderr() {
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
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.rerank, Some(true));
        assert_eq!(request.dense_top_k, Some(5));
        assert_eq!(request.bm25_top_k, Some(7));
        assert_eq!(request.rerank_top_n, Some(2));
        assert!(request.include_debug);
        assert!(request.include_locator);
        assert!(stdout.contains("\"structured_locator\""));
        assert!(!stdout.contains("\"debug\""));
        let debug: serde_json::Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(debug["kind"], "retrieval_debug_summary");
        assert_eq!(debug["counts"]["bm25_hits"], 1);
        assert!(stderr.lines().count() < 50);
    }

    #[test]
    fn retrieve_no_cache_sets_bypass_cache_request_flag() {
        let (code, _, stderr, client, _) =
            run_mock(["retrieve", "--no-cache", "What", "is", "cited?"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.question, "What is cited?");
        assert!(request.bypass_cache);
    }

    #[test]
    fn retrieve_passage_sets_passage_request_flag() {
        let (code, _, stderr, client, _) = run_mock([
            "retrieve",
            "--passage",
            "--collection",
            "csb_bible",
            "crown",
            "of",
            "righteousness",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.question, "crown of righteousness");
        assert_eq!(
            request.collection_filter.names,
            vec!["csb_bible".to_string()]
        );
        assert!(request.passage);
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
        assert!(request.collection_filter.require_fresh);
    }

    #[test]
    fn collection_queries_require_fresh_by_default_and_can_allow_stale() {
        let (code, _, stderr, client, _) =
            run_mock(["ask", "--collection", "articles", "What", "changed?"]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        let request = client.last_ask.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(
            request.collection_filter.names,
            vec!["articles".to_string()]
        );
        assert!(request.collection_filter.require_fresh);

        let (code, _, stderr, client, _) = run_mock([
            "retrieve",
            "--collection",
            "articles",
            "--allow-stale",
            "What",
            "changed?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(
            request.collection_filter.names,
            vec!["articles".to_string()]
        );
        assert!(!request.collection_filter.require_fresh);
    }

    #[test]
    fn retrieve_markdown_show_debug_writes_compact_summary_to_stderr() {
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
        assert_eq!(client.calls.borrow().as_slice(), ["retrieve"]);
        assert!(
            client
                .last_retrieve
                .borrow()
                .as_ref()
                .unwrap()
                .include_debug
        );
        assert!(
            !client
                .last_retrieve
                .borrow()
                .as_ref()
                .unwrap()
                .include_debug_packs
        );
        assert!(
            client
                .last_retrieve
                .borrow()
                .as_ref()
                .unwrap()
                .include_locator,
            "show-debug should request structured locator metadata"
        );
        assert!(stdout.contains("1. score=0.0310 [doc.md L1]"));
        assert!(!stdout.contains("Context pack: task-1"));
        assert!(!stdout.contains("Retrieval Debug"));
        let debug: serde_json::Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(debug["kind"], "retrieval_debug_summary");
        assert_eq!(debug["timing_ms"]["retrieval_ms"], 7);
        assert_eq!(debug["local_spans_ms"]["dense_vector_search_ms"], 3);
        assert_eq!(debug["local_spans_ms"]["response_formatting_ms"], 12);
        assert_eq!(debug["counts"]["final_evidence"], 1);
        assert_eq!(debug["reranker"]["status"], "skipped");
        assert_eq!(
            debug["top_candidates"]["bm25_hits"][0]["chunk_id"],
            "chunk-1"
        );
        assert!(debug["top_candidates"]["bm25_hits"][0]
            .get("evidence_ids")
            .is_none());
        assert!(stderr.lines().count() < 50);
    }

    #[test]
    fn retrieve_markdown_show_debug_verbose_writes_full_debug_to_stderr() {
        let (code, stdout, stderr, client, _) = run_mock([
            "retrieve",
            "--show-debug",
            "--verbose",
            "--format",
            "markdown",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(
            client
                .last_retrieve
                .borrow()
                .as_ref()
                .unwrap()
                .include_debug_packs
        );
        assert!(stdout.contains("1. score=0.0310 [doc.md L1]"));
        assert!(!stdout.contains("Retrieval Debug"));
        assert!(stderr.contains("Context pack: task-1"));
        assert!(stderr.contains("Retrieval Debug"));
        assert!(stderr.contains("dense_vector_search_ms=3ms"));
        assert!(stderr.contains("Final evidence pack:"));
    }

    #[test]
    fn retrieve_snippets_and_text_only_render_same_low_noise_output() {
        let (code, snippets_stdout, snippets_stderr, snippets_client, _) =
            run_mock(["retrieve", "--format", "snippets", "What", "is", "cited?"]);
        assert_eq!(code.unwrap(), 0);
        assert!(snippets_stderr.is_empty());
        assert_eq!(snippets_client.calls.borrow().as_slice(), ["retrieve"]);

        let (code, text_stdout, text_stderr, text_client, _) =
            run_mock(["retrieve", "--text-only", "What", "is", "cited?"]);
        assert_eq!(code.unwrap(), 0);
        assert!(text_stderr.is_empty());
        assert_eq!(text_client.calls.borrow().as_slice(), ["retrieve"]);

        assert_eq!(text_stdout, snippets_stdout);
        assert_eq!(snippets_stdout, "[doc.md L1] compact cited text\n");
        assert!(!snippets_stdout.contains("Context pack:"));
        assert!(!snippets_stdout.contains("score="));
        assert!(!snippets_stdout.contains("/tmp/doc.md"));
        assert!(!snippets_stdout.contains("evidence="));
        assert!(!snippets_stdout.contains("role="));
        assert!(!snippets_stdout.contains("kind="));
    }

    #[test]
    fn ask_context_only_writes_retrieval_debug_summary_to_stderr() {
        let (code, stdout, stderr, client, _) = run_mock([
            "ask",
            "--context-only",
            "--show-retrieval",
            "--page-size",
            "1",
            "--page",
            "2",
            "--limit",
            "3",
            "-s",
            "src-1",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
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
                fast: false,
                rerank: None,
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: true,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }
        );
        assert!(stdout.contains("1. score=0.0310 [doc.md L1]"));
        assert!(!stdout.contains("Context pack: task-1"));
        assert!(!stdout.contains("Retrieval Debug"));
        let debug: serde_json::Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(debug["kind"], "retrieval_debug_summary");
        assert_eq!(debug["counts"]["rrf_fused"], 1);
    }

    #[test]
    fn ask_generation_rejects_context_pagination_controls() {
        let (code, stdout, stderr, client, _) =
            run_mock(["ask", "--page", "2", "What", "is", "cited?"]);

        assert_eq!(code.unwrap_err(), 1);
        assert!(stdout.is_empty());
        assert!(stderr.contains("--page"));
        assert!(stderr.contains("--context-only"));
        assert!(client.calls.borrow().is_empty());
        assert!(client.last_ask.borrow().is_none());
        assert!(client.last_retrieve.borrow().is_none());
    }

    #[test]
    fn ask_no_generate_json_requests_structured_context_pack() {
        let (code, stdout, stderr, client, _) = run_mock([
            "ask",
            "--no-generate",
            "--format",
            "json",
            "--limit",
            "4",
            "--page-size",
            "2",
            "--page",
            "3",
            "What",
            "is",
            "cited?",
        ]);

        assert_eq!(code.unwrap(), 0);
        assert!(stderr.is_empty());
        assert_eq!(client.calls.borrow().as_slice(), ["retrieve"]);
        let request = client.last_retrieve.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.limit, Some(4));
        assert_eq!(request.page_size, Some(2));
        assert_eq!(request.page, Some(3));
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
                limit: None,
                page_size: None,
                page: None,
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

    #[test]
    fn resolve_outputs_display_and_normalized() {
        let (code, stdout, stderr, _, _) = run_mock(["resolve", "John 3:16"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stdout.contains("display:   John 3:16"));
        assert!(stdout.contains("normalized: john:3:16"));
        assert!(stdout.contains("profile:   bible"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn resolve_json_output() {
        let (code, stdout, _, _, _) = run_mock(["resolve", "1 Cor 13:4-7", "--format", "json"]);
        assert_eq!(code.unwrap(), 0);
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(parsed["display"], "1 Corinthians 13:4-7");
        assert_eq!(parsed["normalized"], "1corinthians:13:4");
        assert_eq!(parsed["profile"], "bible");
    }

    #[test]
    fn resolve_cross_chapter_range() {
        let (code, stdout, _, _, _) = run_mock(["resolve", "Gen 1:31-2:3"]);
        assert_eq!(code.unwrap(), 0);
        assert!(stdout.contains("display:   Genesis 1:31-2:3"));
    }

    #[test]
    fn resolve_rejects_ambiguous_chapter_references() {
        for reference in ["John 3", "john 3", " John   3 ", "Jn 3", "John 3-5"] {
            let (code, stdout, stderr, _, _) = run_mock(["resolve", reference]);
            assert!(code.is_err(), "{reference}: {stdout}");
            assert!(stdout.is_empty(), "{reference}: {stdout}");
            assert!(
                stderr.contains("could not parse") && stderr.contains(reference),
                "{reference}: {stderr}"
            );
        }
    }

    #[test]
    fn resolve_rejects_invalid_reference() {
        let (code, stdout, _, _, _) = run_mock(["resolve", "not a reference"]);
        assert!(code.is_err());
        assert!(!stdout.contains("display:"));
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

    fn run_mock_with<I>(
        args: I,
        client: &MockDaemonClient,
        local: &MockLocalActions,
    ) -> (Result<u8, u8>, String, String)
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with(args, &mut stdout, &mut stderr, client, local);
        (
            code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    pub(super) fn generated_config_template() -> String {
        let _env_lock = super::CONFIG_INIT_ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::capture(&["VERBATIM_CONFIG"]);
        let tempdir = TempDirGuard::new("config-init-template");
        let config_path = tempdir.path().join("config.toml");
        env::set_var("VERBATIM_CONFIG", &config_path);

        let written_path = verbatim_core::config::init_default_config().unwrap();
        assert_eq!(written_path, config_path);

        fs::read_to_string(&written_path).unwrap()
    }

    pub(super) struct EnvGuard {
        values: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        pub(super) fn capture(names: &[&'static str]) -> Self {
            let values = names
                .iter()
                .map(|name| (*name, env::var_os(name)))
                .collect();
            Self { values }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.values {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }

    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new(name: &str) -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "verbatim-cli-{name}-{}-{suffix}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct TaskListHttpServer {
        addr: SocketAddr,
        request: Arc<Mutex<Option<String>>>,
        handle: thread::JoinHandle<()>,
    }

    impl TaskListHttpServer {
        fn respond_json(body: String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let request = Arc::new(Mutex::new(None));
            let thread_request = Arc::clone(&request);
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request_bytes = Vec::new();
                let mut buffer = [0u8; 512];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request_bytes.extend_from_slice(&buffer[..read]);
                    if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request_bytes).into_owned();
                thread_request.lock().unwrap().replace(request_text);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            });
            Self {
                addr,
                request,
                handle,
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn request(self) -> String {
            self.handle.join().unwrap();
            self.request.lock().unwrap().take().unwrap()
        }
    }

    #[derive(Default)]
    struct MockDaemonClient {
        calls: RefCell<Vec<String>>,
        last_ask: RefCell<Option<AskRequest>>,
        last_retrieve: RefCell<Option<RetrieveRequest>>,
        last_reindex: RefCell<Option<ReindexRequest>>,
        last_index_profile_delete: RefCell<Option<IndexProfileDeleteRequest>>,
        last_vector_json_cleanup: RefCell<Option<VectorJsonCleanupRequest>>,
        last_collection_create: RefCell<Option<CreateCollectionRequest>>,
        last_collection_root: RefCell<Option<AddCollectionRootRequest>>,
        collection_root_response: RefCell<Option<AddCollectionRootResponse>>,
        last_collection_sync: RefCell<Option<CollectionSyncRequest>>,
        last_watcher_update: RefCell<Option<CollectionWatcherUpdateRequest>>,
        list_error: Option<CliError>,
        retrieve_error: Option<CliError>,
        health_error: Option<CliError>,
        health_errors: RefCell<Vec<CliError>>,
        health_response: RefCell<Option<HealthResponse>>,
        task_list_response: RefCell<Option<TaskListResponse>>,
        task_profile_response: RefCell<Option<TaskProfileResponse>>,
        task_profile_error: Option<CliError>,
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

        fn relocate_source(&self, id: &str, new_path: &str) -> client::CliResult<SourceResponse> {
            self.calls
                .borrow_mut()
                .push(format!("relocate_source:{id}:{new_path}"));
            let mut source = sample_source();
            source.id = id.to_string();
            source.path = new_path.to_string();
            Ok(source)
        }

        fn check_sources(&self) -> client::CliResult<CheckStaleResponse> {
            self.calls.borrow_mut().push("check_sources".into());
            Ok(CheckStaleResponse {
                stale: vec!["src-1".into()],
                profile_status: Some(sample_index_status_response()),
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
        ) -> client::CliResult<AddCollectionRootResponse> {
            self.calls
                .borrow_mut()
                .push(format!("add_collection_root:{name}"));
            self.last_collection_root.replace(Some(request.clone()));
            Ok(self
                .collection_root_response
                .borrow()
                .clone()
                .unwrap_or_else(sample_add_collection_root_response))
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

        fn index_status(&self) -> client::CliResult<IndexStatusResponse> {
            self.calls.borrow_mut().push("index_status".into());
            Ok(sample_index_status_response())
        }

        fn index_gc(&self, request: &IndexGcRequest) -> client::CliResult<IndexGcResponse> {
            self.calls
                .borrow_mut()
                .push(format!("index_gc:{}", request.dry_run));
            Ok(sample_index_gc_response(request.dry_run))
        }

        fn index_delete_profile(
            &self,
            request: &IndexProfileDeleteRequest,
        ) -> client::CliResult<IndexProfileDeleteResponse> {
            self.calls.borrow_mut().push(format!(
                "index_delete_profile:{}:{}:{}:{}",
                request.profile_id, request.dry_run, request.confirm, request.allow_active
            ));
            self.last_index_profile_delete
                .replace(Some(request.clone()));
            Ok(sample_index_profile_delete_response(request.dry_run))
        }

        fn vector_json_cleanup(
            &self,
            request: &VectorJsonCleanupRequest,
        ) -> client::CliResult<VectorJsonCleanupResponse> {
            self.calls.borrow_mut().push(format!(
                "vector_json_cleanup:{}:{}",
                request.dry_run, request.confirm
            ));
            self.last_vector_json_cleanup.replace(Some(request.clone()));
            Ok(sample_vector_json_cleanup_response(request.dry_run))
        }

        fn list_tasks(&self) -> client::CliResult<TaskListResponse> {
            self.calls.borrow_mut().push("list_tasks:active".into());
            Ok(self
                .task_list_response
                .borrow()
                .clone()
                .unwrap_or_else(sample_task_list_response))
        }

        fn get_task(&self, task_id: &str) -> client::CliResult<TaskSummaryResponse> {
            self.calls.borrow_mut().push(format!("get_task:{task_id}"));
            Ok(sample_task_response(TaskStatus::Succeeded))
        }

        fn get_task_profile(&self, task_id: &str) -> client::CliResult<TaskProfileResponse> {
            if let Some(error) = &self.task_profile_error {
                return Err(clone_cli_error(error));
            }
            self.calls
                .borrow_mut()
                .push(format!("get_task_profile:{task_id}"));
            Ok(self
                .task_profile_response
                .borrow()
                .clone()
                .unwrap_or_else(|| TaskProfileResponse {
                    profile: sample_task_profile(task_id),
                }))
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
            if let Some(error) = &self.retrieve_error {
                return Err(clone_cli_error(error));
            }
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
            if !self.health_errors.borrow().is_empty() {
                return Err(self.health_errors.borrow_mut().remove(0));
            }
            if let Some(error) = &self.health_error {
                return Err(clone_cli_error(error));
            }
            self.calls.borrow_mut().push("health".into());
            Ok(self
                .health_response
                .borrow()
                .clone()
                .unwrap_or(HealthResponse {
                    status: "ok".into(),
                    readiness: ReadinessHealth::ready(),
                    memory_budget: Default::default(),
                    resources: Vec::new(),
                    idle_reclaim: None,
                    idle_exit: None,
                    sqlite_durability: None,
                }))
        }
    }

    #[derive(Default)]
    struct MockLocalActions {
        calls: RefCell<Vec<String>>,
        task_list_history: RefCell<Option<render::TaskListAggregateHistory>>,
        task_list_history_load_error: RefCell<bool>,
        task_list_history_store_error: RefCell<bool>,
        task_list_history_clear_error: RefCell<bool>,
        now_millis: RefCell<u64>,
        daemon_user_service_error: RefCell<Option<CliError>>,
        idle_exit_auto_start_on_cli: RefCell<bool>,
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

        fn daemon_start_user_service(&self) -> client::CliResult<()> {
            self.calls
                .borrow_mut()
                .push("daemon_start_user_service".into());
            if let Some(error) = self.daemon_user_service_error.borrow().as_ref() {
                return Err(clone_cli_error(error));
            }
            Ok(())
        }

        fn daemon_idle_exit_auto_start_on_cli(&self) -> client::CliResult<bool> {
            self.calls
                .borrow_mut()
                .push("daemon_idle_exit_auto_start_on_cli".into());
            Ok(*self.idle_exit_auto_start_on_cli.borrow())
        }

        fn daemon_install(&self, force: bool) -> client::CliResult<PathBuf> {
            self.calls
                .borrow_mut()
                .push(format!("daemon_install:{force}"));
            Ok(PathBuf::from("/tmp/verbatim.service"))
        }

        fn load_task_list_history(
            &self,
        ) -> client::CliResult<Option<render::TaskListAggregateHistory>> {
            self.calls
                .borrow_mut()
                .push("load_task_list_history".into());
            if *self.task_list_history_load_error.borrow() {
                return Err(CliError::Api("task list history load failed".into()));
            }
            Ok(self.task_list_history.borrow().clone())
        }

        fn store_task_list_history(
            &self,
            history: &render::TaskListAggregateHistory,
        ) -> client::CliResult<()> {
            self.calls
                .borrow_mut()
                .push("store_task_list_history".into());
            if *self.task_list_history_store_error.borrow() {
                return Err(CliError::Api("task list history store failed".into()));
            }
            self.task_list_history.replace(Some(history.clone()));
            Ok(())
        }

        fn clear_task_list_history(&self) -> client::CliResult<()> {
            self.calls
                .borrow_mut()
                .push("clear_task_list_history".into());
            if *self.task_list_history_clear_error.borrow() {
                return Err(CliError::Api("task list history clear failed".into()));
            }
            self.task_list_history.replace(None);
            Ok(())
        }

        fn now_millis(&self) -> u64 {
            *self.now_millis.borrow()
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

    fn sample_collection_root() -> CollectionRoot {
        CollectionRoot {
            collection_name: "articles".into(),
            path: PathBuf::from("/tmp/articles"),
            canonical_path: Some(PathBuf::from("/tmp/articles")),
            kind: CollectionRootKind::Directory,
            added_at: "1".into(),
            updated_at: "2".into(),
        }
    }

    fn sample_add_collection_root_response() -> AddCollectionRootResponse {
        AddCollectionRootResponse {
            collection_name: "articles".into(),
            root: sample_collection_root(),
            root_count: 1,
            member_count: 1,
            added: true,
        }
    }

    fn sample_collection_response() -> CollectionResponse {
        CollectionResponse {
            collection: sample_collection_record(),
            roots: vec![sample_collection_root()],
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

    fn sample_index_status_response() -> IndexStatusResponse {
        IndexStatusResponse {
            embedding_enabled: true,
            active_profile_id: "openai:text-embedding-3-small".into(),
            source_count: 4,
            stale_source_count: 1,
            stale_source_ids: vec!["src-1".into()],
            capability: EmbeddingCapabilityStatusResponse {
                provider: "openai-compatible".into(),
                model: "text-embedding-3-small".into(),
                dimension: 1536,
                normalize: true,
                endpoint_identity: Some("https://embeddings.local/v1".into()),
                requested_model: Some("text-embedding-3-small".into()),
                served_model: Some("text-embedding-3-small@2026-06".into()),
                max_context_tokens: Some(8192),
                dtype: Some("float16".into()),
                quantization: Some("fp16".into()),
                weight_identity: Some("sha256:weights".into()),
            },
            chunking: ChunkingProfileStatusResponse {
                version: "markdown-v1".into(),
                child_target_tokens: 512,
                child_overlap_tokens: 64,
                parent_children_count: 4,
                embedding_input_budget_tokens: Some(7168),
            },
            messages: vec![
                "context window grew from 4096 to 8192; reindex is optional for quality".into(),
            ],
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

    fn sample_index_profile_delete_response(dry_run: bool) -> IndexProfileDeleteResponse {
        let sqlite = verbatim_core::store::EmbeddingProfileStorageCounts {
            chunk_vectors: 2,
            embedding_cache_entries: 1,
            source_embedding_statuses: 3,
            embeddings_meta_entries: 2,
            embedding_profile_index_meta_entries: 1,
            embedding_profiles: 1,
        };
        let artifact = verbatim_core::index_profile_delete::IndexProfileArtifactPlan {
            path: PathBuf::from("/tmp/verbatim/indexes/profiles/old-profile"),
            approximate_bytes: 4096,
            reason: "profile-scoped published vector artifacts are obsolete".into(),
        };
        IndexProfileDeleteResponse {
            dry_run,
            plan: verbatim_core::index_profile_delete::IndexProfileDeletePlan {
                profile_id: "old-profile".into(),
                active_profile: false,
                sqlite,
                artifact: Some(artifact.clone()),
                skipped: Vec::new(),
                approximate_reclaim_bytes: artifact.approximate_bytes,
            },
            apply: if dry_run {
                verbatim_core::index_profile_delete::IndexProfileDeleteApplyReport::default()
            } else {
                verbatim_core::index_profile_delete::IndexProfileDeleteApplyReport {
                    sqlite,
                    removed_artifacts: vec![artifact.clone()],
                    reclaimed_bytes: artifact.approximate_bytes,
                }
            },
        }
    }

    fn sample_vector_json_cleanup_response(dry_run: bool) -> VectorJsonCleanupResponse {
        VectorJsonCleanupResponse {
            dry_run,
            report: verbatim_core::store::VectorJsonCleanupReport {
                tables: verbatim_core::store::VectorJsonCleanupTables {
                    chunk_vectors: verbatim_core::store::VectorJsonCleanupTableStats {
                        eligible: 2,
                        already_clean: 1,
                        json_only: 3,
                        missing_blob: 4,
                        malformed_blob: 5,
                    },
                    embedding_cache: verbatim_core::store::VectorJsonCleanupTableStats {
                        eligible: 6,
                        already_clean: 2,
                        json_only: 7,
                        missing_blob: 8,
                        malformed_blob: 9,
                    },
                },
                cleared: if dry_run {
                    verbatim_core::store::VectorJsonCleanupCleared::default()
                } else {
                    verbatim_core::store::VectorJsonCleanupCleared {
                        chunk_vectors: 2,
                        embedding_cache: 6,
                    }
                },
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
            source_hash: Some("persisted-source-hash".into()),
            source_bounded: true,
            text_hash: "receipt-text-hash".into(),
            kind: "text".into(),
            derived_from: None,
            locator: "PDF p.1 para.1".into(),
            structured_locator: SourceLocator::legacy_pdf(1, 1, None),
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
            source_bounded: true,
            controls: RetrieveControlsResponse {
                fast: request.fast,
                rerank_enabled: request.rerank.unwrap_or(false),
                dense_top_k: request.dense_top_k.unwrap_or(20),
                bm25_top_k: request.bm25_top_k.unwrap_or(20),
                rrf_k: 60,
                rerank_top_n: request.rerank_top_n.unwrap_or(0),
            },
            audit_receipt: AuditReceipt {
                version: AUDIT_RECEIPT_VERSION,
                embedding_profile_id: request
                    .embedding_profile_id
                    .clone()
                    .unwrap_or_else(|| "default".into()),
                source_bounded: true,
                controls: RetrieveControlsResponse {
                    fast: request.fast,
                    rerank_enabled: request.rerank.unwrap_or(false),
                    dense_top_k: request.dense_top_k.unwrap_or(20),
                    bm25_top_k: request.bm25_top_k.unwrap_or(20),
                    rrf_k: 60,
                    rerank_top_n: request.rerank_top_n.unwrap_or(0),
                },
                results: vec![AuditReceiptResult {
                    evidence_id: "ev-1".into(),
                    text_hash: "verified-text-hash".into(),
                    source_hash: "persisted-source-hash".into(),
                }],
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
                text_hash: "verified-text-hash".into(),
                source_id: "src-1".into(),
                source_hash: "persisted-source-hash".into(),
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
            spans: vec![
                TaskSpan {
                    sequence: 1,
                    task_id: TaskId("task-1".into()),
                    phase: "retrieval".into(),
                    started_at: "1".into(),
                    duration_ms: 7,
                    metadata: serde_json::json!({"result_count": 1}),
                },
                TaskSpan {
                    sequence: 2,
                    task_id: TaskId("task-1".into()),
                    phase: IngestTaskStage::SqliteWrite.as_str().into(),
                    started_at: "1".into(),
                    duration_ms: 11,
                    metadata: serde_json::json!({
                        "operation": "replace_source_contents",
                        "io": {
                            "scope": "source_ingest_commit",
                            "estimated_logical_write_rows": 42,
                            "logical_rows": { "chunks": 12 },
                        },
                    }),
                },
            ],
        }
    }

    fn sample_task_list_response() -> TaskListResponse {
        TaskListResponse {
            total: 4,
            aggregate: None,
            tasks: vec![
                TaskSummary {
                    id: TaskId("task-run".into()),
                    kind: TaskKind::Ingest,
                    status: TaskStatus::Running,
                    created_at: "1".into(),
                    updated_at: "2".into(),
                    started_at: Some("2".into()),
                    finished_at: None,
                    request: serde_json::json!({"source_id": "src-run"}),
                    result: None,
                    error: None,
                    queue_position: None,
                    blocking_reason: None,
                    progress: Some(TaskProgressSnapshot::phase("embedding").with_counter(
                        "embedding_vectors",
                        4,
                        Some(8),
                    )),
                },
                TaskSummary {
                    id: TaskId("task-queued".into()),
                    kind: TaskKind::Ingest,
                    status: TaskStatus::Queued,
                    created_at: "3".into(),
                    updated_at: "3".into(),
                    started_at: None,
                    finished_at: None,
                    request: serde_json::json!({"source_id": "src-queued"}),
                    result: None,
                    error: None,
                    queue_position: Some(12),
                    blocking_reason: Some("waiting for 11 queued ingest task(s) ahead".into()),
                    progress: Some(TaskProgressSnapshot::phase("queued").with_queue(
                        12,
                        Some("ingest".into()),
                        Some("waiting for 11 queued ingest task(s) ahead".into()),
                    )),
                },
                TaskSummary {
                    id: TaskId("task-unknown".into()),
                    kind: TaskKind::Ask,
                    status: TaskStatus::Running,
                    created_at: "4".into(),
                    updated_at: "5".into(),
                    started_at: Some("5".into()),
                    finished_at: None,
                    request: serde_json::json!({"question_chars": 100}),
                    result: None,
                    error: None,
                    queue_position: None,
                    blocking_reason: None,
                    progress: Some(
                        TaskProgressSnapshot::phase("chat").with_counter("tokens", 42, None),
                    ),
                },
                TaskSummary {
                    id: TaskId("task-done-counter".into()),
                    kind: TaskKind::Ingest,
                    status: TaskStatus::Running,
                    created_at: "6".into(),
                    updated_at: "7".into(),
                    started_at: Some("7".into()),
                    finished_at: None,
                    request: serde_json::json!({"source_id": "src-done"}),
                    result: None,
                    error: None,
                    queue_position: None,
                    blocking_reason: None,
                    progress: Some(
                        TaskProgressSnapshot::phase("publish")
                            .with_counter("embedding_vectors", 8, Some(8))
                            .with_recent_status("embedding complete"),
                    ),
                },
            ],
        }
    }

    fn sample_task_list_aggregate(
        terminalized: usize,
        backfilled: usize,
        embedding_waiting: usize,
        oldest_embedding_wait_ms: u64,
        publish_complete_running: usize,
    ) -> TaskListAggregate {
        sample_task_list_aggregate_with_event_sequence(
            terminalized,
            backfilled,
            embedding_waiting,
            oldest_embedding_wait_ms,
            publish_complete_running,
            42,
        )
    }

    fn sample_task_list_aggregate_with_event_sequence(
        terminalized: usize,
        backfilled: usize,
        embedding_waiting: usize,
        oldest_embedding_wait_ms: u64,
        publish_complete_running: usize,
        event_sequence_ceiling: i64,
    ) -> TaskListAggregate {
        let embedding_reasons = match embedding_waiting {
            0 => Vec::new(),
            1 => vec![TaskReasonBucket {
                reason: "embedding_batch".into(),
                count: 1,
            }],
            count => vec![
                TaskReasonBucket {
                    reason: "embedding_batch".into(),
                    count: 1,
                },
                TaskReasonBucket {
                    reason: "embedding_throughput".into(),
                    count: count.saturating_sub(1),
                },
            ],
        };
        let stale_reasons = if publish_complete_running == 0 {
            Vec::new()
        } else {
            vec![TaskReasonBucket {
                reason: "post_publish_cleanup".into(),
                count: publish_complete_running,
            }]
        };
        TaskListAggregate {
            active_total: 4,
            active_sample_size: 4,
            active_sample_limit: 100,
            turnover: TaskQueueTurnover {
                window: TaskQueueTurnoverWindow {
                    event_sequence_floor: 7,
                    event_sequence_ceiling,
                    event_limit: 1000,
                },
                recent_terminalized: terminalized,
                recent_succeeded: terminalized,
                recent_failed: 0,
                recent_cancelled: 0,
                recent_backfilled: backfilled,
            },
            embedding_wait: TaskEmbeddingWaitAggregate {
                waiting: embedding_waiting,
                oldest_wait_ms: (oldest_embedding_wait_ms > 0).then_some(oldest_embedding_wait_ms),
                reason_buckets: embedding_reasons,
            },
            stale_running: TaskStaleRunningAggregate {
                publish_complete_running,
                reason_buckets: stale_reasons,
            },
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

    fn sample_task_profile(task_id: &str) -> TaskProfile {
        TaskProfile {
            schema_version: verbatim_core::task::TASK_PROFILE_SCHEMA_VERSION,
            task_id: TaskId(task_id.into()),
            task_kind: TaskKind::Retrieve,
            status: TaskStatus::Succeeded,
            queue_wait_ms: 0,
            total_wall_ms: 25,
            controls: Default::default(),
            resources: Default::default(),
            endpoints: Vec::new(),
            retrieve: Some(verbatim_core::task::RetrieveTaskProfile {
                candidate_counters: Default::default(),
                dense: verbatim_core::task::RetrieveDenseStageProfile {
                    path: RetrievalDenseVectorPath::Bm25Only,
                    candidate_count: 0,
                    local_ms: 0,
                    query_embedding_ms: 0,
                    endpoint_latency_ms: None,
                },
                bm25: verbatim_core::task::RetrieveStageProfile {
                    candidate_count: 1,
                    local_ms: 3,
                },
                fusion: verbatim_core::task::RetrieveStageProfile {
                    candidate_count: 1,
                    local_ms: 1,
                },
                rerank: verbatim_core::task::RetrieveRerankStageProfile {
                    status: RetrievalRerankStatus::Disabled,
                    reason: None,
                    input_count: None,
                    configured_top_n: 0,
                    effective_top_n: None,
                    output_count: 0,
                    local_ms: 0,
                    endpoint_latency_ms: None,
                },
                evidence: verbatim_core::task::RetrieveEvidenceStageProfile {
                    result_count: 1,
                    graph_expanded_count: 0,
                    final_count: 1,
                    display_count: 1,
                    result_hydration_ms: 2,
                    graph_expansion_ms: 0,
                    final_pack_ms: 0,
                    display_pack_ms: 1,
                },
                display: verbatim_core::task::RetrieveDisplayStageProfile {
                    returned_count: 1,
                    response_formatting_ms: 1,
                    canonical_support_embedding_ms: None,
                    canonical_display_selection_ms: None,
                    canonical_selected_count: None,
                },
            }),
            ask: None,
        }
    }

    fn sample_ask_task_profile(task_id: &str) -> TaskProfile {
        let mut profile = sample_task_profile(task_id);
        profile.task_kind = TaskKind::Ask;
        profile.endpoints = vec![
            verbatim_core::task::TaskEndpointSummary::single_call("chat", 42),
            verbatim_core::task::TaskEndpointSummary::single_call("verifier", 7),
        ];
        profile.ask = Some(verbatim_core::task::AskTaskProfile {
            generation: verbatim_core::task::AskGenerationStageProfile {
                status: verbatim_core::task::AskGenerationStatus::Succeeded,
                call_count: 1,
                total_latency_ms: 42,
                latest_latency_ms: Some(42),
                retry_count: 0,
                error_count: 0,
                latest_error: None,
            },
            verification: verbatim_core::task::AskVerificationStageProfile {
                enabled: true,
                status: verbatim_core::task::AskVerificationStatus::Passed,
                call_count: 1,
                total_latency_ms: 7,
                latest_latency_ms: Some(7),
                retry_count: 0,
                error_count: 0,
                latest_error: None,
            },
            output: verbatim_core::task::AskOutputStageProfile {
                response_formatting_ms: 2,
                answer_chars: 120,
                citation_count: 1,
                retrieval_included: true,
            },
        });
        profile
    }

    fn sample_debug_json() -> Value {
        serde_json::json!({
            "dense_vector_path": "resident_hnsw",
            "local_spans_ms": {
                "setup_ms": 1,
                "query_embedding_ms": 2,
                "dense_vector_search_ms": 3,
                "bm25_search_ms": 4,
                "rrf_fusion_ms": 5,
                "debug_candidate_pack_ms": 6,
                "rerank_total_ms": 7,
                "result_hydration_ms": 8,
                "graph_expansion_ms": 9,
                "final_evidence_pack_ms": 10,
                "display_evidence_pack_ms": 11,
                "response_formatting_ms": 12,
                "canonical_support_embedding_ms": 13,
                "canonical_display_selection_ms": 14
            },
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
