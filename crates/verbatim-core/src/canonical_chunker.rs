//! Unit-aligned chunker for canonical-reference sources.
//!
//! Unlike the generic [`chunk_evidence`] which splits by approximate token
//! targets with character-based overlap, this chunker groups whole canonical
//! units (e.g., Bible verses) into chunks and overlaps by whole units.
//!
//! Hard boundary: never cross top-level work boundaries (e.g., Bible books).
//! Soft boundary: prefer not to cross mid-level boundaries (e.g., chapters).

use std::collections::HashMap;

use crate::chunker::{deterministic_chunk_hash, estimate_tokens, unique_chunk_id, ChunkOutput};
use crate::types::{
    CanonicalLocator, Chunk, ChunkType, EvidenceId, EvidenceUnit, SourceId, SourceLocator,
};

/// Configuration for unit-aligned chunking.
#[derive(Clone, Debug)]
pub struct CanonicalChunkerConfig {
    /// Target token count per child chunk (approximate, never splits a unit).
    pub target_tokens: usize,
    /// Number of units to overlap between consecutive chunks.
    pub overlap_units: usize,
    /// Maximum units per child chunk.
    pub max_units_per_child: usize,
}

impl Default for CanonicalChunkerConfig {
    fn default() -> Self {
        Self {
            target_tokens: 300,
            overlap_units: 2,
            max_units_per_child: 20,
        }
    }
}

/// Chunk canonical evidence units by whole units, not character overlap.
///
/// - Never splits a canonical unit.
/// - Overlap uses whole units.
/// - Never crosses top-level work boundaries (book level).
/// - Prefers not to cross chapter boundaries.
pub fn chunk_canonical_units(
    source_id: &SourceId,
    evidence: &[EvidenceUnit],
    config: &CanonicalChunkerConfig,
) -> ChunkOutput {
    if evidence.is_empty() {
        return ChunkOutput {
            chunks: Vec::new(),
            links: Vec::new(),
        };
    }

    // Split into groups by hard boundary (book ordinal).
    let groups = split_by_hard_boundary(evidence);

    let mut all_children = Vec::new();
    let mut all_links = Vec::new();
    let mut id_counts = HashMap::new();

    for group in &groups {
        // Further split by soft boundary (chapter) when chunks would be too large.
        let sub_groups = split_by_soft_boundary(group, config);

        for sub_group in sub_groups {
            let children = build_canonical_children(source_id, &sub_group, config, &mut id_counts);
            for child in &children {
                for eid in &child.evidence_unit_ids {
                    all_links.push((child.id.clone(), eid.clone()));
                }
            }
            all_children.extend(children);
        }
    }

    // Build parents from consecutive child groups.
    let mut all_chunks = Vec::new();
    let parent_group_size = 5;
    for children_batch in all_children.chunks(parent_group_size) {
        let parent = build_canonical_parent(source_id, children_batch, &mut id_counts);
        let parent_id = parent.id.clone();
        for eid in &parent.evidence_unit_ids {
            all_links.push((parent_id.clone(), eid.clone()));
        }
        all_chunks.push(parent.clone());
        for child in children_batch {
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

/// Split evidence into groups that must never be in the same chunk.
/// Hard boundary: different book ordinal (top-level work).
fn split_by_hard_boundary(evidence: &[EvidenceUnit]) -> Vec<Vec<EvidenceUnit>> {
    if evidence.is_empty() {
        return Vec::new();
    }

    let mut groups: Vec<Vec<EvidenceUnit>> = vec![Vec::new()];
    let mut current_book: Option<u32> = None;

    for unit in evidence {
        let book_ord = book_ordinal(unit);
        // Start a new group when the book ordinal changes (and we already have units).
        if book_ord != current_book && current_book.is_some() {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(unit.clone());
        current_book = book_ord;
    }
    groups
}

/// Split a hard-boundary group by soft boundary (chapter) when needed.
fn split_by_soft_boundary(
    group: &[EvidenceUnit],
    config: &CanonicalChunkerConfig,
) -> Vec<Vec<EvidenceUnit>> {
    let mut sub_groups: Vec<Vec<EvidenceUnit>> = Vec::new();
    let mut current: Vec<EvidenceUnit> = Vec::new();
    let mut current_chapter: Option<u32> = None;
    let mut current_tokens = 0usize;

    for unit in group {
        let chapter = chapter_ordinal(unit);
        let unit_tokens = estimate_tokens(&unit.text) as usize;

        // Split on chapter boundary, max units, or token overflow.
        let crosses_chapter = chapter != current_chapter && current_chapter.is_some();
        let too_many_units = current.len() >= config.max_units_per_child;
        let token_overflow = current_tokens + unit_tokens > config.target_tokens * 2;

        if !current.is_empty() && (crosses_chapter || too_many_units || token_overflow) {
            sub_groups.push(std::mem::take(&mut current));
            current_tokens = 0;
        }

        current.push(unit.clone());
        current_tokens += unit_tokens;
        current_chapter = chapter;
    }
    if !current.is_empty() {
        sub_groups.push(current);
    }
    sub_groups
}

/// Build child chunks from a sub-group of consecutive canonical units.
fn build_canonical_children(
    source_id: &SourceId,
    units: &[EvidenceUnit],
    config: &CanonicalChunkerConfig,
    id_counts: &mut HashMap<String, usize>,
) -> Vec<Chunk> {
    let target_chars = config.target_tokens * 4; // CHARS_PER_TOKEN
    let mut children = Vec::new();
    let mut current_units: Vec<&EvidenceUnit> = Vec::new();
    let mut current_len = 0usize;

    for unit in units {
        let would_exceed = current_len + unit.text.len() > target_chars + target_chars / 5
            && !current_units.is_empty();
        let too_many = current_units.len() >= config.max_units_per_child;

        if would_exceed || too_many {
            children.push(make_canonical_child(source_id, &current_units, id_counts));
            // Overlap: carry last `overlap_units` units into the next chunk.
            let overlap = config
                .overlap_units
                .min(current_units.len().saturating_sub(1));
            current_units = current_units[current_units.len() - overlap..].to_vec();
            current_len = current_units.iter().map(|u| u.text.len() + 1).sum();
        }

        current_units.push(unit);
        current_len += unit.text.len() + 1; // +1 for space
    }

    if !current_units.is_empty() {
        children.push(make_canonical_child(source_id, &current_units, id_counts));
    }

    children
}

fn make_canonical_child(
    source_id: &SourceId,
    units: &[&EvidenceUnit],
    id_counts: &mut HashMap<String, usize>,
) -> Chunk {
    let text: String = units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = text.trim().to_string();
    let evidence_ids: Vec<EvidenceId> = units.iter().map(|u| u.id.clone()).collect();
    let evidence_hashes: Vec<String> = units.iter().map(|u| evidence_identity_hash(u)).collect();
    let heading_path = units
        .first()
        .map(|u| u.heading_path.clone())
        .unwrap_or_default();
    let chunk_hash =
        deterministic_chunk_hash(ChunkType::Child, &trimmed, &heading_path, &evidence_hashes);
    let id = unique_chunk_id(source_id, "child", &chunk_hash, id_counts);
    Chunk {
        id,
        source_id: source_id.clone(),
        chunk_hash,
        embedding_input_hash: None,
        text: trimmed,
        context_text: None,
        token_count: estimate_tokens(&text),
        chunk_type: ChunkType::Child,
        parent_chunk_id: None,
        heading_path,
        evidence_unit_ids: evidence_ids,
    }
}

fn build_canonical_parent(
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
        .map(|c| c.chunk_hash.clone())
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

// ── Helpers ──────────────────────────────────────────────────────────

/// Extract book ordinal from a canonical locator (level == "book").
fn book_ordinal(unit: &EvidenceUnit) -> Option<u32> {
    canonical_component(unit, "book").and_then(|c| c.ordinal)
}

/// Extract chapter ordinal from a canonical locator (level == "chapter").
fn chapter_ordinal(unit: &EvidenceUnit) -> Option<u32> {
    canonical_component(unit, "chapter").and_then(|c| c.ordinal)
}

/// Get a reference component by level name from the locator.
fn canonical_component<'a>(
    unit: &'a EvidenceUnit,
    level: &str,
) -> Option<&'a crate::types::ReferenceComponent> {
    match &unit.locator {
        SourceLocator::Canonical { locator } => locator.start.iter().find(|c| c.level == level),
        _ => None,
    }
}

/// Extract the display citation from a canonical locator.
pub fn canonical_display(unit: &EvidenceUnit) -> Option<&str> {
    match &unit.locator {
        SourceLocator::Canonical { locator } => Some(&locator.display),
        _ => None,
    }
}

/// Extract the full canonical locator from a unit.
pub fn canonical_locator(unit: &EvidenceUnit) -> Option<&CanonicalLocator> {
    match &unit.locator {
        SourceLocator::Canonical { locator } => Some(locator),
        _ => None,
    }
}

fn evidence_identity_hash(unit: &EvidenceUnit) -> String {
    unit.text_hash.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{hex_sha256, EvidenceId, EvidenceKind, SourceId};

    fn make_verse(
        source_id: &SourceId,
        position: u32,
        book: &str,
        book_ord: u32,
        chapter: u32,
        verse: u32,
        text: &str,
        heading: Option<&str>,
    ) -> EvidenceUnit {
        let components = vec![
            crate::types::ReferenceComponent {
                level: "book".into(),
                value: book.into(),
                ordinal: Some(book_ord),
            },
            crate::types::ReferenceComponent {
                level: "chapter".into(),
                value: chapter.to_string(),
                ordinal: Some(chapter),
            },
            crate::types::ReferenceComponent {
                level: "verse".into(),
                value: verse.to_string(),
                ordinal: Some(verse),
            },
        ];
        let display = format!("{book} {chapter}:{verse}");
        let normalized = format!("{}:{chapter}:{verse}", book.to_lowercase());
        EvidenceUnit {
            id: EvidenceId(format!("{}:ev:{position}", source_id.0)),
            source_id: source_id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Canonical {
                locator: CanonicalLocator::single_unit(
                    "bible", "TEST", components, display, normalized,
                ),
            },
            text: text.into(),
            text_hash: hex_sha256(text.as_bytes()),
            heading_path: heading.map(|h| vec![h.to_string()]).unwrap_or_default(),
            position,
        }
    }

    #[test]
    fn never_splits_a_unit() {
        let sid = SourceId("test-source".into());
        let verses: Vec<EvidenceUnit> = (1..=10)
            .map(|v| {
                make_verse(
                    &sid,
                    v,
                    "John",
                    43,
                    3,
                    v,
                    &format!("Verse {v} text content here."),
                    Some("Section"),
                )
            })
            .collect();
        let config = CanonicalChunkerConfig::default();
        let output = chunk_canonical_units(&sid, &verses, &config);
        // Every child chunk should reference whole evidence units
        for chunk in &output.chunks {
            if chunk.chunk_type == ChunkType::Child {
                assert!(!chunk.evidence_unit_ids.is_empty());
            }
        }
    }

    #[test]
    fn does_not_cross_book_boundary() {
        let sid = SourceId("test-source".into());
        let mut units = Vec::new();
        // John chapter 1, 5 verses
        for v in 1..=5 {
            units.push(make_verse(
                &sid,
                v,
                "John",
                43,
                1,
                v,
                &format!("John 1:{v} text."),
                Some("Jn"),
            ));
        }
        // Romans chapter 1, 5 verses
        for v in 1..=5 {
            units.push(make_verse(
                &sid,
                v + 10,
                "Romans",
                45,
                1,
                v,
                &format!("Romans 1:{v} text."),
                Some("Rom"),
            ));
        }
        let config = CanonicalChunkerConfig::default();
        let output = chunk_canonical_units(&sid, &units, &config);

        // Check that no chunk spans both John and Romans
        for chunk in &output.chunks {
            if chunk.chunk_type != ChunkType::Child {
                continue;
            }
            let evidence_ids = &chunk.evidence_unit_ids;
            let books: std::collections::HashSet<&str> = evidence_ids
                .iter()
                .filter_map(|eid| {
                    // Evidence IDs like "test-source:ev:1" map back to units
                    units
                        .iter()
                        .find(|u| &u.id == eid)
                        .and_then(|u| canonical_component(u, "book").map(|c| c.value.as_str()))
                })
                .collect();
            assert_eq!(
                books.len(),
                1,
                "chunk {:?} spans multiple books: {:?}",
                chunk.id,
                books
            );
        }
    }

    #[test]
    fn overlap_uses_whole_units() {
        let sid = SourceId("test-source".into());
        // Create enough verses to force multiple chunks
        let verses: Vec<EvidenceUnit> = (1..=30)
            .map(|v| {
                make_verse(
                    &sid,
                    v,
                    "Psalms",
                    19,
                    1,
                    v,
                    &format!("Psalm verse number {v} with enough text to fill a chunk target size that will create multiple segments when accumulated beyond the configured token limit."),
                    Some("Psalm 1"),
                )
            })
            .collect();
        let config = CanonicalChunkerConfig {
            target_tokens: 50, // small to force multiple chunks
            overlap_units: 2,
            max_units_per_child: 10,
        };
        let output = chunk_canonical_units(&sid, &verses, &config);
        let child_count = output
            .chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Child)
            .count();
        assert!(
            child_count >= 2,
            "expected multiple children, got {child_count}"
        );
    }

    #[test]
    fn empty_evidence_produces_empty_output() {
        let sid = SourceId("empty".into());
        let output = chunk_canonical_units(&sid, &[], &CanonicalChunkerConfig::default());
        assert!(output.chunks.is_empty());
        assert!(output.links.is_empty());
    }
}
