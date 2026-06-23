use std::io::{self, Write};

use serde_json::Value;
use verbatim_core::api::{
    AskResponse, CheckStaleResponse, CitationResponse, ConfigResponse, EvidenceResponse,
    HealthResponse, IngestResponse, ReindexResponse, RetrieveResponse, SourceResponse,
    TaskCreatedResponse,
};
use verbatim_core::task::{TaskEvent, TaskProgressSnapshot, TaskSpan, TaskStatus, TaskSummary};
use verbatim_core::types::{BBox, OcrSourceStatus, RetrievalDebug, SourceLocator};

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

pub fn write_reindex<W>(writer: &mut W, response: &ReindexResponse) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Reindexed {} source(s).", response.reindexed)
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
    if let SourceLocator::PdfOcr {
        page_label,
        line_index,
        word_index,
        bbox,
        ocr,
        ..
    } = locator
    {
        writeln!(writer, "  ocr_locator:")?;
        if let Some(page_label) = page_label {
            writeln!(writer, "    page_label: {page_label}")?;
        }
        writeln!(writer, "    line_index: {line_index}")?;
        if let Some(word_index) = word_index {
            writeln!(writer, "    word_index: {word_index}")?;
        }
        if let Some(bbox) = bbox {
            writeln!(writer, "    bbox: {}", display_bbox(bbox))?;
        }
        writeln!(writer, "    engine: {}", ocr.profile.engine)?;
        if let Some(version) = &ocr.profile.engine_version {
            writeln!(writer, "    engine_version: {version}")?;
        }
        writeln!(writer, "    language: {}", ocr.profile.language)?;
        writeln!(writer, "    profile: {}", ocr.profile.profile)?;
        writeln!(writer, "    profile_hash: {}", ocr.profile_hash)?;
        if let Some(confidence) = ocr.confidence {
            writeln!(writer, "    confidence: {confidence:.2}")?;
        }
        writeln!(writer, "    text_hash: {}", ocr.text_hash)?;
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
    writeln!(writer, "Daemon status: {}", health.status)
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
            writeln!(writer, "      locator: {}", result.locator)?;
            writeln!(writer, "      snippet: {}", result.snippet)?;
        }
    }

    if let Some(debug) = &response.debug {
        write_retrieval_debug_typed(writer, debug)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use verbatim_core::api::EvidenceResponse;
    use verbatim_core::types::{OcrLocatorMetadata, OcrProfile};

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
}
