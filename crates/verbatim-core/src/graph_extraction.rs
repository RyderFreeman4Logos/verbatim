use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::config::{ChatConfig, GraphExtractionConfig};
use crate::provider::openai_compatible::OpenAiCompatibleChatModel;
use crate::provider::{ChatMessage, ChatModel, ChatRequest};
use crate::types::{
    Chunk, ChunkId, ChunkType, EdgeType, GraphEdge, GraphEdgeId, GraphNode, GraphNodeId,
    GraphNodeKind, Source, SourceId,
};

pub const GRAPH_EXTRACTION_PROMPT_VERSION: &str = "llm-graph-extraction-v1";

const HARD_MAX_CHUNKS: usize = 16;
const HARD_MAX_CHUNK_CHARS: usize = 4_000;
const HARD_MAX_ENTITIES: usize = 64;
const HARD_MAX_RELATIONSHIPS: usize = 96;
const HARD_MAX_CLAIMS: usize = 96;
const HARD_MAX_RETRIES: usize = 2;
const HARD_MAX_OUTPUT_TOKENS: u32 = 4_096;
const HARD_MAX_RESPONSE_CHARS: usize = 64 * 1024;
const HARD_MAX_ERROR_CHARS: usize = 512;
const HARD_MAX_SPANS_PER_ITEM: usize = 8;
const MAX_NAME_CHARS: usize = 160;
const MAX_DESCRIPTION_CHARS: usize = 512;
const MAX_CLAIM_CHARS: usize = 1_000;

/// Bounded graph extraction over selected ingest chunks.
pub struct GraphExtractor {
    chat_model: Arc<dyn ChatModel>,
}

impl GraphExtractor {
    pub fn from_config(config: &ChatConfig) -> Self {
        Self {
            chat_model: Arc::new(OpenAiCompatibleChatModel::from_config(config)),
        }
    }

    pub fn from_chat_model(chat_model: Arc<dyn ChatModel>) -> Self {
        Self { chat_model }
    }

    pub async fn extract(
        &self,
        source: &Source,
        chunks: &[Chunk],
        config: &GraphExtractionConfig,
    ) -> Result<GraphExtractionOutcome> {
        let limits = EffectiveExtractionLimits::from_config(config);
        if !config.enabled || limits.max_chunks == 0 || limits.max_chunk_chars == 0 {
            return Ok(GraphExtractionOutcome::default());
        }

        let selected_chunks = select_prompt_chunks(&source.id, chunks, &limits);
        if selected_chunks.is_empty() {
            return Ok(GraphExtractionOutcome::default());
        }

        let system_prompt = extraction_system_prompt();
        let initial_user_prompt = build_initial_user_prompt(&selected_chunks, &limits);
        let mut prior_response = String::new();
        let mut prior_error = String::new();
        let mut response_was_truncated = false;

        for attempt_index in 0..=limits.max_retries {
            let user_prompt = if attempt_index == 0 {
                initial_user_prompt.clone()
            } else {
                build_repair_user_prompt(
                    &initial_user_prompt,
                    &prior_response,
                    &prior_error,
                    limits.max_error_chars,
                )
            };
            let request = ChatRequest::new(vec![
                ChatMessage::system(system_prompt),
                ChatMessage::user(user_prompt),
            ])
            .with_temperature(0.0)
            .with_max_tokens(limits.max_output_tokens);

            let response = self
                .chat_model
                .chat(request)
                .await
                .context("llm graph extraction provider failed")?;
            let bounded_response =
                BoundedResponse::new(response.content, limits.max_response_chars);
            response_was_truncated |= bounded_response.truncated;
            if bounded_response.truncated {
                prior_response = bounded_response.text;
                prior_error = "model response exceeded graph extraction response limit".to_string();
                continue;
            }

            match parse_extraction_json(&bounded_response.text) {
                Ok(raw) => {
                    let mut outcome = build_generated_graph(source, &selected_chunks, raw, &limits);
                    outcome.stats.selected_chunk_count = selected_chunks.len();
                    outcome.stats.attempt_count = attempt_index + 1;
                    outcome.stats.response_truncated = response_was_truncated;
                    return Ok(outcome);
                }
                Err(err) => {
                    prior_response = bounded_response.text;
                    prior_error = bounded_text(&err.to_string(), limits.max_error_chars);
                }
            }
        }

        bail!(
            "llm graph extraction returned invalid JSON after {} attempt(s): {}",
            limits.max_retries + 1,
            bounded_text(&prior_error, limits.max_error_chars)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphExtractionOutcome {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub stats: GraphExtractionStats,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphExtractionStats {
    pub selected_chunk_count: usize,
    pub entity_count: usize,
    pub relationship_count: usize,
    pub claim_count: usize,
    pub dropped_item_count: usize,
    pub attempt_count: usize,
    pub response_truncated: bool,
}

#[derive(Debug, Clone, Copy)]
struct EffectiveExtractionLimits {
    max_chunks: usize,
    max_chunk_chars: usize,
    max_entities: usize,
    max_relationships: usize,
    max_claims: usize,
    max_retries: usize,
    max_output_tokens: u32,
    max_response_chars: usize,
    max_error_chars: usize,
}

impl EffectiveExtractionLimits {
    fn from_config(config: &GraphExtractionConfig) -> Self {
        Self {
            max_chunks: config.max_chunks.min(HARD_MAX_CHUNKS),
            max_chunk_chars: config.max_chunk_chars.min(HARD_MAX_CHUNK_CHARS),
            max_entities: config.max_entities.min(HARD_MAX_ENTITIES),
            max_relationships: config.max_relationships.min(HARD_MAX_RELATIONSHIPS),
            max_claims: config.max_claims.min(HARD_MAX_CLAIMS),
            max_retries: config.max_retries.min(HARD_MAX_RETRIES),
            max_output_tokens: config.max_output_tokens.clamp(1, HARD_MAX_OUTPUT_TOKENS),
            max_response_chars: config.max_response_chars.min(HARD_MAX_RESPONSE_CHARS),
            max_error_chars: config.max_error_chars.min(HARD_MAX_ERROR_CHARS),
        }
    }
}

#[derive(Debug, Clone)]
struct PromptChunk {
    id: ChunkId,
    heading_path: Vec<String>,
    prompt_lines: String,
    valid_line_count: u32,
}

#[derive(Debug)]
struct BoundedResponse {
    text: String,
    truncated: bool,
}

impl BoundedResponse {
    fn new(text: String, max_chars: usize) -> Self {
        let truncated = text.chars().count() > max_chars;
        let text = bounded_text(&text, max_chars);
        Self { text, truncated }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtraction {
    entities: Vec<RawEntity>,
    relationships: Vec<RawRelationship>,
    claims: Vec<RawClaim>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntity {
    name: String,
    #[serde(rename = "type")]
    entity_type: String,
    description: String,
    source_spans: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRelationship {
    source: String,
    target: String,
    #[serde(rename = "type")]
    relationship_type: String,
    description: String,
    confidence: f64,
    source_spans: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClaim {
    claim: String,
    subject: String,
    predicate: String,
    object: String,
    source_spans: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedSourceSpan {
    chunk_id: ChunkId,
    line_start: u32,
    line_end: u32,
}

impl ValidatedSourceSpan {
    fn as_text(&self) -> String {
        format!("{}:{}-{}", self.chunk_id.0, self.line_start, self.line_end)
    }
}

#[derive(Debug)]
struct ValidatedEntity {
    name: String,
    entity_type: String,
    description: String,
    spans: Vec<ValidatedSourceSpan>,
}

#[derive(Debug)]
struct ValidatedRelationship {
    source: String,
    target: String,
    edge_type: EdgeType,
    relationship_type: String,
    description: String,
    confidence: f64,
    spans: Vec<ValidatedSourceSpan>,
}

#[derive(Debug)]
struct ValidatedClaim {
    claim: String,
    subject: String,
    predicate: String,
    object: String,
    spans: Vec<ValidatedSourceSpan>,
}

fn extraction_system_prompt() -> &'static str {
    "You are Verbatim's bounded graph extraction subsystem. Output only valid JSON. \
     Extract facts only from the provided chunk lines. Every entity, relationship, and claim \
     must include at least one source_spans entry formatted exactly as chunk_id:line_start-line_end. \
     Do not invent sources, line numbers, entities, relationships, or claims."
}

fn build_initial_user_prompt(
    selected_chunks: &[PromptChunk],
    limits: &EffectiveExtractionLimits,
) -> String {
    let mut prompt = format!(
        "Extract a provenance-first graph from these selected chunks.\n\n\
         Output JSON only with this exact schema:\n\
         {{\"entities\":[{{\"name\":\"...\",\"type\":\"feature|component|concept|file|function|issue|other\",\"description\":\"...\",\"source_spans\":[\"chunk_id:line_start-line_end\"]}}],\
         \"relationships\":[{{\"source\":\"...\",\"target\":\"...\",\"type\":\"depends_on|implements|mentions|conflicts_with|supports|other\",\"description\":\"...\",\"confidence\":0.0,\"source_spans\":[\"chunk_id:line_start-line_end\"]}}],\
         \"claims\":[{{\"claim\":\"...\",\"subject\":\"...\",\"predicate\":\"...\",\"object\":\"...\",\"source_spans\":[\"chunk_id:line_start-line_end\"]}}]}}\n\n\
         Bounds: at most {} entities, {} relationships, and {} claims. Relationship source and target names must match extracted entity names exactly. Use only these chunks and cited line ranges.\n\n\
         Selected chunks:\n",
        limits.max_entities, limits.max_relationships, limits.max_claims
    );

    for chunk in selected_chunks {
        prompt.push_str(&format!(
            "\n<chunk id=\"{}\" valid_lines=\"1-{}\">\nheading_path: {}\nlines:\n{}</chunk>\n",
            chunk.id.0,
            chunk.valid_line_count,
            chunk.heading_path.join(" > "),
            chunk.prompt_lines
        ));
    }
    prompt
}

fn build_repair_user_prompt(
    initial_prompt: &str,
    prior_response: &str,
    prior_error: &str,
    max_error_chars: usize,
) -> String {
    format!(
        "{initial_prompt}\n\nRepair the previous output. Return JSON only and keep the same schema.\n\
         Previous validation error: {}\n\
         Previous response excerpt: {}",
        bounded_text(prior_error, max_error_chars),
        bounded_text(prior_response, max_error_chars)
    )
}

fn select_prompt_chunks(
    source_id: &SourceId,
    chunks: &[Chunk],
    limits: &EffectiveExtractionLimits,
) -> Vec<PromptChunk> {
    chunks
        .iter()
        .filter(|chunk| chunk.source_id == *source_id && chunk.chunk_type == ChunkType::Child)
        .take(limits.max_chunks)
        .filter_map(|chunk| {
            let (prompt_lines, valid_line_count) =
                numbered_bounded_lines(&chunk.text, limits.max_chunk_chars);
            (valid_line_count > 0).then(|| PromptChunk {
                id: chunk.id.clone(),
                heading_path: chunk.heading_path.clone(),
                prompt_lines,
                valid_line_count,
            })
        })
        .collect()
}

fn numbered_bounded_lines(text: &str, max_chars: usize) -> (String, u32) {
    if max_chars == 0 {
        return (String::new(), 0);
    }

    let mut output = String::new();
    let mut used_chars = 0usize;
    let mut emitted_lines = 0u32;
    let mut lines = text.lines().peekable();
    if lines.peek().is_none() {
        let line = bounded_text("1: ", max_chars);
        return (line, 1);
    }

    for (line_index, line) in lines.enumerate() {
        let formatted = format!("{}: {}\n", line_index + 1, line);
        let remaining = max_chars.saturating_sub(used_chars);
        if remaining == 0 {
            break;
        }
        let formatted_len = formatted.chars().count();
        if formatted_len > remaining {
            output.push_str(&bounded_text(&formatted, remaining));
            emitted_lines += 1;
            break;
        }
        output.push_str(&formatted);
        used_chars += formatted_len;
        emitted_lines += 1;
    }

    (output, emitted_lines)
}

fn parse_extraction_json(response: &str) -> Result<RawExtraction> {
    let cleaned = clean_json_response(response);
    serde_json::from_str::<RawExtraction>(cleaned).context("parse graph extraction JSON")
}

fn clean_json_response(response: &str) -> &str {
    let trimmed = response.trim();
    if !trimmed.starts_with("```") {
        return trimmed;
    }
    let Some((_, rest)) = trimmed.split_once('\n') else {
        return trimmed;
    };
    rest.trim()
        .strip_suffix("```")
        .map(str::trim)
        .unwrap_or_else(|| rest.trim())
}

fn build_generated_graph(
    source: &Source,
    selected_chunks: &[PromptChunk],
    raw: RawExtraction,
    limits: &EffectiveExtractionLimits,
) -> GraphExtractionOutcome {
    let mut outcome = GraphExtractionOutcome::default();
    let valid_line_counts = selected_chunks
        .iter()
        .map(|chunk| (chunk.id.0.clone(), chunk.valid_line_count))
        .collect::<HashMap<_, _>>();

    let mut entity_nodes_by_name = HashMap::new();
    let mut pushed_node_ids = HashSet::new();
    let mut pushed_edge_ids = HashSet::new();
    let raw_entity_len = raw.entities.len();
    for (ordinal, entity) in raw
        .entities
        .into_iter()
        .take(limits.max_entities)
        .enumerate()
    {
        let Some(validated) = validate_entity(entity, &valid_line_counts) else {
            outcome.stats.dropped_item_count += 1;
            continue;
        };
        let external_id = generated_entity_external_id(&validated.entity_type, &validated.name);
        let node = generated_entity_node(&source.id, validated, external_id, ordinal as u32);
        let name_key = normalize_lookup_key(node.label.as_deref().unwrap_or_default());
        entity_nodes_by_name
            .entry(name_key)
            .or_insert_with(|| node.id.clone());
        if pushed_node_ids.insert(node.id.0.clone()) {
            outcome.nodes.push(node);
            outcome.stats.entity_count += 1;
        }
    }
    outcome.stats.dropped_item_count += raw_entity_len.saturating_sub(limits.max_entities);

    let raw_relationship_len = raw.relationships.len();
    for (ordinal, relationship) in raw
        .relationships
        .into_iter()
        .take(limits.max_relationships)
        .enumerate()
    {
        let Some(validated) = validate_relationship(relationship, &valid_line_counts) else {
            outcome.stats.dropped_item_count += 1;
            continue;
        };
        let Some(from_node_id) = entity_nodes_by_name
            .get(&normalize_lookup_key(&validated.source))
            .cloned()
        else {
            outcome.stats.dropped_item_count += 1;
            continue;
        };
        let Some(to_node_id) = entity_nodes_by_name
            .get(&normalize_lookup_key(&validated.target))
            .cloned()
        else {
            outcome.stats.dropped_item_count += 1;
            continue;
        };
        let edge = generated_relationship_edge(
            &source.id,
            validated,
            &from_node_id,
            &to_node_id,
            ordinal as u32,
        );
        if pushed_edge_ids.insert(edge.id.0.clone()) {
            outcome.edges.push(edge);
            outcome.stats.relationship_count += 1;
        }
    }
    outcome.stats.dropped_item_count +=
        raw_relationship_len.saturating_sub(limits.max_relationships);

    let raw_claim_len = raw.claims.len();
    for (ordinal, claim) in raw.claims.into_iter().take(limits.max_claims).enumerate() {
        let Some(validated) = validate_claim(claim, &valid_line_counts) else {
            outcome.stats.dropped_item_count += 1;
            continue;
        };
        let node = generated_claim_node(&source.id, validated, ordinal as u32);
        if pushed_node_ids.insert(node.id.0.clone()) {
            outcome.nodes.push(node);
            outcome.stats.claim_count += 1;
        }
    }
    outcome.stats.dropped_item_count += raw_claim_len.saturating_sub(limits.max_claims);

    outcome
}

fn validate_entity(
    entity: RawEntity,
    valid_line_counts: &HashMap<String, u32>,
) -> Option<ValidatedEntity> {
    let name = bounded_non_empty(entity.name, MAX_NAME_CHARS)?;
    let entity_type = parse_entity_type(&entity.entity_type)?;
    let spans = validate_source_spans(&entity.source_spans, valid_line_counts)?;
    Some(ValidatedEntity {
        name,
        entity_type,
        description: bounded_text(entity.description.trim(), MAX_DESCRIPTION_CHARS),
        spans,
    })
}

fn validate_relationship(
    relationship: RawRelationship,
    valid_line_counts: &HashMap<String, u32>,
) -> Option<ValidatedRelationship> {
    let source = bounded_non_empty(relationship.source, MAX_NAME_CHARS)?;
    let target = bounded_non_empty(relationship.target, MAX_NAME_CHARS)?;
    let (edge_type, relationship_type) = parse_relationship_type(&relationship.relationship_type)?;
    if !(0.0..=1.0).contains(&relationship.confidence) {
        return None;
    }
    let spans = validate_source_spans(&relationship.source_spans, valid_line_counts)?;
    Some(ValidatedRelationship {
        source,
        target,
        edge_type,
        relationship_type,
        description: bounded_text(relationship.description.trim(), MAX_DESCRIPTION_CHARS),
        confidence: relationship.confidence,
        spans,
    })
}

fn validate_claim(
    claim: RawClaim,
    valid_line_counts: &HashMap<String, u32>,
) -> Option<ValidatedClaim> {
    let claim_text = bounded_non_empty(claim.claim, MAX_CLAIM_CHARS)?;
    let subject = bounded_non_empty(claim.subject, MAX_NAME_CHARS)?;
    let predicate = bounded_non_empty(claim.predicate, MAX_NAME_CHARS)?;
    let object = bounded_non_empty(claim.object, MAX_NAME_CHARS)?;
    let spans = validate_source_spans(&claim.source_spans, valid_line_counts)?;
    Some(ValidatedClaim {
        claim: claim_text,
        subject,
        predicate,
        object,
        spans,
    })
}

fn validate_source_spans(
    spans: &[String],
    valid_line_counts: &HashMap<String, u32>,
) -> Option<Vec<ValidatedSourceSpan>> {
    if spans.is_empty() || spans.len() > HARD_MAX_SPANS_PER_ITEM {
        return None;
    }
    let mut seen = HashSet::new();
    let mut validated = Vec::with_capacity(spans.len());
    for raw_span in spans {
        let span = parse_source_span(raw_span.trim(), valid_line_counts)?;
        if !seen.insert((span.chunk_id.0.clone(), span.line_start, span.line_end)) {
            return None;
        }
        validated.push(span);
    }
    Some(validated)
}

fn parse_source_span(
    raw_span: &str,
    valid_line_counts: &HashMap<String, u32>,
) -> Option<ValidatedSourceSpan> {
    let (chunk_id, range) = raw_span.rsplit_once(':')?;
    let (line_start, line_end) = range.split_once('-')?;
    let line_start = line_start.parse::<u32>().ok()?;
    let line_end = line_end.parse::<u32>().ok()?;
    if line_start == 0 || line_end < line_start {
        return None;
    }
    let max_line = valid_line_counts.get(chunk_id).copied()?;
    if line_end > max_line {
        return None;
    }
    Some(ValidatedSourceSpan {
        chunk_id: ChunkId(chunk_id.to_string()),
        line_start,
        line_end,
    })
}

fn generated_entity_node(
    source_id: &SourceId,
    entity: ValidatedEntity,
    external_id: String,
    ordinal: u32,
) -> GraphNode {
    let source_spans = source_span_texts(&entity.spans);
    GraphNode {
        id: GraphNodeId::new(source_id, GraphNodeKind::GeneratedEntity, &external_id),
        source_id: source_id.clone(),
        kind: GraphNodeKind::GeneratedEntity,
        external_id,
        label: Some(entity.name),
        locator: None,
        ordinal: Some(ordinal),
        metadata: Some(json!({
            "origin": "llm_generated",
            "graph_data_kind": "entity",
            "prompt_version": GRAPH_EXTRACTION_PROMPT_VERSION,
            "entity_type": entity.entity_type,
            "description": entity.description,
            "source_spans": source_spans,
        })),
    }
}

fn generated_relationship_edge(
    source_id: &SourceId,
    relationship: ValidatedRelationship,
    from_node_id: &GraphNodeId,
    to_node_id: &GraphNodeId,
    ordinal: u32,
) -> GraphEdge {
    let source_spans = source_span_texts(&relationship.spans);
    GraphEdge {
        id: GraphEdgeId::new(
            source_id,
            relationship.edge_type,
            from_node_id,
            to_node_id,
            Some(ordinal),
        ),
        source_id: source_id.clone(),
        edge_type: relationship.edge_type,
        from_node_id: from_node_id.clone(),
        to_node_id: to_node_id.clone(),
        ordinal: Some(ordinal),
        weight: Some(relationship.confidence),
        metadata: Some(json!({
            "origin": "llm_generated",
            "graph_data_kind": "relationship",
            "prompt_version": GRAPH_EXTRACTION_PROMPT_VERSION,
            "relationship_type": relationship.relationship_type,
            "source": relationship.source,
            "target": relationship.target,
            "description": relationship.description,
            "confidence": relationship.confidence,
            "source_spans": source_spans,
        })),
    }
}

fn generated_claim_node(source_id: &SourceId, claim: ValidatedClaim, ordinal: u32) -> GraphNode {
    let source_spans = source_span_texts(&claim.spans);
    let external_id = generated_claim_external_id(&claim, &source_spans);
    GraphNode {
        id: GraphNodeId::new(source_id, GraphNodeKind::GeneratedClaim, &external_id),
        source_id: source_id.clone(),
        kind: GraphNodeKind::GeneratedClaim,
        external_id,
        label: Some(claim.claim.clone()),
        locator: None,
        ordinal: Some(ordinal),
        metadata: Some(json!({
            "origin": "llm_generated",
            "graph_data_kind": "claim",
            "prompt_version": GRAPH_EXTRACTION_PROMPT_VERSION,
            "claim": claim.claim,
            "subject": claim.subject,
            "predicate": claim.predicate,
            "object": claim.object,
            "source_spans": source_spans,
        })),
    }
}

fn generated_entity_external_id(entity_type: &str, name: &str) -> String {
    format!(
        "generated_entity:{entity_type}:{}",
        normalize_lookup_key(name)
    )
}

fn generated_claim_external_id(claim: &ValidatedClaim, source_spans: &[String]) -> String {
    let payload = format!(
        "{}\n{}\n{}\n{}\n{}",
        claim.claim,
        claim.subject,
        claim.predicate,
        claim.object,
        source_spans.join("\n")
    );
    format!(
        "generated_claim:{}",
        &crate::types::hex_sha256(payload.as_bytes())[..16]
    )
}

fn source_span_texts(spans: &[ValidatedSourceSpan]) -> Vec<String> {
    spans.iter().map(ValidatedSourceSpan::as_text).collect()
}

fn parse_entity_type(value: &str) -> Option<String> {
    match normalize_schema_value(value).as_str() {
        "feature" => Some("feature".to_string()),
        "component" => Some("component".to_string()),
        "concept" => Some("concept".to_string()),
        "file" => Some("file".to_string()),
        "function" => Some("function".to_string()),
        "issue" => Some("issue".to_string()),
        "other" => Some("other".to_string()),
        _ => None,
    }
}

fn parse_relationship_type(value: &str) -> Option<(EdgeType, String)> {
    match normalize_schema_value(value).as_str() {
        "depends_on" => Some((EdgeType::GeneratedDependsOn, "depends_on".to_string())),
        "implements" => Some((EdgeType::GeneratedImplements, "implements".to_string())),
        "mentions" => Some((EdgeType::GeneratedMentions, "mentions".to_string())),
        "conflicts_with" => Some((
            EdgeType::GeneratedConflictsWith,
            "conflicts_with".to_string(),
        )),
        "supports" => Some((EdgeType::GeneratedSupports, "supports".to_string())),
        "other" => Some((EdgeType::GeneratedOther, "other".to_string())),
        _ => None,
    }
}

fn normalize_schema_value(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_lookup_key(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn bounded_non_empty(value: String, max_chars: usize) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| bounded_text(trimmed, max_chars))
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::StreamExt;

    use super::*;
    use crate::provider::{ChatResponse, ChatStream, ProviderResult, TokenUsage};
    use crate::types::SourceStatus;

    #[derive(Clone)]
    struct MockChatModel {
        responses: Arc<Mutex<VecDeque<MockChatResult>>>,
        requests: Arc<Mutex<Vec<ChatRequest>>>,
        calls: Arc<AtomicUsize>,
    }

    #[derive(Debug)]
    enum MockChatResult {
        Text(String),
    }

    impl MockChatModel {
        fn new(responses: Vec<MockChatResult>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn text(response: impl Into<String>) -> Self {
            Self::new(vec![MockChatResult::Text(response.into())])
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests
                .lock()
                .expect("request lock should not be poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl ChatModel for MockChatModel {
        async fn chat(&self, req: ChatRequest) -> ProviderResult<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .expect("request lock should not be poisoned")
                .push(req);
            match self
                .responses
                .lock()
                .expect("response lock should not be poisoned")
                .pop_front()
                .expect("mock chat response should be available")
            {
                MockChatResult::Text(content) => Ok(ChatResponse {
                    content,
                    finish_reason: None,
                    usage: Some(TokenUsage {
                        prompt_tokens: Some(1),
                        completion_tokens: Some(1),
                        total_tokens: Some(2),
                    }),
                }),
            }
        }

        async fn stream_chat(&self, _req: ChatRequest) -> ProviderResult<ChatStream> {
            Ok(futures::stream::empty().boxed())
        }
    }

    fn source() -> Source {
        Source {
            id: SourceId("src".to_string()),
            path: "doc.md".into(),
            hash: "hash".to_string(),
            status: SourceStatus::Indexed,
            parser_used: Some("markdown".to_string()),
            last_ingested_at: None,
        }
    }

    fn chunk(id: &str, text: &str) -> Chunk {
        Chunk {
            id: ChunkId(id.to_string()),
            source_id: SourceId("src".to_string()),
            chunk_hash: format!("hash-{id}"),
            embedding_input_hash: None,
            text: text.to_string(),
            context_text: None,
            token_count: 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Intro".to_string()],
            evidence_unit_ids: Vec::new(),
        }
    }

    fn enabled_config() -> GraphExtractionConfig {
        GraphExtractionConfig {
            enabled: true,
            ..GraphExtractionConfig::default()
        }
    }

    fn valid_json() -> String {
        serde_json::json!({
            "entities": [
                {
                    "name": "Contextual Retrieval",
                    "type": "feature",
                    "description": "Adds context before indexing",
                    "source_spans": ["chunk-a:1-2"]
                },
                {
                    "name": "Ingest Pipeline",
                    "type": "component",
                    "description": "Coordinates ingest",
                    "source_spans": ["chunk-a:2-2"]
                }
            ],
            "relationships": [
                {
                    "source": "Ingest Pipeline",
                    "target": "Contextual Retrieval",
                    "type": "implements",
                    "description": "The ingest pipeline performs contextual retrieval.",
                    "confidence": 0.9,
                    "source_spans": ["chunk-a:2-2"]
                }
            ],
            "claims": [
                {
                    "claim": "Contextual Retrieval is optional during ingest.",
                    "subject": "Contextual Retrieval",
                    "predicate": "is optional during",
                    "object": "ingest",
                    "source_spans": ["chunk-a:1-2"]
                }
            ]
        })
        .to_string()
    }

    #[tokio::test]
    async fn extracts_generated_graph_with_valid_spans() {
        let mock = MockChatModel::text(valid_json());
        let extractor = GraphExtractor::from_chat_model(Arc::new(mock.clone()));

        let outcome = extractor
            .extract(
                &source(),
                &[chunk(
                    "chunk-a",
                    "Contextual Retrieval is optional.\nThe ingest pipeline implements it.",
                )],
                &enabled_config(),
            )
            .await
            .unwrap();

        assert_eq!(mock.call_count(), 1);
        assert_eq!(outcome.stats.entity_count, 2);
        assert_eq!(outcome.stats.relationship_count, 1);
        assert_eq!(outcome.stats.claim_count, 1);
        assert_eq!(outcome.nodes.len(), 3);
        assert_eq!(outcome.edges.len(), 1);
        assert!(outcome
            .nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::GeneratedEntity));
        assert!(outcome
            .nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::GeneratedClaim));
        let edge = &outcome.edges[0];
        assert_eq!(edge.edge_type, EdgeType::GeneratedImplements);
        assert_eq!(edge.weight, Some(0.9));
        assert_eq!(
            edge.metadata
                .as_ref()
                .and_then(|value| value.get("origin"))
                .and_then(serde_json::Value::as_str),
            Some("llm_generated")
        );
        assert_eq!(
            outcome.nodes[0]
                .metadata
                .as_ref()
                .and_then(|value| value.get("source_spans"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[tokio::test]
    async fn retries_invalid_json_once_and_repairs() {
        let mock = MockChatModel::new(vec![
            MockChatResult::Text("not json".to_string()),
            MockChatResult::Text(valid_json()),
        ]);
        let extractor = GraphExtractor::from_chat_model(Arc::new(mock.clone()));

        let outcome = extractor
            .extract(
                &source(),
                &[chunk(
                    "chunk-a",
                    "Contextual Retrieval is optional.\nThe ingest pipeline implements it.",
                )],
                &enabled_config(),
            )
            .await
            .unwrap();

        assert_eq!(mock.call_count(), 2);
        assert_eq!(outcome.stats.attempt_count, 2);
        assert_eq!(outcome.stats.entity_count, 2);
        let requests = mock.requests();
        assert!(requests[1]
            .messages
            .iter()
            .any(|message| matches!(&message.content, crate::provider::ChatMessageContent::Text(text) if text.contains("Repair the previous output"))));
    }

    #[tokio::test]
    async fn drops_items_with_missing_duplicate_or_out_of_range_spans() {
        let response = serde_json::json!({
            "entities": [
                {
                    "name": "Valid Entity",
                    "type": "concept",
                    "description": "valid",
                    "source_spans": ["chunk-a:1-1"]
                },
                {
                    "name": "Missing Span",
                    "type": "concept",
                    "description": "invalid",
                    "source_spans": []
                },
                {
                    "name": "Duplicate Span",
                    "type": "concept",
                    "description": "invalid",
                    "source_spans": ["chunk-a:1-1", "chunk-a:1-1"]
                },
                {
                    "name": "Out Of Range",
                    "type": "concept",
                    "description": "invalid",
                    "source_spans": ["chunk-a:3-3"]
                },
                {
                    "name": "Unknown Chunk",
                    "type": "concept",
                    "description": "invalid",
                    "source_spans": ["chunk-z:1-1"]
                }
            ],
            "relationships": [],
            "claims": [
                {
                    "claim": "Invalid claim",
                    "subject": "s",
                    "predicate": "p",
                    "object": "o",
                    "source_spans": ["chunk-a:2-1"]
                }
            ]
        })
        .to_string();
        let extractor = GraphExtractor::from_chat_model(Arc::new(MockChatModel::text(response)));

        let outcome = extractor
            .extract(
                &source(),
                &[chunk("chunk-a", "one\ntwo")],
                &enabled_config(),
            )
            .await
            .unwrap();

        assert_eq!(outcome.stats.entity_count, 1);
        assert_eq!(outcome.stats.claim_count, 0);
        assert_eq!(outcome.stats.dropped_item_count, 5);
        assert_eq!(outcome.nodes.len(), 1);
        assert_eq!(outcome.nodes[0].label.as_deref(), Some("Valid Entity"));
    }

    #[tokio::test]
    async fn accepts_source_spans_when_chunk_id_contains_colons() {
        let chunk_id = "src:p1:n0:caption:abcdef0123456789:chunk";
        let response = serde_json::json!({
            "entities": [
                {
                    "name": "Caption Entity",
                    "type": "concept",
                    "description": "valid",
                    "source_spans": [format!("{chunk_id}:1-1")]
                },
                {
                    "name": "Caption Component",
                    "type": "component",
                    "description": "valid",
                    "source_spans": [format!("{chunk_id}:1-1")]
                }
            ],
            "relationships": [
                {
                    "source": "Caption Entity",
                    "target": "Caption Component",
                    "type": "mentions",
                    "description": "Caption Entity mentions Caption Component.",
                    "confidence": 0.8,
                    "source_spans": [format!("{chunk_id}:1-1")]
                }
            ],
            "claims": [
                {
                    "claim": "Caption Entity mentions Caption Component.",
                    "subject": "Caption Entity",
                    "predicate": "mentions",
                    "object": "Caption Component",
                    "source_spans": [format!("{chunk_id}:1-1")]
                }
            ]
        })
        .to_string();
        let extractor = GraphExtractor::from_chat_model(Arc::new(MockChatModel::text(response)));

        let outcome = extractor
            .extract(
                &source(),
                &[chunk(
                    chunk_id,
                    "Caption Entity mentions Caption Component.",
                )],
                &enabled_config(),
            )
            .await
            .unwrap();

        assert_eq!(outcome.stats.entity_count, 2);
        assert_eq!(outcome.stats.relationship_count, 1);
        assert_eq!(outcome.stats.claim_count, 1);
        assert_eq!(outcome.stats.dropped_item_count, 0);
        assert!(outcome.nodes.iter().all(|node| {
            node.metadata
                .as_ref()
                .and_then(|value| value.get("source_spans"))
                .and_then(serde_json::Value::as_array)
                .and_then(|spans| spans.first())
                .and_then(serde_json::Value::as_str)
                == Some(&format!("{chunk_id}:1-1"))
        }));
    }

    #[tokio::test]
    async fn rejects_malformed_line_range_after_colon_bearing_chunk_id() {
        let chunk_id = "src:p1:n0:caption:abcdef0123456789:chunk";
        let response = serde_json::json!({
            "entities": [
                {
                    "name": "Malformed Span",
                    "type": "concept",
                    "description": "invalid",
                    "source_spans": [format!("{chunk_id}:1-nope")]
                }
            ],
            "relationships": [],
            "claims": []
        })
        .to_string();
        let extractor = GraphExtractor::from_chat_model(Arc::new(MockChatModel::text(response)));

        let outcome = extractor
            .extract(
                &source(),
                &[chunk(chunk_id, "Malformed Span")],
                &enabled_config(),
            )
            .await
            .unwrap();

        assert_eq!(outcome.stats.entity_count, 0);
        assert_eq!(outcome.stats.dropped_item_count, 1);
        assert!(outcome.nodes.is_empty());
    }

    #[tokio::test]
    async fn hard_bounds_chunks_items_retries_and_output_tokens() {
        let mut entities = Vec::new();
        for index in 0..70 {
            entities.push(serde_json::json!({
                "name": format!("Entity {index}"),
                "type": "concept",
                "description": "bounded",
                "source_spans": ["chunk-0:1-1"]
            }));
        }
        let response = serde_json::json!({
            "entities": entities,
            "relationships": [],
            "claims": []
        })
        .to_string();
        let mock = MockChatModel::text(response);
        let extractor = GraphExtractor::from_chat_model(Arc::new(mock.clone()));
        let chunks = (0..20)
            .map(|index| chunk(&format!("chunk-{index}"), "one"))
            .collect::<Vec<_>>();
        let config = GraphExtractionConfig {
            enabled: true,
            max_chunks: usize::MAX,
            max_chunk_chars: usize::MAX,
            max_entities: usize::MAX,
            max_relationships: usize::MAX,
            max_claims: usize::MAX,
            max_retries: usize::MAX,
            max_output_tokens: u32::MAX,
            max_response_chars: usize::MAX,
            max_error_chars: usize::MAX,
        };

        let outcome = extractor
            .extract(&source(), &chunks, &config)
            .await
            .unwrap();

        assert_eq!(outcome.stats.selected_chunk_count, HARD_MAX_CHUNKS);
        assert_eq!(outcome.stats.entity_count, HARD_MAX_ENTITIES);
        assert_eq!(outcome.stats.dropped_item_count, 70 - HARD_MAX_ENTITIES);
        let requests = mock.requests();
        assert_eq!(requests[0].max_tokens, Some(HARD_MAX_OUTPUT_TOKENS));
        let user_prompt = match &requests[0].messages[1].content {
            crate::provider::ChatMessageContent::Text(text) => text,
            crate::provider::ChatMessageContent::Parts(_) => "",
        };
        assert!(user_prompt.contains("chunk-15"));
        assert!(!user_prompt.contains("chunk-16"));
    }

    #[tokio::test]
    async fn hard_bounds_retry_attempts_after_invalid_json() {
        let mock = MockChatModel::new(vec![
            MockChatResult::Text("not json 1".to_string()),
            MockChatResult::Text("not json 2".to_string()),
            MockChatResult::Text("not json 3".to_string()),
        ]);
        let extractor = GraphExtractor::from_chat_model(Arc::new(mock.clone()));
        let config = GraphExtractionConfig {
            enabled: true,
            max_retries: usize::MAX,
            ..GraphExtractionConfig::default()
        };

        let error = extractor
            .extract(&source(), &[chunk("chunk-a", "one")], &config)
            .await
            .unwrap_err();

        assert_eq!(mock.call_count(), HARD_MAX_RETRIES + 1);
        assert!(error.to_string().contains("invalid JSON"));
    }
}
