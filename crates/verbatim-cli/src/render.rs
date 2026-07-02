use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use verbatim_core::api::{
    AddCollectionRootResponse, AskResponse, CheckStaleResponse, CitationResponse,
    CollectionFilterResponse, CollectionResultProvenance, CollectionWatcherStatus, ConfigResponse,
    EvidenceResponse, HealthResponse, IndexGcResponse, IndexProfileDeleteResponse,
    IndexStatusResponse, IngestResponse, ReindexResponse, RetrieveResponse, SourceResponse,
    TaskCreatedResponse, TaskListAggregate, TaskListResponse, TaskReasonBucket, TaskWaitEvent,
};
use verbatim_core::collection::{CollectionRecord, CollectionStatus, CollectionSyncReport};
use verbatim_core::index_gc::{IndexGcPlanEntry, IndexGcSkippedEntry};
use verbatim_core::task::{TaskEvent, TaskProgressSnapshot, TaskSpan, TaskStatus, TaskSummary};
use verbatim_core::types::{
    BBox, OcrSourceStatus, RetrievalDebug, RetrievalEvidencePackEntry, RetrievalFusedHit,
    RetrievalRerankStatus, RetrievalStageHit, SourceLocator,
};

/// Persisted sample used to estimate aggregate task-list progress across CLI calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListAggregateHistory {
    /// Highest active-task count observed for the current queue drain.
    pub baseline_total: usize,
    /// Active-task count from the previous task-list sample.
    pub previous_total: usize,
    /// Millisecond timestamp for the previous task-list sample.
    pub sampled_at_ms: u64,
    /// Stable sample of active task IDs used to avoid reusing history across queue drains.
    #[serde(default)]
    pub sampled_task_ids: Vec<String>,
    /// Highest `task_events.id` observed at the previous sample (0 if unavailable).
    ///
    /// This is a daemon-provided monotonically advancing event sequence number.
    /// Unlike `baseline_total - current_total`, it is not masked by watcher backfilling
    /// and serves as a reliable throughput signal for ETA estimation on plateau queues.
    #[serde(default)]
    pub last_event_sequence: i64,
}

const TASK_LIST_HISTORY_SAMPLE_TASKS: usize = 32;

/// Persistence action requested after rendering a task list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskListHistoryUpdate {
    /// Store the new sample as the next ETA baseline.
    Store(TaskListAggregateHistory),
    /// Clear any old sample because no active tasks remain.
    Clear,
}

pub fn write_sources<W>(writer: &mut W, sources: &[SourceResponse]) -> std::io::Result<()>
where
    W: Write,
{
    if sources.is_empty() {
        return writeln!(writer, "No sources.");
    }

    writeln!(writer, "Sources:")?;
    for source in sources {
        writeln!(
            writer,
            "  id={} status={} path={}",
            source.id, source.status, source.path
        )?;
    }
    Ok(())
}

pub fn write_source<W>(writer: &mut W, source: &SourceResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Source:")?;
    writeln!(writer, "  id: {}", source.id)?;
    writeln!(writer, "  path: {}", source.path)?;
    writeln!(writer, "  status: {}", source.status)?;
    writeln!(writer, "  hash: {}", source.hash)?;
    if let Some(parser) = &source.parser_used {
        writeln!(writer, "  parser: {parser}")?;
    }
    if let Some(ingested_at) = &source.last_ingested_at {
        writeln!(writer, "  last_ingested_at: {ingested_at}")?;
    }
    if let Some(diagnostics) = &source.diagnostics {
        if let Some(pdf) = &diagnostics.pdf {
            writeln!(
                writer,
                "  pdf: pages={} text_density={:.1} image_only_pages={}",
                pdf.page_count, pdf.text_density, pdf.image_only_page_count
            )?;
        }
        writeln!(
            writer,
            "  ocr: {} evidence_count={}",
            ocr_status_name(diagnostics.ocr.status),
            diagnostics.ocr.evidence_count
        )?;
    }
    Ok(())
}

pub fn write_collections<W>(writer: &mut W, collections: &[CollectionRecord]) -> std::io::Result<()>
where
    W: Write,
{
    if collections.is_empty() {
        return writeln!(writer, "No collections.");
    }

    writeln!(writer, "Collections:")?;
    for collection in collections {
        let synced = collection.last_synced_at.as_deref().unwrap_or("never");
        writeln!(
            writer,
            "  name={} watch={} auto_index={} synced_at={} ignore_patterns={}",
            collection.name,
            collection.watch_enabled,
            collection.auto_index_enabled,
            synced,
            collection.ignore_patterns.len()
        )?;
    }
    Ok(())
}

pub fn write_collection<W>(
    writer: &mut W,
    response: &verbatim_core::api::CollectionResponse,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Collection:")?;
    writeln!(writer, "  name: {}", response.collection.name)?;
    writeln!(
        writer,
        "  watch_enabled: {}",
        response.collection.watch_enabled
    )?;
    writeln!(
        writer,
        "  auto_index_enabled: {}",
        response.collection.auto_index_enabled
    )?;
    writeln!(writer, "  roots: {}", response.roots.len())?;
    writeln!(writer, "  members: {}", response.members.len())?;
    if !response.collection.ignore_patterns.is_empty() {
        writeln!(
            writer,
            "  ignore_patterns: {}",
            response.collection.ignore_patterns.join(", ")
        )?;
    }
    if let Some(last_synced_at) = &response.collection.last_synced_at {
        writeln!(writer, "  last_synced_at: {last_synced_at}")?;
    }
    if !response.roots.is_empty() {
        writeln!(writer, "Roots:")?;
        for root in &response.roots {
            let canonical = root
                .canonical_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string());
            writeln!(
                writer,
                "  kind={} path={} canonical={}",
                root.kind.as_str(),
                root.path.display(),
                canonical
            )?;
        }
    }
    if !response.members.is_empty() {
        writeln!(writer, "Members:")?;
        for member in &response.members {
            writeln!(
                writer,
                "  logical={} source_id={} source_path={}",
                member.logical_path,
                member.source_id.0,
                member.source_path.display()
            )?;
        }
    }
    Ok(())
}

pub fn write_collection_root_summary<W>(
    writer: &mut W,
    response: &AddCollectionRootResponse,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Collection root:")?;
    writeln!(writer, "  collection: {}", response.collection_name)?;
    writeln!(
        writer,
        "  action: {}",
        if response.added {
            "added"
        } else {
            "already_present"
        }
    )?;
    writeln!(writer, "  path: {}", response.root.path.display())?;
    writeln!(writer, "  kind: {}", response.root.kind.as_str())?;
    writeln!(writer, "  roots: {}", response.root_count)?;
    writeln!(writer, "  members: {}", response.member_count)?;
    Ok(())
}

pub fn write_collection_watcher_statuses<W>(
    writer: &mut W,
    statuses: &[CollectionWatcherStatus],
) -> std::io::Result<()>
where
    W: Write,
{
    if statuses.is_empty() {
        return writeln!(writer, "No collection watchers.");
    }

    writeln!(writer, "Collection watchers:")?;
    for status in statuses {
        writeln!(
            writer,
            "  name={} watch={} auto_index={} active={} ignored={} roots={} pending={} last_task={}",
            status.collection_name,
            status.watch_enabled,
            status.auto_index_enabled,
            status.active,
            status.ignored_by_config,
            status.watched_root_count,
            status.pending_event_count,
            status.last_task_id.as_deref().unwrap_or("-")
        )?;
        if let Some(error) = &status.last_error {
            writeln!(writer, "    last_error: {error}")?;
        }
    }
    Ok(())
}

pub fn write_collection_watcher_status<W>(
    writer: &mut W,
    status: &CollectionWatcherStatus,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Collection watcher:")?;
    writeln!(writer, "  name: {}", status.collection_name)?;
    writeln!(writer, "  watch_enabled: {}", status.watch_enabled)?;
    writeln!(
        writer,
        "  auto_index_enabled: {}",
        status.auto_index_enabled
    )?;
    writeln!(writer, "  active: {}", status.active)?;
    writeln!(writer, "  ignored_by_config: {}", status.ignored_by_config)?;
    writeln!(writer, "  watched_roots: {}", status.watched_root_count)?;
    writeln!(writer, "  pending_events: {}", status.pending_event_count)?;
    if let Some(last_event_at) = &status.last_event_at {
        writeln!(writer, "  last_event_at: {last_event_at}")?;
    }
    if let Some(last_sync_at) = &status.last_sync_at {
        writeln!(writer, "  last_sync_at: {last_sync_at}")?;
    }
    if let Some(last_task_id) = &status.last_task_id {
        writeln!(writer, "  last_task_id: {last_task_id}")?;
    }
    if let Some(error) = &status.last_error {
        writeln!(writer, "  last_error: {error}")?;
    }
    writeln!(
        writer,
        "  last_sync_diff: added={} removed={} unchanged={}",
        status.last_added, status.last_removed, status.last_unchanged
    )
}

pub fn write_collection_status<W>(writer: &mut W, status: &CollectionStatus) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Collection status:")?;
    writeln!(writer, "  name: {}", status.collection.name)?;
    writeln!(writer, "  roots: {}", status.root_count)?;
    writeln!(writer, "  members: {}", status.member_count)?;
    writeln!(
        writer,
        "  last_synced_at: {}",
        status
            .collection
            .last_synced_at
            .as_deref()
            .unwrap_or("never")
    )?;
    if let Some(report) = &status.collection.last_sync {
        write_collection_sync_report(writer, report)?;
    }
    Ok(())
}

pub fn write_collection_sync_report<W>(
    writer: &mut W,
    report: &CollectionSyncReport,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "Synced {} member(s): added={} removed={} unchanged={} skipped={}.",
        report.member_count,
        report.added,
        report.removed,
        report.unchanged,
        report.skipped.len()
    )?;
    for skip in &report.skipped {
        writeln!(
            writer,
            "  skipped reason={:?} path={} message={}",
            skip.reason, skip.path, skip.message
        )?;
    }
    Ok(())
}

pub fn write_check_stale<W>(writer: &mut W, response: &CheckStaleResponse) -> std::io::Result<()>
where
    W: Write,
{
    if response.stale.is_empty() {
        writeln!(writer, "No stale sources.")?;
    } else {
        writeln!(writer, "Stale sources:")?;
        for id in &response.stale {
            writeln!(writer, "  {id}")?;
        }
    }
    if let Some(status) = &response.profile_status {
        write_index_status_summary(writer, status)?;
        write_index_status_messages(writer, status)?;
    }
    Ok(())
}

pub fn write_index_status<W>(writer: &mut W, response: &IndexStatusResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Index status:")?;
    writeln!(
        writer,
        "  embedding_enabled: {}",
        response.embedding_enabled
    )?;
    writeln!(
        writer,
        "  active_profile_id: {}",
        response.active_profile_id
    )?;
    writeln!(writer, "  source_count: {}", response.source_count)?;
    writeln!(
        writer,
        "  stale_source_count: {}",
        response.stale_source_count
    )?;
    if !response.stale_source_ids.is_empty() {
        writeln!(
            writer,
            "  stale_source_ids: {}",
            response.stale_source_ids.join(", ")
        )?;
    }
    writeln!(writer, "  capability:")?;
    writeln!(writer, "    provider: {}", response.capability.provider)?;
    writeln!(writer, "    model: {}", response.capability.model)?;
    writeln!(writer, "    dimension: {}", response.capability.dimension)?;
    writeln!(writer, "    normalize: {}", response.capability.normalize)?;
    write_optional_field(
        writer,
        "    endpoint_identity",
        response.capability.endpoint_identity.as_deref(),
    )?;
    write_optional_field(
        writer,
        "    requested_model",
        response.capability.requested_model.as_deref(),
    )?;
    write_optional_field(
        writer,
        "    served_model",
        response.capability.served_model.as_deref(),
    )?;
    write_optional_usize_field(
        writer,
        "    max_context_tokens",
        response.capability.max_context_tokens,
    )?;
    write_optional_field(writer, "    dtype", response.capability.dtype.as_deref())?;
    write_optional_field(
        writer,
        "    quantization",
        response.capability.quantization.as_deref(),
    )?;
    write_optional_field(
        writer,
        "    weight_identity",
        response.capability.weight_identity.as_deref(),
    )?;
    writeln!(writer, "  chunking:")?;
    writeln!(writer, "    version: {}", response.chunking.version)?;
    writeln!(
        writer,
        "    child_target_tokens: {}",
        response.chunking.child_target_tokens
    )?;
    writeln!(
        writer,
        "    child_overlap_tokens: {}",
        response.chunking.child_overlap_tokens
    )?;
    writeln!(
        writer,
        "    parent_children_count: {}",
        response.chunking.parent_children_count
    )?;
    write_optional_usize_field(
        writer,
        "    embedding_input_budget_tokens",
        response.chunking.embedding_input_budget_tokens,
    )?;
    write_index_status_messages(writer, response)
}

fn write_index_status_messages<W>(
    writer: &mut W,
    response: &IndexStatusResponse,
) -> std::io::Result<()>
where
    W: Write,
{
    if response.messages.is_empty() {
        return Ok(());
    }
    writeln!(writer, "  diagnostics:")?;
    for message in &response.messages {
        writeln!(writer, "    - {message}")?;
    }
    Ok(())
}

fn write_index_status_summary<W>(
    writer: &mut W,
    response: &IndexStatusResponse,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Embedding profile:")?;
    writeln!(
        writer,
        "  active_profile_id: {}",
        response.active_profile_id
    )?;
    writeln!(writer, "  provider: {}", response.capability.provider)?;
    writeln!(writer, "  model: {}", response.capability.model)?;
    write_optional_field(
        writer,
        "  served_model",
        response.capability.served_model.as_deref(),
    )?;
    write_optional_usize_field(
        writer,
        "  max_context_tokens",
        response.capability.max_context_tokens,
    )?;
    write_optional_field(writer, "  dtype", response.capability.dtype.as_deref())?;
    write_optional_field(
        writer,
        "  quantization",
        response.capability.quantization.as_deref(),
    )?;
    writeln!(writer, "  chunking:")?;
    writeln!(writer, "    version: {}", response.chunking.version)?;
    write_optional_usize_field(
        writer,
        "    embedding_input_budget_tokens",
        response.chunking.embedding_input_budget_tokens,
    )
}

fn write_optional_field<W>(writer: &mut W, name: &str, value: Option<&str>) -> std::io::Result<()>
where
    W: Write,
{
    if let Some(value) = value {
        writeln!(writer, "{name}: {value}")?;
    }
    Ok(())
}

fn write_optional_usize_field<W>(
    writer: &mut W,
    name: &str,
    value: Option<usize>,
) -> std::io::Result<()>
where
    W: Write,
{
    if let Some(value) = value {
        writeln!(writer, "{name}: {value}")?;
    }
    Ok(())
}

pub fn write_ingest<W>(writer: &mut W, response: &IngestResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Ingested {} source(s).", response.ingested)
}

pub fn write_reindex<W>(writer: &mut W, response: &ReindexResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Reindexed {} source(s).", response.reindexed)
}

pub fn write_index_gc<W>(writer: &mut W, response: &IndexGcResponse) -> std::io::Result<()>
where
    W: Write,
{
    let action = if response.dry_run {
        "Index GC dry-run"
    } else {
        "Index GC"
    };
    writeln!(writer, "{action}:")?;
    writeln!(
        writer,
        "  policy: retain_previous_generations={} stale_staging_seconds={}",
        response.policy.retain_previous_generations, response.policy.stale_staging_seconds
    )?;
    writeln!(
        writer,
        "  planned: {} artifact(s), {} approximate reclaimable",
        response.plan.entries.len(),
        format_bytes(response.plan.approximate_reclaim_bytes)
    )?;
    if !response.dry_run {
        writeln!(
            writer,
            "  removed: {} artifact(s), {} reclaimed",
            response.apply.removed.len(),
            format_bytes(response.apply.reclaimed_bytes)
        )?;
    }

    let entries = if response.dry_run {
        &response.plan.entries
    } else {
        &response.apply.removed
    };
    if entries.is_empty() {
        writeln!(writer, "No removable index artifacts.")?;
    } else {
        let heading = if response.dry_run {
            "Planned removals:"
        } else {
            "Removed artifacts:"
        };
        writeln!(writer, "{heading}")?;
        for entry in entries {
            write_index_gc_entry(writer, entry)?;
        }
    }

    if !response.plan.skipped.is_empty() {
        writeln!(writer, "Skipped:")?;
        for entry in &response.plan.skipped {
            write_index_gc_skipped(writer, entry)?;
        }
    }
    Ok(())
}

pub fn write_index_profile_delete<W>(
    writer: &mut W,
    response: &IndexProfileDeleteResponse,
) -> std::io::Result<()>
where
    W: Write,
{
    let heading = if response.dry_run {
        "Index profile delete dry-run"
    } else {
        "Index profile delete complete"
    };
    writeln!(writer, "{heading}")?;
    writeln!(writer, "  profile: {}", response.plan.profile_id)?;
    writeln!(writer, "  active_profile: {}", response.plan.active_profile)?;
    writeln!(
        writer,
        "  planned sqlite rows: chunk_vectors={} embedding_cache={} source_status={} embeddings_meta={} index_meta={} profiles={}",
        response.plan.sqlite.chunk_vectors,
        response.plan.sqlite.embedding_cache_entries,
        response.plan.sqlite.source_embedding_statuses,
        response.plan.sqlite.embeddings_meta_entries,
        response.plan.sqlite.embedding_profile_index_meta_entries,
        response.plan.sqlite.embedding_profiles,
    )?;
    writeln!(
        writer,
        "  planned artifacts: {} reclaimable",
        format_bytes(response.plan.approximate_reclaim_bytes)
    )?;
    if !response.dry_run {
        writeln!(
            writer,
            "  removed sqlite rows: chunk_vectors={} embedding_cache={} source_status={} embeddings_meta={} index_meta={} profiles={}",
            response.apply.sqlite.chunk_vectors,
            response.apply.sqlite.embedding_cache_entries,
            response.apply.sqlite.source_embedding_statuses,
            response.apply.sqlite.embeddings_meta_entries,
            response.apply.sqlite.embedding_profile_index_meta_entries,
            response.apply.sqlite.embedding_profiles,
        )?;
        writeln!(
            writer,
            "  removed artifacts: {} reclaimed",
            format_bytes(response.apply.reclaimed_bytes)
        )?;
    }

    if let Some(artifact) = &response.plan.artifact {
        let action = if response.dry_run {
            "would remove"
        } else {
            "planned"
        };
        writeln!(
            writer,
            "  artifact {action}: bytes={} path={}",
            format_bytes(artifact.approximate_bytes),
            artifact.path.display()
        )?;
        writeln!(writer, "    reason: {}", artifact.reason)?;
    } else {
        writeln!(writer, "No profile artifact directory found.")?;
    }

    if !response.dry_run && !response.apply.removed_artifacts.is_empty() {
        writeln!(writer, "Removed artifact directories:")?;
        for artifact in &response.apply.removed_artifacts {
            writeln!(
                writer,
                "  bytes={} path={}",
                format_bytes(artifact.approximate_bytes),
                artifact.path.display()
            )?;
        }
    }

    if !response.plan.skipped.is_empty() {
        writeln!(writer, "Skipped:")?;
        for skipped in &response.plan.skipped {
            writeln!(writer, "  path={}", skipped.path.display())?;
            writeln!(writer, "    reason: {}", skipped.reason)?;
        }
    }
    Ok(())
}

fn write_index_gc_entry<W>(writer: &mut W, entry: &IndexGcPlanEntry) -> std::io::Result<()>
where
    W: Write,
{
    let profile = entry.profile_id.as_deref().unwrap_or("-");
    let generation = entry
        .generation
        .map(|generation| generation.to_string())
        .unwrap_or_else(|| "-".to_string());
    writeln!(
        writer,
        "  kind={} profile={} generation={} bytes={} path={}",
        entry.kind.as_str(),
        profile,
        generation,
        format_bytes(entry.approximate_bytes),
        entry.path.display()
    )?;
    writeln!(writer, "    reason: {}", entry.reason)
}

fn write_index_gc_skipped<W>(writer: &mut W, entry: &IndexGcSkippedEntry) -> std::io::Result<()>
where
    W: Write,
{
    let kind = entry.kind.map(|kind| kind.as_str()).unwrap_or("-");
    let profile = entry.profile_id.as_deref().unwrap_or("-");
    let generation = entry
        .generation
        .map(|generation| generation.to_string())
        .unwrap_or_else(|| "-".to_string());
    writeln!(
        writer,
        "  kind={} profile={} generation={} path={}",
        kind,
        profile,
        generation,
        entry.path.display()
    )?;
    writeln!(writer, "    reason: {}", entry.reason)
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1}GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1}MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1}KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes}B")
    }
}

pub fn write_task_created<W>(writer: &mut W, response: &TaskCreatedResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Task queued: {}", response.task_id)?;
    writeln!(writer, "Wait: verbatim task wait {}", response.task_id)
}

pub fn write_task_status_line<W>(writer: &mut W, task: &TaskSummary) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Task {} status={}", task.id.0, task.status.as_str())
}

pub fn write_task_progress_line<W>(writer: &mut W, task: &TaskSummary) -> std::io::Result<()>
where
    W: Write,
{
    let Some(progress) = &task.progress else {
        return Ok(());
    };
    writeln!(writer, "progress: {}", task_progress_summary(progress))
}

pub fn write_task_list<W>(
    writer: &mut W,
    response: &TaskListResponse,
    details: bool,
    history: Option<&TaskListAggregateHistory>,
    sampled_at_ms: u64,
) -> std::io::Result<TaskListHistoryUpdate>
where
    W: Write,
{
    if response.tasks.is_empty() {
        writeln!(writer, "No active tasks.")?;
        return Ok(TaskListHistoryUpdate::Clear);
    }
    let summary = task_queue_summary(response, history, sampled_at_ms);
    write_task_queue_summary(writer, &summary)?;
    if !details {
        writeln!(
            writer,
            "Use `verbatim task list --details` for per-task rows."
        )?;
        return Ok(TaskListHistoryUpdate::Store(summary.history));
    }
    writeln!(writer, "Active tasks:")?;
    for task in &response.tasks {
        writeln!(writer, "  {}", task_progress_list_line(task))?;
    }
    if response.total > response.tasks.len() {
        writeln!(
            writer,
            "  showing {} of {} active tasks",
            response.tasks.len(),
            response.total
        )?;
    }
    Ok(TaskListHistoryUpdate::Store(summary.history))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskQueueSummary {
    completed: usize,
    total: usize,
    percent_tenths: u64,
    eta_seconds: Option<u64>,
    explanations: Vec<String>,
    history: TaskListAggregateHistory,
}

fn task_queue_summary(
    response: &TaskListResponse,
    history: Option<&TaskListAggregateHistory>,
    sampled_at_ms: u64,
) -> TaskQueueSummary {
    let current_total = response.total.max(response.tasks.len());
    let sampled_task_ids = sample_task_ids(response);
    // Monotonically advancing daemon event sequence (0 when aggregate is absent).
    let current_event_sequence = response
        .aggregate
        .as_ref()
        .map(|agg| agg.turnover.window.event_sequence_ceiling)
        .unwrap_or(0);
    let reusable_history = history.filter(|history| {
        history.baseline_total >= current_total
            && history.baseline_total > 0
            && (history.previous_total > current_total
                || sampled_task_ids_overlap(history, &sampled_task_ids))
    });
    let baseline_total = reusable_history
        .map(|history| history.baseline_total)
        .unwrap_or(current_total);
    let completed = baseline_total.saturating_sub(current_total);
    let percent_tenths = if baseline_total == 0 {
        0
    } else {
        ((completed as u128 * 1000) / baseline_total as u128) as u64
    };
    let eta_seconds = reusable_history.and_then(|history| {
        if sampled_at_ms <= history.sampled_at_ms {
            return None;
        }
        let elapsed_ms = sampled_at_ms - history.sampled_at_ms;
        if elapsed_ms == 0 {
            return None;
        }
        // Primary path: the queue shrank between samples — throughput is unambiguous.
        if history.previous_total > current_total {
            let completed_since_previous = history.previous_total - current_total;
            let numerator = current_total as u128 * elapsed_ms as u128;
            let denominator = completed_since_previous as u128 * 1000;
            return Some(numerator.div_ceil(denominator) as u64);
        }
        // When the queue did not shrink (watcher backfilled tasks), we cannot
        // derive a reliable completion rate from active-total changes alone.
        // The daemon's task_events.id advances for every event type (progress,
        // queued, etc.), not just terminal events, so using it as throughput
        // would overcount.  A proper fix requires a daemon-side monotonic
        // terminalized counter; until then, ETA stays "--" for plateau queues.
        None
    });
    let reusable_active_total_unchanged = reusable_history.is_some_and(|history| {
        history.previous_total == current_total && sampled_at_ms > history.sampled_at_ms
    });
    let same_total_as_previous_sample = history.is_some_and(|history| {
        history.previous_total == current_total && sampled_at_ms > history.sampled_at_ms
    });
    let explanations = response
        .aggregate
        .as_ref()
        .map(|aggregate| {
            let active_total_unchanged = reusable_active_total_unchanged
                || (same_total_as_previous_sample && has_recent_turnover(aggregate));
            task_queue_explanations(aggregate, active_total_unchanged)
        })
        .unwrap_or_default();
    TaskQueueSummary {
        completed,
        total: baseline_total,
        percent_tenths,
        eta_seconds,
        explanations,
        history: TaskListAggregateHistory {
            baseline_total,
            previous_total: current_total,
            sampled_at_ms,
            sampled_task_ids,
            last_event_sequence: current_event_sequence,
        },
    }
}

fn sample_task_ids(response: &TaskListResponse) -> Vec<String> {
    response
        .tasks
        .iter()
        .take(TASK_LIST_HISTORY_SAMPLE_TASKS)
        .map(|task| task.id.0.clone())
        .collect()
}

fn sampled_task_ids_overlap(
    history: &TaskListAggregateHistory,
    sampled_task_ids: &[String],
) -> bool {
    !history.sampled_task_ids.is_empty()
        && !sampled_task_ids.is_empty()
        && sampled_task_ids
            .iter()
            .any(|task_id| history.sampled_task_ids.contains(task_id))
}

fn write_task_queue_summary<W>(writer: &mut W, summary: &TaskQueueSummary) -> std::io::Result<()>
where
    W: Write,
{
    let percent = summary.percent_tenths as f64 / 10.0;
    writeln!(
        writer,
        "Task queue: {} {}/{} {:.1}% ETA {}",
        aggregate_progress_bar(percent),
        summary.completed,
        summary.total,
        percent,
        format_eta(summary.eta_seconds)
    )?;
    for explanation in &summary.explanations {
        writeln!(writer, "  {explanation}")?;
    }
    Ok(())
}

fn format_eta(eta_seconds: Option<u64>) -> String {
    let Some(seconds) = eta_seconds else {
        return "--".into();
    };
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds.div_ceil(60);
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    let remainder_minutes = minutes % 60;
    if remainder_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remainder_minutes}m")
    }
}

fn task_queue_explanations(
    aggregate: &TaskListAggregate,
    active_total_unchanged: bool,
) -> Vec<String> {
    let mut explanations = Vec::new();
    if active_total_unchanged {
        if has_recent_turnover(aggregate) {
            explanations.push(format!(
                "active total unchanged; recent turnover terminalized={} backfilled={} window={}..{}",
                aggregate.turnover.recent_terminalized,
                aggregate.turnover.recent_backfilled,
                aggregate.turnover.window.event_sequence_floor,
                aggregate.turnover.window.event_sequence_ceiling,
            ));
        } else {
            explanations.push(format!(
                "active total unchanged; no recent completions in last {} task events",
                aggregate.turnover.window.event_limit
            ));
        }
    }
    if aggregate.embedding_wait.waiting > 0 {
        let oldest = aggregate
            .embedding_wait
            .oldest_wait_ms
            .map(format_duration_ms)
            .unwrap_or_else(|| "unknown age".to_string());
        explanations.push(format!(
            "embedding wait: {} sampled active task(s), oldest {}, reasons {}",
            aggregate.embedding_wait.waiting,
            oldest,
            format_reason_buckets(&aggregate.embedding_wait.reason_buckets)
        ));
    }
    if aggregate.stale_running.publish_complete_running > 0 {
        explanations.push(format!(
            "stale running: {} publish-complete task(s), reasons {}",
            aggregate.stale_running.publish_complete_running,
            format_reason_buckets(&aggregate.stale_running.reason_buckets)
        ));
    }
    explanations
}

fn has_recent_turnover(aggregate: &TaskListAggregate) -> bool {
    aggregate.turnover.recent_terminalized > 0 || aggregate.turnover.recent_backfilled > 0
}

fn format_reason_buckets(buckets: &[TaskReasonBucket]) -> String {
    if buckets.is_empty() {
        return "unknown".to_string();
    }
    buckets
        .iter()
        .map(|bucket| format!("{}={}", bucket.reason, bucket.count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_duration_ms(ms: u64) -> String {
    let seconds = ms.div_ceil(1000);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds.div_ceil(60);
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    let remainder_minutes = minutes % 60;
    if remainder_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remainder_minutes}m")
    }
}

fn aggregate_progress_bar(percent: f64) -> String {
    let clamped = percent.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * 20.0).floor() as usize;
    let empty = 20usize.saturating_sub(filled);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(empty))
}

pub fn write_task_summary<W>(
    writer: &mut W,
    task: &TaskSummary,
    spans: &[TaskSpan],
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Task: {}", task.id.0)?;
    writeln!(writer, "  kind: {}", task.kind.as_str())?;
    writeln!(writer, "  status: {}", task.status.as_str())?;
    if let Some(position) = task.queue_position {
        writeln!(writer, "  queue_position: {position}")?;
    }
    if let Some(reason) = &task.blocking_reason {
        writeln!(writer, "  blocking_reason: {reason}")?;
    }
    if let Some(progress) = &task.progress {
        writeln!(writer, "  progress: {}", task_progress_summary(progress))?;
    }
    writeln!(writer, "  created_at: {}", task.created_at)?;
    if let Some(started_at) = &task.started_at {
        writeln!(writer, "  started_at: {started_at}")?;
    }
    if let Some(finished_at) = &task.finished_at {
        writeln!(writer, "  finished_at: {finished_at}")?;
    }
    if let Some(error) = &task.error {
        writeln!(writer, "  error: {error}")?;
    }
    writeln!(writer, "  request: {}", compact_json(&task.request))?;
    if let Some(result) = &task.result {
        if let Some(summary) = task_embedding_cache_summary(result) {
            writeln!(writer, "  embedding_cache: {summary}")?;
        }
        writeln!(writer, "  result: {}", compact_json(result))?;
    }
    write_task_spans(writer, spans)?;
    if task.status == TaskStatus::Cancelled {
        writeln!(
            writer,
            "  note: cancellation is best-effort for in-flight model/file work"
        )?;
    }
    Ok(())
}

pub fn write_task_events<W>(writer: &mut W, events: &[TaskEvent]) -> std::io::Result<()>
where
    W: Write,
{
    for event in events {
        if event.event_type == "progress" {
            if let Ok(progress) =
                serde_json::from_value::<TaskProgressSnapshot>(event.payload.clone())
            {
                writeln!(
                    writer,
                    "[{}] progress: {}",
                    event.sequence,
                    task_progress_summary(&progress)
                )?;
                continue;
            }
        }
        if event
            .payload
            .as_object()
            .is_some_and(|object| object.is_empty())
        {
            writeln!(
                writer,
                "[{}] {}: {}",
                event.sequence, event.event_type, event.message
            )?;
        } else {
            writeln!(
                writer,
                "[{}] {}: {} {}",
                event.sequence,
                event.event_type,
                event.message,
                compact_json(&event.payload)
            )?;
        }
    }
    Ok(())
}

pub fn write_task_wait_timeout_summary<W>(
    writer: &mut W,
    last_event: Option<&TaskWaitEvent>,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Last known task state before timeout:")?;
    let Some(event) = last_event else {
        return writeln!(writer, "  unavailable: no task event received");
    };

    if event.terminal {
        write_task_summary(writer, &event.task, &event.spans)?;
    } else {
        write_task_status_line(writer, &event.task)?;
        write_task_progress_line(writer, &event.task)?;
        if !event.spans.is_empty() {
            write_task_spans(writer, &event.spans)?;
        }
    }

    if !event.events.is_empty() {
        writeln!(writer, "Last task events:")?;
        write_task_events(writer, &event.events)?;
    }

    Ok(())
}

fn task_progress_summary(progress: &TaskProgressSnapshot) -> String {
    let mut parts = Vec::new();
    if let Some(phase) = &progress.phase {
        parts.push(format!(
            "phase={} elapsed={}ms",
            phase.name, phase.elapsed_ms
        ));
    }
    if let Some(queue) = &progress.queue {
        parts.push(format!("queue_position={}", queue.position));
        if let Some(worker) = &queue.active_worker_kind {
            parts.push(format!("active_worker={worker}"));
        }
        if let Some(reason) = &queue.blocking_reason {
            parts.push(format!("blocked=\"{reason}\""));
        }
    }
    if let Some(worker) = &progress.active_worker_kind {
        parts.push(format!("active_worker={worker}"));
    }
    if let Some(reason) = &progress.wait_reason {
        parts.push(format!("wait_reason={reason}"));
    }
    for counter in &progress.counters {
        match counter.total {
            Some(total) => parts.push(format!("{}={}/{}", counter.name, counter.completed, total)),
            None => parts.push(format!("{}={}", counter.name, counter.completed)),
        }
    }
    for endpoint in &progress.endpoints {
        if let Some(latency_ms) = endpoint.latest_latency_ms {
            parts.push(format!("{}.latest={}ms", endpoint.name, latency_ms));
        }
        if let Some(latency_ms) = endpoint.first_token_latency_ms {
            parts.push(format!("{}.first_token={}ms", endpoint.name, latency_ms));
        }
        if let Some(latency_ms) = endpoint.p50_latency_ms {
            parts.push(format!("{}.p50={}ms", endpoint.name, latency_ms));
        }
        if let Some(latency_ms) = endpoint.p95_latency_ms {
            parts.push(format!("{}.p95={}ms", endpoint.name, latency_ms));
        }
        if let Some(error) = &endpoint.latest_error {
            parts.push(format!("{}.error=\"{error}\"", endpoint.name));
        }
    }
    if let Some(status) = &progress.recent_status {
        parts.push(format!("status=\"{status}\""));
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(" ")
    }
}

fn task_progress_list_line(task: &TaskSummary) -> String {
    let (bar, detail) = match &task.progress {
        Some(progress) => (
            task_progress_bar(task.status, progress),
            task_progress_list_detail(task, progress),
        ),
        None => (
            unknown_task_progress_bar(),
            task_progress_fallback_detail(task),
        ),
    };
    format!(
        "{} {} {} {}{}",
        task.id.0,
        task.kind.as_str(),
        task.status.as_str(),
        bar,
        detail
    )
}

fn task_progress_bar(status: TaskStatus, progress: &TaskProgressSnapshot) -> String {
    let Some(counter) = progress
        .counters
        .iter()
        .find(|counter| counter.total.is_some_and(|total| total > 0))
    else {
        return unknown_task_progress_bar();
    };
    let total = counter.total.unwrap_or(0);
    let percent = if total == 0 {
        0
    } else {
        counter
            .completed
            .saturating_mul(100)
            .checked_div(total)
            .unwrap_or(0)
            .min(100)
    };
    let filled = (percent * 20 / 100) as usize;
    let empty = 20usize.saturating_sub(filled);
    let suffix = if status == TaskStatus::Running && percent == 100 {
        " (still running)"
    } else {
        ""
    };
    format!(
        "[{}{}] {:>3}%{suffix}",
        "#".repeat(filled),
        "-".repeat(empty),
        percent
    )
}

fn unknown_task_progress_bar() -> String {
    "[????????????????????]   --".into()
}

fn task_progress_list_detail(task: &TaskSummary, progress: &TaskProgressSnapshot) -> String {
    let mut parts = Vec::new();
    if let Some(phase) = &progress.phase {
        parts.push(format!("{} elapsed={}ms", phase.name, phase.elapsed_ms));
    }
    if let Some(position) = task
        .queue_position
        .or_else(|| progress.queue.as_ref().map(|queue| queue.position))
    {
        parts.push(format!("queue #{position}"));
    }
    for counter in &progress.counters {
        match counter.total {
            Some(total) => parts.push(format!("{} {}/{}", counter.name, counter.completed, total)),
            None => parts.push(format!("{} {}", counter.name, counter.completed)),
        }
    }
    if let Some(status) = &progress.recent_status {
        parts.push(status.clone());
    }
    if let Some(reason) = &progress.wait_reason {
        parts.push(format!("wait_reason {reason}"));
    }
    if let Some(reason) = task.blocking_reason.as_ref().or_else(|| {
        progress
            .queue
            .as_ref()
            .and_then(|queue| queue.blocking_reason.as_ref())
    }) {
        parts.push(reason.clone());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" {}", parts.join(" | "))
    }
}

fn task_progress_fallback_detail(task: &TaskSummary) -> String {
    task.blocking_reason
        .as_ref()
        .map(|reason| format!(" {reason}"))
        .unwrap_or_default()
}

fn task_embedding_cache_summary(result: &Value) -> Option<String> {
    let cache = result.get("embedding_cache")?.as_object()?;
    let value = |key: &str| cache.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(format!(
        "hits={} misses={} embedded={} reused={} changed={}",
        value("cache_hits"),
        value("cache_misses"),
        value("embedded_chunks"),
        value("reused_chunks"),
        value("changed_chunks")
    ))
}

pub fn write_task_spans<W>(writer: &mut W, spans: &[TaskSpan]) -> std::io::Result<()>
where
    W: Write,
{
    if spans.is_empty() {
        return Ok(());
    }
    writeln!(writer, "  spans:")?;
    for span in spans {
        writeln!(
            writer,
            "    {} {}ms {}",
            span.phase,
            span.duration_ms,
            compact_json(&span.metadata)
        )?;
    }
    Ok(())
}

pub fn write_evidence<W>(writer: &mut W, evidence: &EvidenceResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Evidence:")?;
    writeln!(writer, "  id: {}", evidence.id)?;
    writeln!(writer, "  source_id: {}", evidence.source_id)?;
    writeln!(writer, "  kind: {}", evidence.kind)?;
    if let Some(derived_from) = &evidence.derived_from {
        writeln!(writer, "  derived_from: {derived_from}")?;
    }
    writeln!(writer, "  locator: {}", evidence.locator)?;
    write_structured_locator_details(writer, &evidence.structured_locator)?;
    writeln!(writer, "  position: {}", evidence.position)?;
    if !evidence.heading_path.is_empty() {
        writeln!(
            writer,
            "  heading_path: {}",
            evidence.heading_path.join(" > ")
        )?;
    }
    if let Some(artifact) = &evidence.image_artifact {
        writeln!(writer, "  image_artifact:")?;
        writeln!(writer, "    image_id: {}", artifact.image_id)?;
        writeln!(writer, "    path: {}", artifact.path)?;
        writeln!(
            writer,
            "    mime_type: {} dimensions={}x{} page={} image_index={}",
            artifact.mime_type,
            artifact.width,
            artifact.height,
            artifact.page,
            artifact.image_index
        )?;
    }
    writeln!(writer)?;
    writeln!(writer, "Text:")?;
    writeln!(writer, "{}", evidence.text)
}

fn write_structured_locator_details<W>(
    writer: &mut W,
    locator: &SourceLocator,
) -> std::io::Result<()>
where
    W: Write,
{
    write_structured_locator_details_with_indent(writer, locator, "  ")
}

fn write_structured_locator_details_with_indent<W>(
    writer: &mut W,
    locator: &SourceLocator,
    indent: &str,
) -> std::io::Result<()>
where
    W: Write,
{
    match locator {
        SourceLocator::PdfOcr {
            page_label,
            line_index,
            word_index,
            bbox,
            ocr,
            ..
        } => {
            writeln!(writer, "{indent}ocr_locator:")?;
            if let Some(page_label) = page_label {
                writeln!(writer, "{indent}  page_label: {page_label}")?;
            }
            writeln!(writer, "{indent}  line_index: {line_index}")?;
            if let Some(word_index) = word_index {
                writeln!(writer, "{indent}  word_index: {word_index}")?;
            }
            if let Some(bbox) = bbox {
                writeln!(writer, "{indent}  bbox: {}", display_bbox(bbox))?;
            }
            writeln!(writer, "{indent}  engine: {}", ocr.profile.engine)?;
            if let Some(version) = &ocr.profile.engine_version {
                writeln!(writer, "{indent}  engine_version: {version}")?;
            }
            writeln!(writer, "{indent}  language: {}", ocr.profile.language)?;
            writeln!(writer, "{indent}  profile: {}", ocr.profile.profile)?;
            writeln!(writer, "{indent}  profile_hash: {}", ocr.profile_hash)?;
            if let Some(confidence) = ocr.confidence {
                writeln!(writer, "{indent}  confidence: {confidence:.2}")?;
            }
            writeln!(writer, "{indent}  text_hash: {}", ocr.text_hash)?;
        }
        SourceLocator::Markdown {
            line_start,
            line_end,
            byte_start,
            byte_end,
            block_kind,
            block_index,
            block_hash,
            heading_level,
            heading_slug,
            heading_path,
            ..
        } => {
            writeln!(writer, "{indent}markdown_locator:")?;
            writeln!(writer, "{indent}  block_kind: {}", block_kind.as_str())?;
            writeln!(writer, "{indent}  block_index: {block_index}")?;
            writeln!(writer, "{indent}  line_range: {line_start}-{line_end}")?;
            writeln!(writer, "{indent}  byte_range: {byte_start}-{byte_end}")?;
            writeln!(writer, "{indent}  block_hash: {block_hash}")?;
            if let Some(level) = heading_level {
                writeln!(writer, "{indent}  heading_level: {level}")?;
            }
            if let Some(slug) = heading_slug {
                writeln!(writer, "{indent}  heading_slug: {slug}")?;
            }
            for heading in heading_path {
                writeln!(
                    writer,
                    "{indent}  heading: level={} line={} slug={} text={}",
                    heading.level, heading.line, heading.slug, heading.text
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn display_bbox(bbox: &BBox) -> String {
    format!(
        "[{:.2},{:.2},{:.2},{:.2}]",
        bbox.x0, bbox.y0, bbox.x1, bbox.y1
    )
}

fn ocr_status_name(status: OcrSourceStatus) -> &'static str {
    match status {
        OcrSourceStatus::NotRequired => "not_required",
        OcrSourceStatus::Disabled => "disabled_recommended",
        OcrSourceStatus::Recommended => "recommended",
        OcrSourceStatus::Applied => "applied",
        OcrSourceStatus::Stale => "stale",
    }
}

pub fn write_config<W>(writer: &mut W, config: &ConfigResponse) -> std::io::Result<()>
where
    W: Write,
{
    serde_json::to_writer_pretty(&mut *writer, config).map_err(io::Error::other)?;
    writeln!(writer)
}

pub fn write_health<W>(writer: &mut W, health: &HealthResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Daemon status: {}", health.status)?;
    let budget = &health.memory_budget;
    let limit = budget
        .limit_mb
        .map(|limit| format!("{limit} MB"))
        .unwrap_or_else(|| "unbounded".to_string());
    let available = budget
        .available_mb
        .map(|available| format!("{available} MB"))
        .unwrap_or_else(|| "unbounded".to_string());
    writeln!(
        writer,
        "Memory budget: limit={} rss={} MB reserved={} MB available={} enforcement={:?}",
        limit, budget.rss_mb, budget.reserved_mb, available, budget.enforcement
    )?;
    if !budget.active_reservations.is_empty() {
        writeln!(writer, "Memory reservations:")?;
        for reservation in &budget.active_reservations {
            writeln!(
                writer,
                "  {} owner={} estimated={} MB age_ms={}",
                reservation.key,
                reservation.owner,
                reservation.estimated_mb,
                reservation.reserved_for_millis
            )?;
        }
    }
    if !health.resources.is_empty() {
        writeln!(writer, "Resources:")?;
        for resource in &health.resources {
            writeln!(
                writer,
                "  {} active={} queued={} completed={} errors={} wait_ms={} service_ms={}",
                resource.name,
                resource.active,
                resource.queued,
                resource.completed,
                resource.errors,
                resource.last_queue_wait_ms.unwrap_or(0),
                resource.last_service_ms.unwrap_or(0)
            )?;
        }
    }
    Ok(())
}

pub fn write_ask_response<W>(writer: &mut W, response: &AskResponse) -> std::io::Result<()>
where
    W: Write,
{
    if let Some(context) = &response.context {
        return write_retrieve_response(writer, context);
    }

    writeln!(writer, "{}", response.answer)?;
    write_citations(writer, &response.citations)?;
    if let Some(collection_filter) = &response.collection_filter {
        write_collection_filter_summary(writer, collection_filter)?;
    }

    if let Some(debug) = &response.retrieval {
        write_retrieval_debug_typed(writer, debug)?;
    }

    Ok(())
}

pub fn write_retrieve_response<W>(writer: &mut W, response: &RetrieveResponse) -> io::Result<()>
where
    W: Write,
{
    if response.results.is_empty() {
        writeln!(writer, "No retrieval results on this page.")?;
        return Ok(());
    }

    for row in retrieve_display_rows(response) {
        writeln!(
            writer,
            "{}. score={} {}",
            row.rank,
            format_score(row.score),
            row.citation
        )?;
        write_indented_snippet(writer, row.snippet, "   ")?;
    }

    Ok(())
}

pub fn write_retrieve_debug_response<W>(
    writer: &mut W,
    response: &RetrieveResponse,
) -> io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Context pack: {}", response.task_id)?;
    writeln!(writer, "  query: {}", response.query)?;
    if let Some(source_id) = &response.source_id {
        writeln!(writer, "  source_id: {source_id}")?;
    }
    if let Some(collection_filter) = &response.collection_filter {
        write_collection_filter_summary(writer, collection_filter)?;
    }
    writeln!(
        writer,
        "  page: {} page_size={} limit={} total={} returned={}",
        response.page,
        response.page_size,
        response.limit,
        response.total_results,
        response.returned_results
    )?;
    writeln!(
        writer,
        "  controls: fast={} rerank={} dense_top_k={} bm25_top_k={} rerank_top_n={}",
        response.controls.fast,
        response.controls.rerank_enabled,
        response.controls.dense_top_k,
        response.controls.bm25_top_k,
        response.controls.rerank_top_n
    )?;
    for timing in &response.timings {
        writeln!(
            writer,
            "  timing: {}={}ms",
            timing.phase, timing.duration_ms
        )?;
    }

    if response.results.is_empty() {
        writeln!(writer, "No retrieval results on this page.")?;
    } else {
        writeln!(writer)?;
        writeln!(writer, "Results:")?;
        for result in &response.results {
            writeln!(
                writer,
                "  [{}] {} score={:.4} evidence={} kind={} role={} source={}",
                result.index,
                result.label,
                result.score,
                result.evidence_id,
                result.kind,
                result.role,
                result.source_id
            )?;
            if let Some(source_path) = &result.source_path {
                writeln!(writer, "      source_path: {source_path}")?;
            }
            write_collection_provenance(writer, &result.collections, "      ")?;
            writeln!(writer, "      locator: {}", result.locator)?;
            if let Some(locator) = &result.structured_locator {
                write_structured_locator_details_with_indent(writer, locator, "      ")?;
            }
            writeln!(writer, "      snippet: {}", result.snippet)?;
        }
    }

    if let Some(debug) = &response.debug {
        write_retrieval_debug_typed(writer, debug)?;
    }

    Ok(())
}

pub fn write_retrieve_debug_summary<W>(
    writer: &mut W,
    response: &RetrieveResponse,
) -> io::Result<()>
where
    W: Write,
{
    let summary = RetrieveDebugSummary::from_response(response);
    serde_json::to_writer(&mut *writer, &summary).map_err(io::Error::other)?;
    writeln!(writer)
}

pub fn write_retrieve_snippets<W>(writer: &mut W, response: &RetrieveResponse) -> io::Result<()>
where
    W: Write,
{
    for row in retrieve_display_rows(response) {
        writeln!(writer, "{} {}", row.citation, row.snippet)?;
    }
    Ok(())
}

pub fn write_retrieve_tsv<W>(writer: &mut W, response: &RetrieveResponse) -> io::Result<()>
where
    W: Write,
{
    write_retrieve_delimited(writer, response, b'\t')
}

pub fn write_retrieve_csv<W>(writer: &mut W, response: &RetrieveResponse) -> io::Result<()>
where
    W: Write,
{
    write_retrieve_delimited(writer, response, b',')
}

fn write_retrieve_delimited<W>(
    writer: &mut W,
    response: &RetrieveResponse,
    delimiter: u8,
) -> io::Result<()>
where
    W: Write,
{
    write_delimited_record(
        writer,
        delimiter,
        [
            "rank",
            "score",
            "citation",
            "collection",
            "source",
            "locator",
            "snippet",
        ],
    )?;
    for row in retrieve_display_rows(response) {
        write_delimited_record(
            writer,
            delimiter,
            [
                row.rank.to_string(),
                format_score(row.score),
                row.citation,
                row.collection,
                row.source,
                row.locator,
                row.snippet.to_string(),
            ],
        )?;
    }
    Ok(())
}

const RETRIEVE_DEBUG_SUMMARY_LIMIT: usize = 5;

#[derive(Debug, Serialize)]
struct RetrieveDebugSummary {
    kind: &'static str,
    task_id: String,
    debug_available: bool,
    timing_ms: RetrieveDebugTimingSummary,
    counts: RetrieveDebugCountSummary,
    reranker: RetrieveDebugRerankerSummary,
    top_candidates: RetrieveDebugTopCandidates,
}

impl RetrieveDebugSummary {
    fn from_response(response: &RetrieveResponse) -> Self {
        let debug = response.debug.as_ref();
        Self {
            kind: "retrieval_debug_summary",
            task_id: response.task_id.clone(),
            debug_available: debug.is_some(),
            timing_ms: RetrieveDebugTimingSummary::from_response(response, debug),
            counts: RetrieveDebugCountSummary::from_debug(debug),
            reranker: RetrieveDebugRerankerSummary::from_debug(debug),
            top_candidates: RetrieveDebugTopCandidates::from_debug(debug),
        }
    }
}

#[derive(Debug, Serialize)]
struct RetrieveDebugTimingSummary {
    retrieval_ms: Option<u64>,
    rerank_ms: Option<u64>,
    total_ms: Option<u64>,
}

impl RetrieveDebugTimingSummary {
    fn from_response(response: &RetrieveResponse, debug: Option<&RetrievalDebug>) -> Self {
        let retrieval_ms = response_timing_ms(response, "retrieval");
        let rerank_ms = response_timing_ms(response, "rerank")
            .or_else(|| debug.and_then(|debug| debug.reranker.latency_ms));
        let total_ms = response_timing_ms(response, "total").or_else(|| {
            let total = response
                .timings
                .iter()
                .map(|timing| timing.duration_ms)
                .sum::<u64>();
            (total > 0).then_some(total)
        });
        Self {
            retrieval_ms,
            rerank_ms,
            total_ms,
        }
    }
}

#[derive(Debug, Serialize)]
struct RetrieveDebugCountSummary {
    bm25_hits: usize,
    dense_hits: usize,
    rrf_fused: usize,
    rerank_input: usize,
    final_evidence: usize,
}

impl RetrieveDebugCountSummary {
    fn from_debug(debug: Option<&RetrievalDebug>) -> Self {
        let Some(debug) = debug else {
            return Self {
                bm25_hits: 0,
                dense_hits: 0,
                rrf_fused: 0,
                rerank_input: 0,
                final_evidence: 0,
            };
        };
        Self {
            bm25_hits: debug.bm25_hits.len(),
            dense_hits: debug.dense_hits.len(),
            rrf_fused: debug.rrf_fused_hits.len(),
            rerank_input: rerank_input_count(debug),
            final_evidence: debug.final_evidence_pack.len(),
        }
    }
}

#[derive(Debug, Serialize)]
struct RetrieveDebugRerankerSummary {
    status: Option<&'static str>,
    reason: Option<String>,
}

impl RetrieveDebugRerankerSummary {
    fn from_debug(debug: Option<&RetrievalDebug>) -> Self {
        let Some(debug) = debug else {
            return Self {
                status: None,
                reason: None,
            };
        };
        Self {
            status: Some(reranker_status_name(debug.reranker.status)),
            reason: debug.reranker.reason.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct RetrieveDebugTopCandidates {
    bm25_hits: Vec<RetrieveDebugCandidateSummary>,
    dense_hits: Vec<RetrieveDebugCandidateSummary>,
    rrf_fused: Vec<RetrieveDebugCandidateSummary>,
    rerank_input: Vec<RetrieveDebugCandidateSummary>,
    final_evidence: Vec<RetrieveDebugCandidateSummary>,
}

impl RetrieveDebugTopCandidates {
    fn from_debug(debug: Option<&RetrievalDebug>) -> Self {
        let Some(debug) = debug else {
            return Self {
                bm25_hits: Vec::new(),
                dense_hits: Vec::new(),
                rrf_fused: Vec::new(),
                rerank_input: Vec::new(),
                final_evidence: Vec::new(),
            };
        };
        Self {
            bm25_hits: top_stage_candidates(&debug.bm25_hits),
            dense_hits: top_stage_candidates(&debug.dense_hits),
            rrf_fused: top_fused_candidates(&debug.rrf_fused_hits),
            rerank_input: top_rerank_input_candidates(debug),
            final_evidence: top_final_evidence_candidates(&debug.final_evidence_pack),
        }
    }
}

#[derive(Debug, Serialize)]
struct RetrieveDebugCandidateSummary {
    chunk_id: String,
    source: Option<String>,
    score: f32,
}

fn response_timing_ms(response: &RetrieveResponse, phase: &str) -> Option<u64> {
    response
        .timings
        .iter()
        .find(|timing| timing.phase == phase)
        .map(|timing| timing.duration_ms)
}

fn rerank_input_count(debug: &RetrievalDebug) -> usize {
    debug
        .reranker
        .candidate_count
        .or_else(|| {
            debug
                .reranker
                .request
                .as_ref()
                .map(|request| request.candidate_count)
        })
        .unwrap_or({
            if debug.reranker.scores.is_empty() {
                0
            } else {
                debug.reranker.scores.len()
            }
        })
}

fn reranker_status_name(status: RetrievalRerankStatus) -> &'static str {
    match status {
        RetrievalRerankStatus::Disabled => "disabled",
        RetrievalRerankStatus::Skipped => "skipped",
        RetrievalRerankStatus::Succeeded => "succeeded",
        RetrievalRerankStatus::Fallback => "fallback",
    }
}

fn top_stage_candidates(hits: &[RetrievalStageHit]) -> Vec<RetrieveDebugCandidateSummary> {
    hits.iter()
        .take(RETRIEVE_DEBUG_SUMMARY_LIMIT)
        .map(|hit| RetrieveDebugCandidateSummary {
            chunk_id: hit.chunk_id.0.clone(),
            source: hit.source_id.as_ref().map(|source_id| source_id.0.clone()),
            score: hit.score,
        })
        .collect()
}

fn top_fused_candidates(hits: &[RetrievalFusedHit]) -> Vec<RetrieveDebugCandidateSummary> {
    hits.iter()
        .take(RETRIEVE_DEBUG_SUMMARY_LIMIT)
        .map(|hit| RetrieveDebugCandidateSummary {
            chunk_id: hit.chunk_id.0.clone(),
            source: hit.source_id.as_ref().map(|source_id| source_id.0.clone()),
            score: hit.score,
        })
        .collect()
}

fn top_rerank_input_candidates(debug: &RetrievalDebug) -> Vec<RetrieveDebugCandidateSummary> {
    debug
        .rrf_fused_hits
        .iter()
        .take(RETRIEVE_DEBUG_SUMMARY_LIMIT.min(rerank_input_count(debug)))
        .map(|hit| RetrieveDebugCandidateSummary {
            chunk_id: hit.chunk_id.0.clone(),
            source: hit.source_id.as_ref().map(|source_id| source_id.0.clone()),
            score: hit.score,
        })
        .collect()
}

fn top_final_evidence_candidates(
    items: &[RetrievalEvidencePackEntry],
) -> Vec<RetrieveDebugCandidateSummary> {
    items
        .iter()
        .take(RETRIEVE_DEBUG_SUMMARY_LIMIT)
        .map(|item| RetrieveDebugCandidateSummary {
            chunk_id: item.chunk_id.0.clone(),
            source: Some(item.source_id.0.clone()),
            score: item.score,
        })
        .collect()
}

pub fn write_retrieve_json<W>(writer: &mut W, response: &RetrieveResponse) -> io::Result<()>
where
    W: Write,
{
    serde_json::to_writer_pretty(&mut *writer, response).map_err(io::Error::other)?;
    writeln!(writer)
}

pub fn write_retrieve_json_without_debug<W>(
    writer: &mut W,
    response: &RetrieveResponse,
) -> io::Result<()>
where
    W: Write,
{
    let mut response = response.clone();
    response.debug = None;
    write_retrieve_json(writer, &response)
}

#[derive(Debug, Clone, PartialEq)]
struct RetrieveDisplayRow<'a> {
    rank: usize,
    score: f32,
    citation: String,
    collection: String,
    source: String,
    locator: String,
    snippet: &'a str,
}

fn retrieve_display_rows(response: &RetrieveResponse) -> Vec<RetrieveDisplayRow<'_>> {
    response
        .results
        .iter()
        .map(|result| {
            let locator = compact_locator(result);
            RetrieveDisplayRow {
                rank: result.rank,
                score: result.score,
                citation: format!("[{locator}]"),
                collection: display_collection_names(&result.collections),
                source: display_source(result, &locator),
                locator,
                snippet: &result.snippet,
            }
        })
        .collect()
}

fn write_indented_snippet<W>(writer: &mut W, snippet: &str, indent: &str) -> io::Result<()>
where
    W: Write,
{
    if snippet.is_empty() {
        writeln!(writer, "{indent}")?;
        return Ok(());
    }
    for line in snippet.lines() {
        writeln!(writer, "{indent}{line}")?;
    }
    Ok(())
}

fn compact_locator(result: &verbatim_core::api::RetrieveResultResponse) -> String {
    if let Some(locator) = &result.structured_locator {
        return compact_structured_locator(locator);
    }

    compact_raw_locator(&result.locator, result.source_path.as_deref())
}

fn compact_structured_locator(locator: &SourceLocator) -> String {
    match locator {
        SourceLocator::Document {
            path_or_url,
            line_start,
            line_end: Some(end),
        } => format!("{} L{line_start}-{end}", compact_path_or_url(path_or_url)),
        SourceLocator::Document {
            path_or_url,
            line_start,
            line_end: None,
        } => format!("{} L{line_start}", compact_path_or_url(path_or_url)),
        SourceLocator::Markdown {
            path,
            line_start,
            line_end,
            block_kind,
            heading_slug,
            ..
        } => {
            let path = compact_path_or_url(path);
            let line = if line_start == line_end {
                format!("L{line_start}")
            } else {
                format!("L{line_start}-{line_end}")
            };
            let heading = heading_slug
                .as_ref()
                .map(|slug| format!(" #{slug}"))
                .unwrap_or_default();
            format!("{path} {line} markdown:{}{heading}", block_kind.as_str())
        }
        SourceLocator::Pdf { .. }
        | SourceLocator::PdfOcr { .. }
        | SourceLocator::PdfImage { .. } => locator.to_string(),
    }
}

fn compact_raw_locator(locator: &str, source_path: Option<&str>) -> String {
    let mut compact = locator.to_string();
    if let Some(source_path) = source_path {
        compact = compact.replace(source_path, &compact_path_or_url(source_path));
    }

    let Some((first, rest)) = compact.split_once(|character: char| character.is_whitespace())
    else {
        return compact_path_or_url(&compact);
    };

    if first.contains('/') || first.contains('\\') {
        format!("{} {rest}", compact_path_or_url(first))
    } else {
        compact
    }
}

fn compact_path_or_url(value: &str) -> String {
    if value.starts_with("http://") || value.starts_with("https://") {
        return value.to_string();
    }

    value
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .or_else(|| {
            Path::new(value)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
        })
        .unwrap_or(value)
        .to_string()
}

fn display_collection_names(collections: &[CollectionResultProvenance]) -> String {
    let mut names = Vec::new();
    for collection in collections {
        if !names.contains(&collection.name.as_str()) {
            names.push(collection.name.as_str());
        }
    }
    names.join(",")
}

fn display_source(result: &verbatim_core::api::RetrieveResultResponse, locator: &str) -> String {
    if let Some(collection) = result.collections.first() {
        return collection.logical_path.clone();
    }

    if let Some(source_path) = &result.source_path {
        return compact_path_or_url(source_path);
    }

    locator
        .split_whitespace()
        .next()
        .filter(|source| !source.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn format_score(score: f32) -> String {
    format!("{score:.4}")
}

fn write_delimited_record<W, I, S>(writer: &mut W, delimiter: u8, fields: I) -> io::Result<()>
where
    W: Write,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let delimiter = char::from(delimiter);
    let mut first = true;
    for field in fields {
        if first {
            first = false;
        } else {
            write!(writer, "{delimiter}")?;
        }
        write_delimited_field(writer, field.as_ref(), delimiter)?;
    }
    writeln!(writer)
}

fn write_delimited_field<W>(writer: &mut W, field: &str, delimiter: char) -> io::Result<()>
where
    W: Write,
{
    if field
        .chars()
        .any(|character| matches!(character, '"' | '\n' | '\r') || character == delimiter)
    {
        write!(writer, "\"")?;
        for character in field.chars() {
            if character == '"' {
                write!(writer, "\"\"")?;
            } else {
                write!(writer, "{character}")?;
            }
        }
        write!(writer, "\"")
    } else {
        write!(writer, "{field}")
    }
}

pub fn write_citations<W>(writer: &mut W, citations: &[CitationResponse]) -> std::io::Result<()>
where
    W: Write,
{
    if citations.is_empty() {
        return Ok(());
    }

    writeln!(writer)?;
    writeln!(writer, "Citations:")?;
    for citation in citations {
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
        write_collection_provenance(writer, &citation.collections, "      ")?;
    }
    Ok(())
}

fn write_collection_filter_summary<W>(
    writer: &mut W,
    filter: &CollectionFilterResponse,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "  collections: stale={} union_sources={}",
        filter.stale, filter.union_source_count
    )?;
    for collection in &filter.applied {
        writeln!(
            writer,
            "    - {} members={} indexed={} stale_members={} last_synced={}",
            collection.name,
            collection.member_count,
            collection.indexed_member_count,
            collection.stale_member_count,
            collection.last_synced_at.as_deref().unwrap_or("never")
        )?;
    }
    for warning in &filter.warnings {
        writeln!(writer, "    warning: {warning}")?;
    }
    Ok(())
}

fn write_collection_provenance<W>(
    writer: &mut W,
    collections: &[CollectionResultProvenance],
    indent: &str,
) -> std::io::Result<()>
where
    W: Write,
{
    for collection in collections {
        writeln!(
            writer,
            "{indent}collection: {} logical_path={} member_updated_at={}",
            collection.name, collection.logical_path, collection.member_updated_at
        )?;
    }
    Ok(())
}

pub fn write_retrieval_debug_typed<W>(writer: &mut W, debug: &RetrievalDebug) -> std::io::Result<()>
where
    W: Write,
{
    let value = serde_json::to_value(debug).map_err(io::Error::other)?;
    write_retrieval_debug(writer, &value)
}

pub fn write_retrieval_debug<W>(writer: &mut W, debug: &Value) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer)?;
    writeln!(writer, "Retrieval Debug")?;
    if let Some(path) = debug.get("dense_vector_path").and_then(Value::as_str) {
        writeln!(writer, "Dense vector path: {path}")?;
    }
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
    let provider = value_string(reranker, "provider");
    let model = value_string(reranker, "model");
    let top_n = value_string(reranker, "top_n");
    let candidate_count = value_string(reranker, "candidate_count");
    if !provider.is_empty() || !model.is_empty() || !top_n.is_empty() || !candidate_count.is_empty()
    {
        writeln!(
            writer,
            "  provider={} model={} top_n={} candidates={}",
            provider, model, top_n, candidate_count
        )?;
    }
    if let Some(capability) = reranker.get("capability") {
        let state = value_string(capability, "state");
        let reason = value_string(capability, "reason");
        let retried = value_string(capability, "retried_after_context_limit");
        writeln!(
            writer,
            "  capability state={} max_context_tokens={} max_candidates={} max_documents={} max_document_chars={} max_payload_chars={} retried_after_limit={} reason={}",
            state,
            value_string(capability, "max_context_tokens"),
            value_string(capability, "max_candidates"),
            value_string(capability, "max_documents"),
            value_string(capability, "max_document_chars"),
            value_string(capability, "max_payload_chars"),
            retried,
            reason
        )?;
    }
    if let Some(request) = reranker.get("request") {
        writeln!(
            writer,
            "  request candidates={} document_char_limit={} top_n={}",
            value_string(request, "candidate_count"),
            value_string(request, "document_char_limit"),
            value_string(request, "top_n")
        )?;
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

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid-json>".into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use verbatim_core::api::{
        CollectionResultProvenance, EvidenceResponse, RetrieveControlsResponse, RetrieveResponse,
        RetrieveResultResponse, RetrieveTimingResponse,
    };
    use verbatim_core::task::{TaskId, TaskKind, TaskStatus};
    use verbatim_core::types::{
        MarkdownBlockKind, MarkdownHeadingLocator, OcrLocatorMetadata, OcrProfile,
    };

    #[test]
    fn task_summary_renders_embedding_cache_stats_from_result_metadata() {
        let task = TaskSummary {
            id: TaskId("task-1".into()),
            kind: TaskKind::Ingest,
            status: TaskStatus::Succeeded,
            created_at: "1".into(),
            updated_at: "2".into(),
            started_at: Some("1".into()),
            finished_at: Some("2".into()),
            request: serde_json::json!({"operation": "reindex"}),
            result: Some(serde_json::json!({
                "reindexed": 1,
                "embedding_cache": {
                    "cache_hits": 2,
                    "cache_misses": 1,
                    "embedded_chunks": 1,
                    "reused_chunks": 2,
                    "changed_chunks": 1
                }
            })),
            error: None,
            queue_position: None,
            blocking_reason: None,
            progress: None,
        };
        let mut output = Vec::new();

        write_task_summary(&mut output, &task, &[]).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("embedding_cache: hits=2 misses=1 embedded=1 reused=2 changed=1"));
        assert!(output.contains("\"embedding_cache\""));
    }

    #[test]
    fn reranker_debug_renders_sanitized_capability_and_request_state() {
        let reranker = serde_json::json!({
            "status": "fallback",
            "reason": "http_status_400",
            "provider": "vllm",
            "model": "rerank-model",
            "top_n": 2,
            "candidate_count": 4,
            "capability": {
                "state": "refreshed",
                "max_context_tokens": 512,
                "max_candidates": 2,
                "max_documents": 2,
                "max_document_chars": 1024,
                "max_payload_chars": 4096,
                "retried_after_context_limit": true
            },
            "request": {
                "candidate_count": 2,
                "document_char_limit": 1024,
                "top_n": 2
            },
            "scores": []
        });
        let mut output = Vec::new();

        write_reranker(&mut output, Some(&reranker)).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("capability state=refreshed"));
        assert!(output.contains("max_context_tokens=512"));
        assert!(output.contains("max_document_chars=1024"));
        assert!(output.contains("request candidates=2 document_char_limit=1024 top_n=2"));
    }

    #[test]
    fn ocr_evidence_lookup_renders_structured_locator_details() {
        let profile = OcrProfile {
            provider: "test".into(),
            engine: "mock-ocr".into(),
            engine_version: Some("1.0".into()),
            language: "eng".into(),
            profile: "default".into(),
        };
        let response = EvidenceResponse {
            id: "ev-ocr".into(),
            source_id: "src-1".into(),
            kind: "ocr".into(),
            derived_from: None,
            locator: "PDF p.1, OCR line 1, conf=0.97".into(),
            structured_locator: SourceLocator::PdfOcr {
                page: 1,
                page_label: Some("1".into()),
                line_index: 1,
                word_index: None,
                bbox: Some(BBox {
                    x0: 10.0,
                    y0: 20.0,
                    x1: 120.0,
                    y1: 36.0,
                }),
                ocr: Box::new(OcrLocatorMetadata {
                    profile,
                    profile_hash: "profile-hash".into(),
                    confidence: Some(0.97),
                    text_hash: "text-hash".into(),
                }),
            },
            text: "ocrneedle scanned invoice total".into(),
            heading_path: Vec::new(),
            position: 1,
            image_artifact: None,
        };
        let mut output = Vec::new();

        write_evidence(&mut output, &response).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("kind: ocr"));
        assert!(output.contains("ocr_locator:"));
        assert!(output.contains("engine: mock-ocr"));
        assert!(output.contains("language: eng"));
        assert!(output.contains("confidence: 0.97"));
        assert!(output.contains("bbox: [10.00,20.00,120.00,36.00]"));
    }

    #[test]
    fn markdown_evidence_lookup_renders_structured_locator_details() {
        let response = EvidenceResponse {
            id: "ev-md".into(),
            source_id: "src-1".into(),
            kind: "text".into(),
            derived_from: None,
            locator: "/tmp/doc.md L3 markdown:paragraph #intro".into(),
            structured_locator: SourceLocator::Markdown {
                path: "/tmp/doc.md".into(),
                line_start: 3,
                line_end: 4,
                byte_start: 24,
                byte_end: 86,
                block_kind: MarkdownBlockKind::Paragraph,
                block_index: 2,
                block_hash: "block-hash".into(),
                heading_level: Some(1),
                heading_slug: Some("intro".into()),
                heading_path: vec![MarkdownHeadingLocator {
                    level: 1,
                    text: "Intro".into(),
                    slug: "intro".into(),
                    line: 1,
                }],
            },
            text: "markdown evidence text".into(),
            heading_path: vec!["Intro".into()],
            position: 2,
            image_artifact: None,
        };
        let mut output = Vec::new();

        write_evidence(&mut output, &response).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("markdown_locator:"));
        assert!(output.contains("block_kind: paragraph"));
        assert!(output.contains("line_range: 3-4"));
        assert!(output.contains("byte_range: 24-86"));
        assert!(output.contains("block_hash: block-hash"));
        assert!(output.contains("heading_slug: intro"));
        assert!(output.contains("heading: level=1 line=1 slug=intro text=Intro"));
    }

    #[test]
    fn retrieve_debug_output_renders_markdown_structured_locator_when_present() {
        let response = RetrieveResponse {
            task_id: "task-1".into(),
            query: "markdown".into(),
            source_id: None,
            collection_filter: None,
            embedding_profile_id: "default".into(),
            limit: 1,
            page_size: 1,
            page: 1,
            total_results: 1,
            returned_results: 1,
            controls: RetrieveControlsResponse {
                fast: false,
                rerank_enabled: false,
                dense_top_k: 10,
                bm25_top_k: 10,
                rrf_k: 60,
                rerank_top_n: 1,
            },
            timings: Vec::new(),
            results: vec![RetrieveResultResponse {
                index: 0,
                rank: 1,
                label: "E1".into(),
                evidence_id: "ev-md".into(),
                source_id: "src-1".into(),
                source_path: Some("/tmp/doc.md".into()),
                collections: Vec::new(),
                chunk_id: "chunk-1".into(),
                kind: "text".into(),
                role: "original_text".into(),
                score: 0.42,
                locator: "/tmp/doc.md L3 markdown:paragraph #intro".into(),
                structured_locator: Some(SourceLocator::Markdown {
                    path: "/tmp/doc.md".into(),
                    line_start: 3,
                    line_end: 3,
                    byte_start: 24,
                    byte_end: 48,
                    block_kind: MarkdownBlockKind::Paragraph,
                    block_index: 0,
                    block_hash: "block-hash".into(),
                    heading_level: Some(1),
                    heading_slug: Some("intro".into()),
                    heading_path: vec![MarkdownHeadingLocator {
                        level: 1,
                        text: "Intro".into(),
                        slug: "intro".into(),
                        line: 1,
                    }],
                }),
                provenance: None,
                derived_from: None,
                snippet: "markdown evidence".into(),
            }],
            debug: None,
        };
        let mut output = Vec::new();

        write_retrieve_debug_response(&mut output, &response).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("locator: /tmp/doc.md L3 markdown:paragraph #intro"));
        assert!(output.contains("markdown_locator:"));
        assert!(output.contains("block_hash: block-hash"));
    }

    #[test]
    fn retrieve_compact_markdown_omits_debug_metadata_and_paths() {
        let response = retrieve_display_fixture();
        let mut output = Vec::new();

        write_retrieve_response(&mut output, &response).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("3. score=0.9876 [doc.md L10 markdown:paragraph #intro]"));
        assert!(output.contains("   alpha\tbeta"));
        assert!(output.contains("   gamma, \"delta\" 中文"));
        assert_low_noise_retrieve_output(&output);
    }

    #[test]
    fn retrieve_snippets_omits_headers_scores_and_debug_metadata() {
        let response = retrieve_display_fixture();
        let mut output = Vec::new();

        write_retrieve_snippets(&mut output, &response).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert_eq!(
            output,
            "[doc.md L10 markdown:paragraph #intro] alpha\tbeta\ngamma, \"delta\" 中文\n"
        );
        assert!(!output.contains("score="));
        assert_low_noise_retrieve_output(&output);
    }

    #[test]
    fn retrieve_tsv_round_trips_special_characters_with_fixed_columns() {
        let response = retrieve_display_fixture();
        let mut output = Vec::new();

        write_retrieve_tsv(&mut output, &response).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_low_noise_retrieve_output(&output);

        let mut records = parse_delimited_records(&output, '\t');
        let headers = records.remove(0);
        assert_eq!(
            headers,
            vec![
                "rank",
                "score",
                "citation",
                "collection",
                "source",
                "locator",
                "snippet"
            ]
        );
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.len(), 7);
        assert_eq!(record[0], "3");
        assert_eq!(record[1], "0.9876");
        assert_eq!(record[2], "[doc.md L10 markdown:paragraph #intro]");
        assert_eq!(record[3], "articles");
        assert_eq!(record[4], "nested/doc.md");
        assert_eq!(record[5], "doc.md L10 markdown:paragraph #intro");
        assert_eq!(record[6], "alpha\tbeta\ngamma, \"delta\" 中文");
    }

    #[test]
    fn retrieve_csv_round_trips_special_characters_with_fixed_columns() {
        let response = retrieve_display_fixture();
        let mut output = Vec::new();

        write_retrieve_csv(&mut output, &response).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_low_noise_retrieve_output(&output);

        let mut records = parse_delimited_records(&output, ',');
        let headers = records.remove(0);
        assert_eq!(
            headers,
            vec![
                "rank",
                "score",
                "citation",
                "collection",
                "source",
                "locator",
                "snippet"
            ]
        );
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.len(), 7);
        assert_eq!(record[0], "3");
        assert_eq!(record[1], "0.9876");
        assert_eq!(record[2], "[doc.md L10 markdown:paragraph #intro]");
        assert_eq!(record[3], "articles");
        assert_eq!(record[4], "nested/doc.md");
        assert_eq!(record[5], "doc.md L10 markdown:paragraph #intro");
        assert_eq!(record[6], "alpha\tbeta\ngamma, \"delta\" 中文");
    }

    fn retrieve_display_fixture() -> RetrieveResponse {
        RetrieveResponse {
            task_id: "task-internal-123".into(),
            query: "fixture query".into(),
            source_id: Some("src-internal-123".into()),
            collection_filter: None,
            embedding_profile_id: "default".into(),
            limit: 10,
            page_size: 10,
            page: 1,
            total_results: 1,
            returned_results: 1,
            controls: RetrieveControlsResponse {
                fast: true,
                rerank_enabled: true,
                dense_top_k: 80,
                bm25_top_k: 50,
                rrf_k: 60,
                rerank_top_n: 12,
            },
            timings: vec![RetrieveTimingResponse {
                phase: "retrieval".into(),
                duration_ms: 1234,
            }],
            results: vec![RetrieveResultResponse {
                index: 2,
                rank: 3,
                label: "E3".into(),
                evidence_id: "internal-ev-abc123".into(),
                source_id: "src-internal-123".into(),
                source_path: Some("/home/obj/private/docs/doc.md".into()),
                collections: vec![CollectionResultProvenance {
                    collection_id: "collection-internal-123".into(),
                    name: "articles".into(),
                    logical_path: "nested/doc.md".into(),
                    source_path: "/home/obj/private/docs/doc.md".into(),
                    member_updated_at: "1782810157".into(),
                }],
                chunk_id: "chunk-internal-123".into(),
                kind: "text".into(),
                role: "original_text".into(),
                score: 0.9876,
                locator: "/home/obj/private/docs/doc.md L10 markdown:paragraph #intro".into(),
                structured_locator: None,
                provenance: None,
                derived_from: None,
                snippet: "alpha\tbeta\ngamma, \"delta\" 中文".into(),
            }],
            debug: None,
        }
    }

    fn assert_low_noise_retrieve_output(output: &str) {
        for forbidden in [
            "Context pack:",
            "controls:",
            "timing:",
            "source_path:",
            "evidence=",
            "role=",
            "kind=",
            "member_updated_at=",
            "task-internal-123",
            "internal-ev-abc123",
            "src-internal-123",
            "collection-internal-123",
            "chunk-internal-123",
            "/home/obj/private",
        ] {
            assert!(
                !output.contains(forbidden),
                "output unexpectedly contained {forbidden:?}: {output}"
            );
        }
    }

    fn parse_delimited_records(input: &str, delimiter: char) -> Vec<Vec<String>> {
        let mut records = Vec::new();
        let mut record = Vec::new();
        let mut field = String::new();
        let mut characters = input.chars().peekable();
        let mut in_quotes = false;

        while let Some(character) = characters.next() {
            if in_quotes {
                if character == '"' {
                    if characters.peek() == Some(&'"') {
                        field.push('"');
                        characters.next();
                    } else {
                        in_quotes = false;
                    }
                } else {
                    field.push(character);
                }
                continue;
            }

            if character == '"' {
                in_quotes = true;
            } else if character == delimiter {
                record.push(std::mem::take(&mut field));
            } else if character == '\n' {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            } else if character == '\r' {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            } else {
                field.push(character);
            }
        }

        if !field.is_empty() || !record.is_empty() {
            record.push(field);
            records.push(record);
        }

        records
    }
}
