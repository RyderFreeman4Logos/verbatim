use crate::types::{Chunk, ChunkId, ChunkType, EvidenceId, EvidenceUnit, SourceId};

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
    let mut child_counter: usize = 0;

    for section in &sections {
        let children = build_children(source_id, section, config, &mut child_counter);
        for child in &children {
            for eid in &child.evidence_unit_ids {
                all_links.push((child.id.clone(), eid.clone()));
            }
        }
        all_children.extend(children);
    }

    let mut all_chunks = Vec::new();
    let mut parent_counter: usize = 0;
    for group in all_children.chunks(config.parent_children_count) {
        let parent = build_parent(source_id, group, &mut parent_counter);
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
    counter: &mut usize,
) -> Vec<Chunk> {
    let target_chars = config.child_target_tokens * CHARS_PER_TOKEN;
    let overlap_chars = config.child_overlap_tokens * CHARS_PER_TOKEN;
    let mut children = Vec::new();
    let mut current_text = String::new();
    let mut current_evidence: Vec<EvidenceId> = Vec::new();
    let mut current_heading: Vec<String> = Vec::new();

    for unit in section {
        let would_exceed = current_text.len() + unit.text.len() > target_chars + target_chars / 5;

        if would_exceed && !current_text.is_empty() {
            children.push(make_child(
                source_id,
                counter,
                &current_text,
                &current_evidence,
                &current_heading,
            ));

            let overlap_start = floor_char_boundary(
                &current_text,
                current_text.len().saturating_sub(overlap_chars),
            );
            let overlap = current_text[overlap_start..].to_string();
            current_text = overlap;
            current_evidence.clear();
        }

        if !current_text.is_empty() {
            current_text.push(' ');
        }
        current_text.push_str(&unit.text);
        current_evidence.push(unit.id.clone());
        if current_heading.is_empty() {
            current_heading = unit.heading_path.clone();
        }
    }

    if !current_text.trim().is_empty() {
        children.push(make_child(
            source_id,
            counter,
            &current_text,
            &current_evidence,
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
    counter: &mut usize,
    text: &str,
    evidence_ids: &[EvidenceId],
    heading_path: &[String],
) -> Chunk {
    let id = ChunkId(format!("{}-child-{}", source_id.0, counter));
    *counter += 1;
    Chunk {
        id,
        source_id: source_id.clone(),
        text: text.trim().to_string(),
        context_text: None,
        token_count: estimate_tokens(text),
        chunk_type: ChunkType::Child,
        parent_chunk_id: None,
        heading_path: heading_path.to_vec(),
        evidence_unit_ids: evidence_ids.to_vec(),
    }
}

fn build_parent(source_id: &SourceId, children: &[Chunk], counter: &mut usize) -> Chunk {
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
    let id = ChunkId(format!("{}-parent-{}", source_id.0, counter));
    *counter += 1;
    Chunk {
        id,
        source_id: source_id.clone(),
        text: text.trim().to_string(),
        context_text: None,
        token_count: estimate_tokens(&text),
        chunk_type: ChunkType::Parent,
        parent_chunk_id: None,
        heading_path,
        evidence_unit_ids: evidence_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EvidenceKind, SourceLocator};

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
}
