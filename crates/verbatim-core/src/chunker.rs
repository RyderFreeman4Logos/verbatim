use anyhow::{anyhow, ensure, Result};
use std::collections::{HashMap, HashSet};

use crate::evidence_spans::{ChunkEvidenceSpan, EvidenceSpanTrust};
use crate::types::{
    hex_sha256, Chunk, ChunkId, ChunkType, EvidenceId, EvidenceUnit, SourceId, SourceLocator,
};

/// Chunking identity; v7 declares the conservative-v4 token estimator.
pub const CHUNKER_VERSION: &str = "parent-child-v7";
const DEFAULT_CHILD_TARGET: usize = 300;
const DEFAULT_CHILD_OVERLAP: usize = 80;
const DEFAULT_PARENT_CHILDREN: usize = 5;
const ESTIMATOR_UNITS_PER_TOKEN: usize = 4;
const CHEAP_ALPHANUMERIC_RUN_LENGTH: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkerConfig {
    pub child_target_tokens: usize,
    pub child_overlap_tokens: usize,
    pub parent_children_count: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            child_target_tokens: DEFAULT_CHILD_TARGET,
            child_overlap_tokens: DEFAULT_CHILD_OVERLAP,
            parent_children_count: DEFAULT_PARENT_CHILDREN,
        }
    }
}

pub struct ChunkOutput {
    pub chunks: Vec<Chunk>,
    pub links: Vec<(ChunkId, EvidenceId)>,
    pub evidence_spans: Vec<ChunkEvidenceSpan>,
}

/// Estimates tokens without model-specific tokenizer artifacts.
///
/// The first 24 scalars in an ASCII alphanumeric run cost one quarter-token unit
/// each; subsequent scalars and all other scalars cost one token each. This
/// keeps plain Latin text near chars/4 while conservatively counting opaque
/// identifiers, code, URLs, punctuation, whitespace, and CJK.
pub(crate) fn estimate_tokens(text: &str) -> u32 {
    estimate_spaced_tokens(std::iter::once(text)).min(u32::MAX as usize) as u32
}

pub(crate) fn estimate_spaced_tokens<'a>(texts: impl IntoIterator<Item = &'a str>) -> usize {
    let mut units = 0usize;
    let mut has_text = false;
    let mut alphanumeric_run_length = 0usize;

    for text in texts.into_iter().filter(|text| !text.is_empty()) {
        if has_text {
            units = units.saturating_add(scalar_estimator_units(' ', &mut alphanumeric_run_length));
        }
        units = units.saturating_add(estimator_units(text, &mut alphanumeric_run_length));
        has_text = true;
    }

    if units == 0 {
        0
    } else {
        1 + (units - 1) / ESTIMATOR_UNITS_PER_TOKEN
    }
}

fn estimator_units(text: &str, alphanumeric_run_length: &mut usize) -> usize {
    text.chars().fold(0usize, |units, character| {
        units.saturating_add(scalar_estimator_units(character, alphanumeric_run_length))
    })
}

fn scalar_estimator_units(character: char, alphanumeric_run_length: &mut usize) -> usize {
    if character.is_ascii_alphanumeric() {
        *alphanumeric_run_length = alphanumeric_run_length.saturating_add(1);
        if *alphanumeric_run_length <= CHEAP_ALPHANUMERIC_RUN_LENGTH {
            1
        } else {
            ESTIMATOR_UNITS_PER_TOKEN
        }
    } else {
        *alphanumeric_run_length = 0;
        ESTIMATOR_UNITS_PER_TOKEN
    }
}

fn overlap_start_for_token_budget(text: &str, budget_tokens: usize) -> usize {
    let budget_units = budget_tokens.saturating_mul(ESTIMATOR_UNITS_PER_TOKEN);
    let mut retained_units = 0usize;
    let mut start = text.len();
    let mut alphanumeric_run_length = 0usize;

    for (index, character) in text.char_indices().rev() {
        let character_units = scalar_estimator_units(character, &mut alphanumeric_run_length);
        if retained_units.saturating_add(character_units) > budget_units {
            break;
        }
        retained_units = retained_units.saturating_add(character_units);
        start = index;
    }

    start
}

pub fn chunk_evidence(
    source_id: &SourceId,
    evidence: &[EvidenceUnit],
    config: &ChunkerConfig,
) -> ChunkOutput {
    if evidence.is_empty() {
        return ChunkOutput {
            chunks: Vec::new(),
            links: Vec::new(),
            evidence_spans: Vec::new(),
        };
    }

    let hard_boundary_groups = split_by_hard_boundary(evidence);
    let mut all_chunks = Vec::new();
    let mut all_links = Vec::new();
    let mut all_evidence_spans = Vec::new();
    let mut child_id_counts = HashMap::new();
    let mut parent_id_counts = HashMap::new();

    for hard_boundary_group in hard_boundary_groups {
        let children = build_children(
            source_id,
            &hard_boundary_group,
            config,
            &mut child_id_counts,
        );
        for child_group in children.chunks(config.parent_children_count.max(1)) {
            let parent = build_parent(source_id, child_group, &mut parent_id_counts);
            let parent_id = parent.chunk.id.clone();
            append_links(&mut all_links, &parent.chunk);
            all_evidence_spans.extend(persisted_spans(&parent.chunk, &parent.spans));
            all_chunks.push(parent.chunk);

            for child in child_group {
                let mut child_with_parent = child.chunk.clone();
                child_with_parent.parent_chunk_id = Some(parent_id.clone());
                append_links(&mut all_links, &child_with_parent);
                all_evidence_spans.extend(persisted_spans(&child_with_parent, &child.spans));
                all_chunks.push(child_with_parent);
            }
        }
    }

    ChunkOutput {
        chunks: all_chunks,
        links: all_links,
        evidence_spans: all_evidence_spans,
    }
}

fn split_by_hard_boundary(evidence: &[EvidenceUnit]) -> Vec<Vec<&EvidenceUnit>> {
    let mut groups = Vec::new();
    let mut current: Vec<&EvidenceUnit> = Vec::new();
    let mut current_key = None;

    for unit in evidence {
        let key = hard_boundary_key(unit);
        if current_key
            .as_ref()
            .is_some_and(|current_key| current_key != &key)
        {
            groups.push(std::mem::take(&mut current));
        }
        current.push(unit);
        current_key = Some(key);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

fn hard_boundary_key(unit: &EvidenceUnit) -> String {
    let section = unit
        .heading_path
        .first()
        .filter(|heading| !heading.trim().is_empty())
        .map(|heading| format!("heading:{heading}"));
    let locator_key = match &unit.locator {
        SourceLocator::Markdown {
            path,
            heading_path,
            heading_slug,
            ..
        } => {
            let heading = heading_path
                .first()
                .filter(|heading| !heading.slug.trim().is_empty())
                .map(|heading| format!("{}:{}:{}", heading.level, heading.slug, heading.line))
                .or_else(|| {
                    heading_slug
                        .as_deref()
                        .filter(|heading| !heading.trim().is_empty())
                        .map(str::to_owned)
                })
                .or(section)
                .unwrap_or_else(|| "preamble".to_string());
            format!("markdown:{path}:{heading}")
        }
        SourceLocator::Pdf { page, .. } | SourceLocator::PdfOcr { page, .. } => {
            section.unwrap_or_else(|| format!("pdf-page:{page}"))
        }
        SourceLocator::PdfImage { page, .. } => format!("pdf-image-page:{page}"),
        SourceLocator::Document { path_or_url, .. } => section
            .map(|heading| format!("document:{path_or_url}:{heading}"))
            .unwrap_or_else(|| format!("document:{path_or_url}")),
        SourceLocator::Canonical { locator } => {
            format!("canonical:{}:{}", locator.profile_id, locator.work_id)
        }
    };
    format!("source:{}:{locator_key}", unit.source_id.0)
}

fn build_children(
    source_id: &SourceId,
    section: &[&EvidenceUnit],
    config: &ChunkerConfig,
    id_counts: &mut HashMap<String, usize>,
) -> Vec<ChunkWithSpans> {
    let target_tokens = config
        .child_target_tokens
        .saturating_add(config.child_target_tokens / 5);
    let mut children = Vec::new();
    let mut current = TextWithSpans::default();
    let mut current_heading: Vec<String> = Vec::new();

    for unit in section {
        let would_exceed =
            estimate_spaced_tokens([current.text.as_str(), unit.text.as_str()]) > target_tokens;

        if would_exceed && !current.text.trim().is_empty() {
            let (text, spans) = current.trimmed();
            children.push(make_child(
                source_id,
                id_counts,
                &text,
                &spans,
                &current_heading,
            ));

            let overlap_start =
                overlap_start_for_token_budget(&current.text, config.child_overlap_tokens);
            current.retain_from(overlap_start);
        }

        current.append(unit);
        if current_heading.is_empty() {
            current_heading = unit.heading_path.clone();
        }
    }

    if !current.text.trim().is_empty() {
        let (text, spans) = current.trimmed();
        children.push(make_child(
            source_id,
            id_counts,
            &text,
            &spans,
            &current_heading,
        ));
    }

    children
}

fn make_child(
    source_id: &SourceId,
    id_counts: &mut HashMap<String, usize>,
    text: &str,
    spans: &[PendingEvidenceSpan],
    heading_path: &[String],
) -> ChunkWithSpans {
    let evidence_identity_hashes = unique_evidence_identity_hashes(spans);
    let chunk_hash = deterministic_chunk_hash(
        ChunkType::Child,
        text,
        heading_path,
        &evidence_identity_hashes,
    );
    let id = unique_chunk_id(source_id, "child", &chunk_hash, id_counts);
    ChunkWithSpans {
        spans: spans.to_vec(),
        chunk: Chunk {
            id,
            source_id: source_id.clone(),
            chunk_hash,
            embedding_input_hash: None,
            text: text.to_string(),
            context_text: None,
            token_count: estimate_tokens(text),
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: heading_path.to_vec(),
            evidence_unit_ids: unique_evidence_ids(spans),
        },
    }
}

fn build_parent(
    source_id: &SourceId,
    children: &[ChunkWithSpans],
    id_counts: &mut HashMap<String, usize>,
) -> ChunkWithSpans {
    let mut text = String::new();
    let mut spans = Vec::new();
    for child in children {
        if !text.is_empty() {
            text.push(' ');
        }
        let offset = text.len() as u64;
        text.push_str(&child.chunk.text);
        spans.extend(child.spans.iter().cloned().map(|span| span.rebased(offset)));
    }
    let (text, spans) = trim_text_and_spans(&text, &spans);
    let heading_path = children
        .first()
        .map(|child| child.chunk.heading_path.clone())
        .unwrap_or_default();
    let child_hashes = children
        .iter()
        .map(|child| child.chunk.chunk_hash.clone())
        .collect::<Vec<_>>();
    let chunk_hash =
        deterministic_chunk_hash(ChunkType::Parent, &text, &heading_path, &child_hashes);
    let id = unique_chunk_id(source_id, "parent", &chunk_hash, id_counts);
    let evidence_unit_ids = unique_evidence_ids(&spans);
    ChunkWithSpans {
        spans,
        chunk: Chunk {
            id,
            source_id: source_id.clone(),
            chunk_hash,
            embedding_input_hash: None,
            text: text.clone(),
            context_text: None,
            token_count: estimate_tokens(&text),
            chunk_type: ChunkType::Parent,
            parent_chunk_id: None,
            heading_path,
            evidence_unit_ids,
        },
    }
}

#[derive(Clone)]
pub(crate) struct ChunkWithSpans {
    pub(crate) chunk: Chunk,
    pub(crate) spans: Vec<PendingEvidenceSpan>,
}

#[derive(Clone)]
pub(crate) struct PendingEvidenceSpan {
    evidence_id: EvidenceId,
    chunk_byte_start: u64,
    chunk_byte_end: u64,
    evidence_byte_start: u64,
    evidence_byte_end: u64,
    evidence_text_hash: String,
    evidence_identity_hash: String,
    locator: SourceLocator,
    trust: EvidenceSpanTrust,
}

impl PendingEvidenceSpan {
    fn from_unit(unit: &EvidenceUnit, chunk_byte_start: u64, chunk_byte_end: u64) -> Self {
        Self {
            evidence_id: unit.id.clone(),
            chunk_byte_start,
            chunk_byte_end,
            evidence_byte_start: 0,
            evidence_byte_end: unit.text.len() as u64,
            evidence_text_hash: unit.text_hash.clone(),
            evidence_identity_hash: evidence_identity_hash(unit),
            locator: unit.locator.clone(),
            trust: if unit.derived_from.is_some() {
                EvidenceSpanTrust::Derived
            } else {
                EvidenceSpanTrust::Direct
            },
        }
    }

    fn rebased(mut self, offset: u64) -> Self {
        self.chunk_byte_start += offset;
        self.chunk_byte_end += offset;
        self
    }
}

#[derive(Default)]
pub(crate) struct TextWithSpans {
    text: String,
    spans: Vec<PendingEvidenceSpan>,
}

impl TextWithSpans {
    pub(crate) fn append(&mut self, unit: &EvidenceUnit) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        let start = self.text.len() as u64;
        self.text.push_str(&unit.text);
        self.spans.push(PendingEvidenceSpan::from_unit(
            unit,
            start,
            self.text.len() as u64,
        ));
    }

    pub(crate) fn append_composed(&mut self, text: &str, spans: &[PendingEvidenceSpan]) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        let offset = self.text.len() as u64;
        self.text.push_str(text);
        self.spans
            .extend(spans.iter().cloned().map(|span| span.rebased(offset)));
    }

    fn retain_from(&mut self, overlap_start: usize) {
        let overlap_end = self.text.len() as u64;
        let overlap_start = overlap_start as u64;
        self.spans = self
            .spans
            .iter()
            .filter_map(|span| {
                let kept_start = span.chunk_byte_start.max(overlap_start);
                (kept_start < span.chunk_byte_end).then(|| PendingEvidenceSpan {
                    chunk_byte_start: kept_start - overlap_start,
                    chunk_byte_end: span.chunk_byte_end - overlap_start,
                    evidence_byte_start: span.evidence_byte_start
                        + kept_start.saturating_sub(span.chunk_byte_start),
                    evidence_byte_end: span.evidence_byte_end
                        - span.chunk_byte_end.saturating_sub(overlap_end),
                    evidence_id: span.evidence_id.clone(),
                    evidence_text_hash: span.evidence_text_hash.clone(),
                    evidence_identity_hash: span.evidence_identity_hash.clone(),
                    locator: span.locator.clone(),
                    trust: span.trust,
                })
            })
            .collect();
        self.text = self.text[overlap_start as usize..].to_string();
    }

    pub(crate) fn trimmed(&self) -> (String, Vec<PendingEvidenceSpan>) {
        trim_text_and_spans(&self.text, &self.spans)
    }
}

fn trim_text_and_spans(
    text: &str,
    spans: &[PendingEvidenceSpan],
) -> (String, Vec<PendingEvidenceSpan>) {
    let trim_start = (text.len() - text.trim_start().len()) as u64;
    let trimmed = text.trim();
    let trim_end = trim_start + trimmed.len() as u64;
    let spans = spans
        .iter()
        .filter_map(|span| {
            let start = span.chunk_byte_start.max(trim_start);
            let end = span.chunk_byte_end.min(trim_end);
            (start < end).then(|| PendingEvidenceSpan {
                chunk_byte_start: start - trim_start,
                chunk_byte_end: end - trim_start,
                evidence_byte_start: span.evidence_byte_start
                    + start.saturating_sub(span.chunk_byte_start),
                evidence_byte_end: span.evidence_byte_end - span.chunk_byte_end.saturating_sub(end),
                evidence_id: span.evidence_id.clone(),
                evidence_text_hash: span.evidence_text_hash.clone(),
                evidence_identity_hash: span.evidence_identity_hash.clone(),
                locator: span.locator.clone(),
                trust: span.trust,
            })
        })
        .collect();
    (trimmed.to_string(), spans)
}

fn unique_evidence_ids(spans: &[PendingEvidenceSpan]) -> Vec<EvidenceId> {
    let mut seen = HashSet::new();
    spans
        .iter()
        .filter(|span| seen.insert(span.evidence_id.clone()))
        .map(|span| span.evidence_id.clone())
        .collect()
}

fn unique_evidence_identity_hashes(spans: &[PendingEvidenceSpan]) -> Vec<String> {
    let mut seen = HashSet::new();
    spans
        .iter()
        .filter(|span| seen.insert(span.evidence_identity_hash.as_str()))
        .map(|span| span.evidence_identity_hash.clone())
        .collect()
}

fn append_links(links: &mut Vec<(ChunkId, EvidenceId)>, chunk: &Chunk) {
    links.extend(
        chunk
            .evidence_unit_ids
            .iter()
            .cloned()
            .map(|evidence_id| (chunk.id.clone(), evidence_id)),
    );
}

fn persisted_spans(chunk: &Chunk, spans: &[PendingEvidenceSpan]) -> Vec<ChunkEvidenceSpan> {
    spans
        .iter()
        .map(|span| ChunkEvidenceSpan {
            chunk_id: chunk.id.clone(),
            evidence_id: span.evidence_id.clone(),
            chunk_byte_start: span.chunk_byte_start,
            chunk_byte_end: span.chunk_byte_end,
            evidence_byte_start: span.evidence_byte_start,
            evidence_byte_end: span.evidence_byte_end,
            evidence_text_hash: span.evidence_text_hash.clone(),
            locator: span.locator.clone(),
            trust: span.trust,
        })
        .collect()
}

pub(crate) fn persist_spans_checked(
    chunk: &Chunk,
    spans: &[PendingEvidenceSpan],
    evidence: &[EvidenceUnit],
) -> Result<Vec<ChunkEvidenceSpan>> {
    let evidence_by_id = evidence
        .iter()
        .map(|unit| (&unit.id, unit))
        .collect::<HashMap<_, _>>();

    for span in spans {
        let unit = evidence_by_id.get(&span.evidence_id).ok_or_else(|| {
            anyhow!(
                "chunk {} references missing evidence {}",
                chunk.id.0,
                span.evidence_id.0
            )
        })?;
        let chunk_text = text_for_span(
            &chunk.text,
            span.chunk_byte_start,
            span.chunk_byte_end,
            "chunk",
        )?;
        let evidence_text = text_for_span(
            &unit.text,
            span.evidence_byte_start,
            span.evidence_byte_end,
            "evidence",
        )?;
        ensure!(
            chunk_text == evidence_text,
            "chunk {} provenance for evidence {} does not resolve to identical text",
            chunk.id.0,
            unit.id.0
        );
        ensure!(
            span.evidence_text_hash == unit.text_hash,
            "chunk {} provenance text hash mismatches evidence {}",
            chunk.id.0,
            unit.id.0
        );
        ensure!(
            span.locator == unit.locator,
            "chunk {} provenance locator mismatches evidence {}",
            chunk.id.0,
            unit.id.0
        );
        let expected_trust = if unit.derived_from.is_some() {
            EvidenceSpanTrust::Derived
        } else {
            EvidenceSpanTrust::Direct
        };
        ensure!(
            span.trust == expected_trust,
            "chunk {} provenance trust mismatches evidence {}",
            chunk.id.0,
            unit.id.0
        );
    }

    Ok(persisted_spans(chunk, spans))
}

fn text_for_span<'a>(text: &'a str, start: u64, end: u64, subject: &str) -> Result<&'a str> {
    let start = usize::try_from(start)
        .map_err(|_| anyhow!("{subject} provenance start does not fit usize"))?;
    let end =
        usize::try_from(end).map_err(|_| anyhow!("{subject} provenance end does not fit usize"))?;
    text.get(start..end)
        .ok_or_else(|| anyhow!("{subject} provenance range {start}..{end} is invalid"))
}

pub(crate) fn full_unit_evidence_spans(
    chunks: &[Chunk],
    evidence: &[EvidenceUnit],
) -> Vec<ChunkEvidenceSpan> {
    let evidence_by_id = evidence
        .iter()
        .map(|unit| (&unit.id, unit))
        .collect::<HashMap<_, _>>();
    let mut spans = Vec::new();

    for chunk in chunks {
        let mut search_start = 0;
        for evidence_id in &chunk.evidence_unit_ids {
            let Some(unit) = evidence_by_id.get(evidence_id) else {
                continue;
            };
            let Some(start) = chunk.text[search_start..]
                .find(&unit.text)
                .map(|offset| search_start + offset)
            else {
                continue;
            };
            let end = start + unit.text.len();
            spans.push(ChunkEvidenceSpan {
                chunk_id: chunk.id.clone(),
                evidence_id: unit.id.clone(),
                chunk_byte_start: start as u64,
                chunk_byte_end: end as u64,
                evidence_byte_start: 0,
                evidence_byte_end: unit.text.len() as u64,
                evidence_text_hash: unit.text_hash.clone(),
                locator: unit.locator.clone(),
                trust: if unit.derived_from.is_some() {
                    EvidenceSpanTrust::Derived
                } else {
                    EvidenceSpanTrust::Direct
                },
            });
            search_start = end;
        }
    }
    spans
}

pub fn deterministic_chunk_hash(
    chunk_type: ChunkType,
    text: &str,
    heading_path: &[String],
    evidence_identity_hashes: &[String],
) -> String {
    let mut input = String::new();
    input.push_str(CHUNKER_VERSION);
    input.push('\0');
    input.push_str(chunk_type_label(&chunk_type));
    input.push('\0');
    for heading in heading_path {
        input.push_str(&heading.len().to_string());
        input.push(':');
        input.push_str(heading);
        input.push('\0');
    }
    input.push('\0');
    for evidence_hash in evidence_identity_hashes {
        input.push_str(evidence_hash);
        input.push('\0');
    }
    input.push('\0');
    input.push_str(text);
    hex_sha256(input.as_bytes())
}

pub fn unique_chunk_id(
    source_id: &SourceId,
    kind: &str,
    chunk_hash: &str,
    counts: &mut HashMap<String, usize>,
) -> ChunkId {
    let stable = format!("{}:{kind}:{}", source_id.0, &chunk_hash[..16]);
    let count = counts.entry(stable.clone()).or_insert(0);
    let id = if *count == 0 {
        stable
    } else {
        format!("{stable}:{}", *count)
    };
    *count += 1;
    ChunkId(id)
}

fn chunk_type_label(chunk_type: &ChunkType) -> &'static str {
    match chunk_type {
        ChunkType::Child => "child",
        ChunkType::Parent => "parent",
    }
}

fn evidence_identity_hash(unit: &EvidenceUnit) -> String {
    match &unit.locator {
        SourceLocator::Markdown { block_hash, .. } => format!("markdown:{block_hash}"),
        _ => format!("evidence:{}:{}", unit.id.0, unit.text_hash),
    }
}

#[cfg(test)]
#[path = "chunker_tests.rs"]
mod tests;
