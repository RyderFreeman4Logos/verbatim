//! Source subcommand execution through the daemon client.

use std::io::Write;

use crate::client::{CliError, DaemonClient};
use crate::{render, SourceCommand};

pub(super) fn run_source<W, C>(
    command: SourceCommand,
    stdout: &mut W,
    client: &C,
) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
{
    match command {
        SourceCommand::Add { path } => {
            let response = client.add_source(&path)?;
            writeln!(stdout, "Added source: {}", response.id)?;
        }
        SourceCommand::List {
            details,
            limit,
            status,
        } => {
            if limit == Some(0) {
                return Err(CliError::Api("--limit must be >= 1".to_string()));
            }
            let sources = client.list_sources()?;
            let filtered: Vec<_> = match &status {
                Some(s) => sources
                    .iter()
                    .filter(|src| src.status == *s)
                    .cloned()
                    .collect(),
                None => sources,
            };
            if details {
                let limited: Vec<_> = match limit {
                    Some(n) => filtered.iter().take(n).cloned().collect(),
                    None => filtered.clone(),
                };
                render::write_sources(stdout, &limited)?;
            } else {
                render::write_source_summary(stdout, &filtered, limit)?;
            }
        }
        SourceCommand::Inspect { id } => {
            let source = client.get_source(&id)?;
            render::write_source(stdout, &source)?;
        }
        SourceCommand::Remove { id } => {
            client.remove_source(&id)?;
            writeln!(stdout, "Removed source: {id}")?;
        }
        SourceCommand::Relocate { id, new_path } => {
            let source = client.relocate_source(&id, &new_path)?;
            writeln!(stdout, "Relocated source: {} -> {}", source.id, source.path)?;
        }
        SourceCommand::Check => {
            let response = client.check_sources()?;
            render::write_check_stale(stdout, &response)?;
        }
    }
    Ok(0)
}
