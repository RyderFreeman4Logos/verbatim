use std::collections::HashMap;

use crate::types::{
    hex_sha256, Chunk, ChunkId, ChunkType, EvidenceId, EvidenceUnit, SourceId, SourceLocator,
};

pub const CHUNKER_VERSION: &str = "parent-child-v2";
const DEFAULT_CHILD_TARGET: usize = 300;
const DEFAULT_CHILD_OVERLAP: usize = 80;
const DEFAULT_PARENT_CHILDREN: usize = 5;
const CHARS_PER_TOKEN: usize = 4;

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
}

pub(crate) fn estimate_tokens(text: &str) -> u32 {
    (text.len() / CHARS_PER_TOKEN) as u32
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
        };
    }

    let sections = split_by_top_heading(evidence);
    let mut all_children = Vec::new();
    let mut all_links = Vec::new();
    let mut child_id_counts = HashMap::new();

    for section in &sections {
        let children = build_children(source_id, section, config, &mut child_id_counts);
        for child in &children {
            for eid in &child.evidence_unit_ids {
                all_links.push((child.id.clone(), eid.clone()));
            }
        }
        all_children.extend(children);
    }

    let mut all_chunks = Vec::new();
    let mut parent_id_counts = HashMap::new();
    for group in all_children.chunks(config.parent_children_count) {
        let parent = build_parent(source_id, group, &mut parent_id_counts);
        let parent_id = parent.id.clone();
        for eid in &parent.evidence_unit_ids {
            all_links.push((parent_id.clone(), eid.clone()));
        }
        all_chunks.push(parent.clone());
        for child in group {
            let mut child_with_parent = child.clone();
            child_with_parent.parent_chunk_id = Some(parent_id.clone());
            all_chunks.push(child_with_parent);
        }
    }

    ChunkOutput {
        chunks: all_chunks,
        links: all_links,
    }
}

fn split_by_top_heading(evidence: &[EvidenceUnit]) -> Vec<Vec<&EvidenceUnit>> {
    let mut sections: Vec<Vec<&EvidenceUnit>> = Vec::new();
    let mut current: Vec<&EvidenceUnit> = Vec::new();

    let top_heading = evidence
        .iter()
        .filter(|e| !e.heading_path.is_empty())
        .map(|e| &e.heading_path[0])
        .next();

    for unit in evidence {
        let is_new_section = !unit.heading_path.is_empty()
            && top_heading.is_some_and(|th| unit.heading_path[0] != *th || current.is_empty())
            && !current.is_empty()
            && unit.heading_path.len() == 1;

        if is_new_section {
            sections.push(std::mem::take(&mut current));
        }
        current.push(unit);
    }
    if !current.is_empty() {
        sections.push(current);
    }
    sections
}

fn build_children(
    source_id: &SourceId,
    section: &[&EvidenceUnit],
    config: &ChunkerConfig,
    id_counts: &mut HashMap<String, usize>,
) -> Vec<Chunk> {
    let target_chars = config.child_target_tokens * CHARS_PER_TOKEN;
    let overlap_chars = config.child_overlap_tokens * CHARS_PER_TOKEN;
    let mut children = Vec::new();
    let mut current_text = String::new();
    let mut current_evidence: Vec<EvidenceId> = Vec::new();
    let mut current_evidence_hashes: Vec<String> = Vec::new();
    let mut current_heading: Vec<String> = Vec::new();

    for unit in section {
        let would_exceed = current_text.len() + unit.text.len() > target_chars + target_chars / 5;

        if would_exceed && !current_text.is_empty() {
            children.push(make_child(
                source_id,
                id_counts,
                &current_text,
                &current_evidence,
                &current_evidence_hashes,
                &current_heading,
            ));

            let overlap_start = floor_char_boundary(
                &current_text,
                current_text.len().saturating_sub(overlap_chars),
            );
            let overlap = current_text[overlap_start..].to_string();
            current_text = overlap;
            current_evidence.clear();
            current_evidence_hashes.clear();
        }

        if !current_text.is_empty() {
            current_text.push(' ');
        }
        current_text.push_str(&unit.text);
        current_evidence.push(unit.id.clone());
        current_evidence_hashes.push(evidence_identity_hash(unit));
        if current_heading.is_empty() {
            current_heading = unit.heading_path.clone();
        }
    }

    if !current_text.trim().is_empty() {
        children.push(make_child(
            source_id,
            id_counts,
            &current_text,
            &current_evidence,
            &current_evidence_hashes,
            &current_heading,
        ));
    }

    children
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut boundary = index.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn make_child(
    source_id: &SourceId,
    id_counts: &mut HashMap<String, usize>,
    text: &str,
    evidence_ids: &[EvidenceId],
    evidence_identity_hashes: &[String],
    heading_path: &[String],
) -> Chunk {
    let trimmed = text.trim().to_string();
    let chunk_hash = deterministic_chunk_hash(
        ChunkType::Child,
        &trimmed,
        heading_path,
        evidence_identity_hashes,
    );
    let id = unique_chunk_id(source_id, "child", &chunk_hash, id_counts);
    Chunk {
        id,
        source_id: source_id.clone(),
        chunk_hash,
        embedding_input_hash: None,
        text: trimmed,
        context_text: None,
        token_count: estimate_tokens(text),
        chunk_type: ChunkType::Child,
        parent_chunk_id: None,
        heading_path: heading_path.to_vec(),
        evidence_unit_ids: evidence_ids.to_vec(),
    }
}

fn build_parent(
    source_id: &SourceId,
    children: &[Chunk],
    id_counts: &mut HashMap<String, usize>,
) -> Chunk {
    let text: String = children
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let evidence_ids: Vec<EvidenceId> = children
        .iter()
        .flat_map(|c| c.evidence_unit_ids.iter().cloned())
        .collect();
    let heading_path = children
        .first()
        .map(|c| c.heading_path.clone())
        .unwrap_or_default();
    let child_hashes = children
        .iter()
        .map(|child| child.chunk_hash.clone())
        .collect::<Vec<_>>();
    let trimmed = text.trim().to_string();
    let chunk_hash =
        deterministic_chunk_hash(ChunkType::Parent, &trimmed, &heading_path, &child_hashes);
    let id = unique_chunk_id(source_id, "parent", &chunk_hash, id_counts);
    Chunk {
        id,
        source_id: source_id.clone(),
        chunk_hash,
        embedding_input_hash: None,
        text: trimmed,
        context_text: None,
        token_count: estimate_tokens(&text),
        chunk_type: ChunkType::Parent,
        parent_chunk_id: None,
        heading_path,
        evidence_unit_ids: evidence_ids,
    }
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

fn unique_chunk_id(
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
mod tests {
    use super::*;
    use crate::types::{EvidenceKind, MarkdownBlockKind, MarkdownHeadingLocator, SourceLocator};

    fn make_evidence(n: usize, heading: &str) -> Vec<EvidenceUnit> {
        (0..n)
            .map(|i| EvidenceUnit {
                id: EvidenceId(format!("ev-{i}")),
                source_id: SourceId("test".into()),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: SourceLocator::Pdf {
                    page: 1,
                    paragraph: i as u32,
                    bbox: None,
                },
                text: format!("Word{i} ").repeat(80),
                text_hash: format!("hash-{i}"),
                heading_path: if heading.is_empty() {
                    vec![]
                } else {
                    vec![heading.to_string()]
                },
                position: i as u32,
            })
            .collect()
    }

    #[test]
    fn produces_parent_child_hierarchy() {
        let evidence = make_evidence(20, "Chapter 1");
        let config = ChunkerConfig::default();
        let output = chunk_evidence(&SourceId("test".into()), &evidence, &config);

        let parents: Vec<_> = output
            .chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Parent)
            .collect();
        let children: Vec<_> = output
            .chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Child)
            .collect();

        assert!(!parents.is_empty());
        assert!(!children.is_empty());

        for child in &children {
            assert!(child.parent_chunk_id.is_some());
        }
    }

    #[test]
    fn child_token_count_in_range() {
        let evidence = make_evidence(20, "Chapter 1");
        let config = ChunkerConfig::default();
        let output = chunk_evidence(&SourceId("test".into()), &evidence, &config);

        for chunk in &output.chunks {
            if chunk.chunk_type == ChunkType::Child {
                let target = config.child_target_tokens as f64;
                assert!(
                    (chunk.token_count as f64) < target * 1.5,
                    "child too large: {} tokens",
                    chunk.token_count
                );
            }
        }
    }

    #[test]
    fn empty_evidence() {
        let output = chunk_evidence(&SourceId("test".into()), &[], &ChunkerConfig::default());
        assert!(output.chunks.is_empty());
        assert!(output.links.is_empty());
    }

    #[test]
    fn links_match_evidence_ids() {
        let evidence = make_evidence(5, "");
        let output = chunk_evidence(
            &SourceId("test".into()),
            &evidence,
            &ChunkerConfig::default(),
        );

        assert!(!output.links.is_empty());
        for (chunk_id, _eid) in &output.links {
            assert!(output.chunks.iter().any(|c| c.id == *chunk_id));
        }
    }

    #[test]
    fn unicode_overlap_starts_on_char_boundary() {
        let mut evidence = make_evidence(2, "中文章节");
        evidence[0].text = "份".repeat(380);
        evidence[1].text = "额".repeat(101);

        let output = chunk_evidence(
            &SourceId("test".into()),
            &evidence,
            &ChunkerConfig::default(),
        );

        assert!(output
            .chunks
            .iter()
            .any(|chunk| chunk.chunk_type == ChunkType::Child));
    }

    #[test]
    fn markdown_chunk_identity_survives_insertion_before_section() {
        let source_id = SourceId("doc".into());
        let original = vec![
            markdown_evidence(&source_id, "intro", "Intro text.", "intro-block", 3, 0),
            markdown_evidence(&source_id, "stable", "Stable text.", "stable-block", 7, 1),
        ];
        let shifted = vec![
            markdown_evidence(
                &source_id,
                "inserted",
                "Inserted text.",
                "inserted-block",
                3,
                0,
            ),
            markdown_evidence(&source_id, "intro", "Intro text.", "intro-block", 7, 1),
            markdown_evidence(&source_id, "stable", "Stable text.", "stable-block", 11, 2),
        ];

        let original_output = chunk_evidence(&source_id, &original, &ChunkerConfig::default());
        let shifted_output = chunk_evidence(&source_id, &shifted, &ChunkerConfig::default());
        let original_stable = child_for_heading(&original_output.chunks, "Stable");
        let shifted_stable = child_for_heading(&shifted_output.chunks, "Stable");

        assert_eq!(original_stable.chunk_hash, shifted_stable.chunk_hash);
        assert_eq!(original_stable.id, shifted_stable.id);
    }

    fn markdown_evidence(
        source_id: &SourceId,
        slug: &str,
        text: &str,
        block_hash: &str,
        line_start: u32,
        position: u32,
    ) -> EvidenceUnit {
        let heading_text = title_case(slug);
        EvidenceUnit {
            id: EvidenceId(format!("ev-{slug}-{line_start}")),
            source_id: source_id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Markdown {
                path: "doc.md".into(),
                line_start,
                line_end: line_start,
                byte_start: 0,
                byte_end: text.len() as u64,
                block_kind: MarkdownBlockKind::Paragraph,
                block_index: position,
                block_hash: block_hash.into(),
                heading_level: Some(1),
                heading_slug: Some(slug.into()),
                heading_path: vec![MarkdownHeadingLocator {
                    level: 1,
                    text: heading_text.clone(),
                    slug: slug.into(),
                    line: line_start.saturating_sub(2),
                }],
            },
            text: text.into(),
            text_hash: format!("{slug}-text-hash"),
            heading_path: vec![heading_text],
            position,
        }
    }

    fn child_for_heading<'a>(chunks: &'a [Chunk], heading: &str) -> &'a Chunk {
        chunks
            .iter()
            .find(|chunk| {
                chunk.chunk_type == ChunkType::Child
                    && chunk.heading_path == vec![heading.to_string()]
            })
            .expect("child chunk for heading")
    }

    fn title_case(slug: &str) -> String {
        let mut chars = slug.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    }
}
