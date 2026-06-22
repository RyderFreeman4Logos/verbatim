use std::io::{self, Write};

use serde_json::Value;
use verbatim_core::api::{
    AskResponse, CheckStaleResponse, CitationResponse, ConfigResponse, EvidenceResponse,
    HealthResponse, IngestResponse, RetrieveResponse, SourceResponse, TaskCreatedResponse,
};
use verbatim_core::task::{TaskEvent, TaskSpan, TaskStatus, TaskSummary};
use verbatim_core::types::RetrievalDebug;

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
    Ok(())
}

pub fn write_check_stale<W>(writer: &mut W, response: &CheckStaleResponse) -> std::io::Result<()>
where
    W: Write,
{
    if response.stale.is_empty() {
        return writeln!(writer, "No stale sources.");
    }
    writeln!(writer, "Stale sources:")?;
    for id in &response.stale {
        writeln!(writer, "  {id}")?;
    }
    Ok(())
}

pub fn write_ingest<W>(writer: &mut W, response: &IngestResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Ingested {} source(s).", response.ingested)
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
    writeln!(writer, "Daemon status: {}", health.status)
}

pub fn write_ask_response<W>(writer: &mut W, response: &AskResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "{}", response.answer)?;
    write_citations(writer, &response.citations)?;

    if let Some(debug) = &response.retrieval {
        write_retrieval_debug_typed(writer, debug)?;
    }

    Ok(())
}

pub fn write_retrieve_response<W>(
    writer: &mut W,
    response: &RetrieveResponse,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Context pack: {}", response.task_id)?;
    writeln!(writer, "  query: {}", response.query)?;
    if let Some(source_id) = &response.source_id {
        writeln!(writer, "  source_id: {source_id}")?;
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
        return writeln!(writer, "No retrieval results on this page.");
    }

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
        writeln!(writer, "      locator: {}", result.locator)?;
        writeln!(writer, "      snippet: {}", result.snippet)?;
    }

    Ok(())
}

pub fn write_retrieve_json<W>(writer: &mut W, response: &RetrieveResponse) -> std::io::Result<()>
where
    W: Write,
{
    serde_json::to_writer_pretty(&mut *writer, response).map_err(io::Error::other)?;
    writeln!(writer)
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
