use std::collections::BTreeSet;

use anyhow::{bail, Result};
use verbatim_core::api::CollectionFilterRequest;
use verbatim_core::collection::validate_collection_name;
use verbatim_core::config::RetrievalConfig;
use verbatim_core::types::SourceId;

pub(crate) fn apply_default_collection_scope(
    config: &RetrievalConfig,
    source_id: Option<&SourceId>,
    request: CollectionFilterRequest,
) -> CollectionFilterRequest {
    if source_id.is_some() || request.has_filters() || config.default_collections.is_empty() {
        return request;
    }

    CollectionFilterRequest {
        names: config.default_collections.clone(),
        ..request
    }
}

pub(crate) fn collection_filter_names(filter: &CollectionFilterRequest) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for raw_name in filter.collection_ids.iter().chain(&filter.names) {
        let name = raw_name.trim();
        if name.is_empty() {
            bail!("collection filter values must not be empty");
        }
        validate_collection_name(name)?;
        names.insert(name.to_string());
    }
    Ok(names.into_iter().collect())
}

pub(crate) fn collection_freshness_remediation_error(
    requested_collection_names: &BTreeSet<String>,
    required_collection_syncs: &BTreeSet<String>,
    required_source_ingests: &BTreeSet<String>,
) -> String {
    const MAX_SOURCE_COMMANDS: usize = 25;

    let mut message = String::from(
        "collection filter requires fresh collection membership and member indexes.\n\nRun the relevant remediation command(s), then retry the query:",
    );

    if required_collection_syncs.is_empty() && required_source_ingests.is_empty() {
        message.push_str("\n  verbatim collection sync <name>");
        message.push_str("\n  verbatim reindex --stale");
        append_collection_retry_command(&mut message, requested_collection_names);
        return message;
    }

    for name in required_collection_syncs {
        message.push_str(&format!("\n  verbatim collection sync {}", shell_arg(name)));
    }

    for source_id in required_source_ingests.iter().take(MAX_SOURCE_COMMANDS) {
        message.push_str(&format!("\n  verbatim ingest {}", shell_arg(source_id)));
    }

    if required_source_ingests.len() > MAX_SOURCE_COMMANDS {
        let omitted = required_source_ingests.len() - MAX_SOURCE_COMMANDS;
        message.push_str(&format!(
            "\n  # {omitted} more stale member source(s) omitted; to rebuild every stale source, run:"
        ));
        message.push_str("\n  verbatim reindex --stale");
    } else if !required_source_ingests.is_empty() {
        message.push_str("\n  # To rebuild every stale source instead, run:");
        message.push_str("\n  verbatim reindex --stale");
    }

    append_collection_retry_command(&mut message, requested_collection_names);
    message
}

fn append_collection_retry_command(
    message: &mut String,
    requested_collection_names: &BTreeSet<String>,
) {
    if requested_collection_names.is_empty() {
        return;
    }

    let collection_args = requested_collection_names
        .iter()
        .map(|name| format!(" --collection {}", shell_arg(name)))
        .collect::<String>();
    message.push_str(&format!(
        "\n\nAfter the command(s) complete, retry:\n  verbatim ask{collection_args} --require-fresh '<question>'"
    ));
}

fn shell_arg(value: &str) -> String {
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
    }) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}
