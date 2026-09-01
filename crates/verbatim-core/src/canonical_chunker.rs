//! Unit-aligned chunker for canonical-reference sources.
//!
//! Unlike the generic [`chunk_evidence`] which splits by estimated token
//! targets with token-budget overlap, this chunker groups whole canonical
//! units (e.g., Bible verses) into chunks and overlaps by whole units.
//!
//! Hard boundary: never cross top-level work boundaries (e.g., Bible books).
//! Soft boundary: prefer not to cross mid-level boundaries (e.g., chapters).

use anyhow::Result;
use std::collections::HashMap;

use crate::chunker::{
    deterministic_chunk_hash, estimate_spaced_tokens, estimate_tokens, persist_spans_checked,
    unique_chunk_id, ChunkOutput, ChunkWithSpans, TextWithSpans,
};
use crate::types::{
    CanonicalLocator, Chunk, ChunkType, EvidenceId, EvidenceUnit, SourceId, SourceLocator,
};

/// Stable identity for the canonical unit chunking algorithm.
pub const CANONICAL_CHUNKER_VERSION: &str = "canonical-unit-v1";

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
) -> Result<ChunkOutput> {
    if evidence.is_empty() {
        return Ok(ChunkOutput {
            chunks: Vec::new(),
            links: Vec::new(),
            evidence_spans: Vec::new(),
        });
    }

    // Split into groups by hard boundary (book identity).
    let groups = split_by_hard_boundary(evidence);

    let mut all_chunks = Vec::new();
    let mut all_links = Vec::new();
    let mut id_counts = HashMap::new();

    for group in &groups {
        let mut group_children = Vec::new();
        // Further split by soft boundary (chapter) when chunks would be too large.
        let sub_groups = split_by_soft_boundary(group, config);

        for sub_group in sub_groups {
            let children = build_canonical_children(source_id, &sub_group, config, &mut id_counts);
            for child in &children {
                for eid in &child.chunk.evidence_unit_ids {
                    all_links.push((child.chunk.id.clone(), eid.clone()));
                }
            }
            group_children.extend(children);
        }

        // A parent is an organizational grouping, so hard document boundaries
        // apply to it just as they do to children.  Building parents per group
        // prevents the last child of one work and the first child of another
        // from sharing a parent when the boundary falls on a five-child batch.
        let parent_group_size = 5;
        for children_batch in group_children.chunks(parent_group_size) {
            let parent = build_canonical_parent(source_id, children_batch, &mut id_counts);
            let parent_id = parent.chunk.id.clone();
            for eid in &parent.chunk.evidence_unit_ids {
                all_links.push((parent_id.clone(), eid.clone()));
            }
            all_chunks.push(parent);
            for child in children_batch {
                let mut child_with_parent = child.clone();
                child_with_parent.chunk.parent_chunk_id = Some(parent_id.clone());
                all_chunks.push(child_with_parent);
            }
        }
    }

    let mut chunks = Vec::with_capacity(all_chunks.len());
    let mut evidence_spans = Vec::new();
    for chunk_with_spans in all_chunks {
        evidence_spans.extend(persist_spans_checked(
            &chunk_with_spans.chunk,
            &chunk_with_spans.spans,
            evidence,
        )?);
        chunks.push(chunk_with_spans.chunk);
    }

    Ok(ChunkOutput {
        evidence_spans,
        chunks,
        links: all_links,
    })
}

/// Split evidence into groups that must never be in the same chunk.
/// Hard boundary: different book value or ordinal (top-level work).
fn split_by_hard_boundary(evidence: &[EvidenceUnit]) -> Vec<Vec<EvidenceUnit>> {
    if evidence.is_empty() {
        return Vec::new();
    }

    let mut groups: Vec<Vec<EvidenceUnit>> = vec![Vec::new()];
    let mut current_book_key = None;

    for unit in evidence {
        let book_key = book_boundary_key(unit);
        if groups.last().is_some_and(|group| !group.is_empty())
            && (book_key.is_none() || book_key != current_book_key)
        {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(unit.clone());
        current_book_key = book_key;
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
        let token_overflow =
            current_tokens.saturating_add(unit_tokens) > config.target_tokens.saturating_mul(2);

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
) -> Vec<ChunkWithSpans> {
    let target_tokens = config
        .target_tokens
        .saturating_add(config.target_tokens / 5);
    let mut children = Vec::new();
    let mut current_units: Vec<&EvidenceUnit> = Vec::new();

    for unit in units {
        let candidate_tokens = estimate_spaced_tokens(
            current_units
                .iter()
                .map(|current| current.text.as_str())
                .chain(std::iter::once(unit.text.as_str())),
        );
        let would_exceed = candidate_tokens > target_tokens && !current_units.is_empty();
        let too_many = current_units.len() >= config.max_units_per_child;

        if would_exceed || too_many {
            children.push(make_canonical_child(source_id, &current_units, id_counts));
            // Overlap: carry last `overlap_units` units into the next chunk.
            let overlap = config
                .overlap_units
                .min(current_units.len().saturating_sub(1));
            current_units = current_units[current_units.len() - overlap..].to_vec();
        }

        current_units.push(unit);
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
) -> ChunkWithSpans {
    let mut text_with_spans = TextWithSpans::default();
    for unit in units {
        text_with_spans.append(unit);
    }
    let (text, spans) = text_with_spans.trimmed();
    let evidence_ids: Vec<EvidenceId> = units.iter().map(|u| u.id.clone()).collect();
    let evidence_hashes: Vec<String> = units.iter().map(|u| evidence_identity_hash(u)).collect();
    let heading_path = units
        .first()
        .map(|u| u.heading_path.clone())
        .unwrap_or_default();
    let chunk_hash =
        deterministic_chunk_hash(ChunkType::Child, &text, &heading_path, &evidence_hashes);
    let id = unique_chunk_id(source_id, "child", &chunk_hash, id_counts);
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
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path,
            evidence_unit_ids: evidence_ids,
        },
    }
}

fn build_canonical_parent(
    source_id: &SourceId,
    children: &[ChunkWithSpans],
    id_counts: &mut HashMap<String, usize>,
) -> ChunkWithSpans {
    let mut text_with_spans = TextWithSpans::default();
    for child in children {
        text_with_spans.append_composed(&child.chunk.text, &child.spans);
    }
    let (text, spans) = text_with_spans.trimmed();
    let evidence_ids: Vec<EvidenceId> = children
        .iter()
        .flat_map(|c| c.chunk.evidence_unit_ids.iter().cloned())
        .collect();
    let heading_path = children
        .first()
        .map(|c| c.chunk.heading_path.clone())
        .unwrap_or_default();
    let child_hashes = children
        .iter()
        .map(|c| c.chunk.chunk_hash.clone())
        .collect::<Vec<_>>();
    let chunk_hash =
        deterministic_chunk_hash(ChunkType::Parent, &text, &heading_path, &child_hashes);
    let id = unique_chunk_id(source_id, "parent", &chunk_hash, id_counts);
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
            evidence_unit_ids: evidence_ids,
        },
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Extract the normalized book hard-boundary key from a canonical locator.
fn book_boundary_key(unit: &EvidenceUnit) -> Option<(&str, Option<u32>)> {
    canonical_component(unit, "book").map(|component| (component.value.trim(), component.ordinal))
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
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn make_verse(
        source_id: &SourceId,
        position: u32,
        book: &str,
        book_ord: impl Into<Option<u32>>,
        chapter: u32,
        verse: u32,
        text: &str,
        heading: Option<&str>,
    ) -> EvidenceUnit {
        let components = vec![
            crate::types::ReferenceComponent {
                level: "book".into(),
                value: book.into(),
                ordinal: book_ord.into(),
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
            language: None,
            position,
            annotations: Default::default(),
        }
    }

    fn assert_source_content_resolves_to_evidence_spans(
        output: &ChunkOutput,
        evidence: &[EvidenceUnit],
    ) {
        let evidence_by_id = evidence
            .iter()
            .map(|unit| (&unit.id, unit))
            .collect::<HashMap<_, _>>();

        for chunk in &output.chunks {
            let spans = output
                .evidence_spans
                .iter()
                .filter(|span| span.chunk_id == chunk.id)
                .collect::<Vec<_>>();
            let mut covered = vec![false; chunk.text.len()];

            for span in spans {
                let chunk_start = span.chunk_byte_start as usize;
                let chunk_end = span.chunk_byte_end as usize;
                let evidence_start = span.evidence_byte_start as usize;
                let evidence_end = span.evidence_byte_end as usize;
                let unit = evidence_by_id
                    .get(&span.evidence_id)
                    .expect("span evidence must persist");

                assert!(chunk.text.is_char_boundary(chunk_start));
                assert!(chunk.text.is_char_boundary(chunk_end));
                assert!(unit.text.is_char_boundary(evidence_start));
                assert!(unit.text.is_char_boundary(evidence_end));
                assert_eq!(span.evidence_text_hash, unit.text_hash);
                assert_eq!(span.locator, unit.locator);
                assert_eq!(
                    &chunk.text[chunk_start..chunk_end],
                    &unit.text[evidence_start..evidence_end],
                    "span must resolve to the exact persisted evidence substring"
                );
                covered[chunk_start..chunk_end].fill(true);
            }

            for (offset, character) in chunk.text.char_indices() {
                if !character.is_whitespace() {
                    assert!(
                        covered[offset..offset + character.len_utf8()]
                            .iter()
                            .all(|covered| *covered),
                        "unresolved source-derived character {character:?} in {}",
                        chunk.id.0
                    );
                }
            }
        }
    }

    #[test]
    fn canonical_whitespace_repeated_text_keeps_child_and_parent_provenance() {
        let source_id = SourceId("canonical-whitespace".into());
        let unit_a = make_verse(
            &source_id,
            0,
            "John",
            43,
            1,
            1,
            "  repeated",
            Some("John 1"),
        );
        let unit_b = make_verse(&source_id, 1, "John", 43, 1, 2, "repeated", Some("John 1"));
        let evidence = vec![unit_a.clone(), unit_b.clone()];
        let output = chunk_canonical_units(
            &source_id,
            &evidence,
            &CanonicalChunkerConfig {
                target_tokens: 100,
                overlap_units: 0,
                max_units_per_child: 2,
            },
        )
        .expect("canonical provenance must resolve");

        for chunk in output
            .chunks
            .iter()
            .filter(|chunk| matches!(chunk.chunk_type, ChunkType::Child | ChunkType::Parent))
        {
            assert_eq!(chunk.text, "repeated repeated");
            let a_span = output
                .evidence_spans
                .iter()
                .find(|span| span.chunk_id == chunk.id && span.evidence_id == unit_a.id)
                .expect("unit A span");
            let b_span = output
                .evidence_spans
                .iter()
                .find(|span| span.chunk_id == chunk.id && span.evidence_id == unit_b.id)
                .expect("unit B span");
            assert_eq!((a_span.chunk_byte_start, a_span.chunk_byte_end), (0, 8));
            assert_eq!(
                (a_span.evidence_byte_start, a_span.evidence_byte_end),
                (2, 10)
            );
            assert_eq!((b_span.chunk_byte_start, b_span.chunk_byte_end), (9, 17));
            assert_eq!(
                (b_span.evidence_byte_start, b_span.evidence_byte_end),
                (0, 8)
            );
        }
        assert_source_content_resolves_to_evidence_spans(&output, &evidence);
    }

    #[test]
    fn canonical_provenance_mismatch_fails_before_ingest() {
        let source_id = SourceId("canonical-mismatch".into());
        let unit = make_verse(&source_id, 0, "John", 43, 1, 1, "repeated", Some("John 1"));
        let output = chunk_canonical_units(
            &source_id,
            std::slice::from_ref(&unit),
            &CanonicalChunkerConfig::default(),
        )
        .expect("canonical provenance must resolve before tampering");
        let mut chunk = output
            .chunks
            .iter()
            .find(|chunk| chunk.chunk_type == ChunkType::Child)
            .expect("child chunk")
            .clone();
        chunk.text = "tampered".into();

        let mut text_with_spans = TextWithSpans::default();
        text_with_spans.append(&unit);
        let (_, spans) = text_with_spans.trimmed();
        let error = persist_spans_checked(&chunk, &spans, std::slice::from_ref(&unit))
            .expect_err("mismatched canonical provenance must fail ingestion");
        assert!(error
            .to_string()
            .contains("does not resolve to identical text"));
    }

    #[test]
    fn cjk_units_use_token_budget_for_canonical_children() {
        let source_id = SourceId("canonical-cjk".into());
        let evidence = vec![
            make_verse(&source_id, 0, "John", 43, 1, 1, "中文测试", Some("John 1")),
            make_verse(&source_id, 1, "John", 43, 1, 2, "保守估算", Some("John 1")),
        ];
        let output = chunk_canonical_units(
            &source_id,
            &evidence,
            &CanonicalChunkerConfig {
                target_tokens: 6,
                overlap_units: 0,
                max_units_per_child: 20,
            },
        )
        .expect("canonical CJK chunks must preserve provenance");

        assert_eq!(
            output
                .chunks
                .iter()
                .filter(|chunk| chunk.chunk_type == ChunkType::Child)
                .count(),
            2
        );
    }

    fn repeated_unicode_text() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just("你".to_string()),
                Just("é".to_string()),
                Just("🙂".to_string()),
                Just("字".to_string()),
            ],
            1..8,
        )
        .prop_map(|characters| characters.concat())
    }

    fn unicode_whitespace() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(" ".to_string()),
            Just("\t".to_string()),
            Just("\u{2003}".to_string()),
            Just("\u{2009}".to_string()),
        ]
    }

    proptest! {
        #[test]
        fn canonical_provenance_property_preserves_utf8_repeated_text_across_boundaries(
            repeated in repeated_unicode_text(),
            leading in unicode_whitespace(),
            trailing in unicode_whitespace(),
            overlap_units in 0usize..3,
            max_units_per_child in 1usize..4,
        ) {
            let source_id = SourceId("canonical-property".into());
            let mut evidence = Vec::new();
            for (book, book_ordinal) in [("John", 43), ("Romans", 45)] {
                for verse in 1..=6 {
                    evidence.push(make_verse(
                        &source_id,
                        evidence.len() as u32,
                        book,
                        book_ordinal,
                        1,
                        verse,
                        &format!("{leading}{repeated}{trailing}"),
                        Some(book),
                    ));
                }
            }
            let output = chunk_canonical_units(
                &source_id,
                &evidence,
                &CanonicalChunkerConfig {
                    target_tokens: 1,
                    overlap_units,
                    max_units_per_child,
                },
            )
            .expect("canonical provenance must resolve");

            prop_assert!(output.chunks.iter().any(|chunk| chunk.chunk_type == ChunkType::Parent));
            assert_source_content_resolves_to_evidence_spans(&output, &evidence);
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
        let output = chunk_canonical_units(&sid, &verses, &config)
            .expect("canonical provenance must resolve");
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
        let output = chunk_canonical_units(&sid, &units, &config)
            .expect("canonical provenance must resolve");

        // Check that no child *or parent* spans both John and Romans. Parents
        // are retrieval units too, so accepting a cross-book parent would
        // silently defeat the hard-boundary contract.
        for chunk in &output.chunks {
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
    fn missing_book_ordinals_do_not_cross_book_boundary() {
        let sid = SourceId("test-source".into());
        let john = make_verse(&sid, 1, "John", None, 1, 1, "John 1:1 text.", Some("Jn"));
        let romans = make_verse(
            &sid,
            2,
            "Romans",
            None,
            1,
            1,
            "Romans 1:1 text.",
            Some("Rom"),
        );
        let john_id = john.id.clone();
        let romans_id = romans.id.clone();
        let output =
            chunk_canonical_units(&sid, &[john, romans], &CanonicalChunkerConfig::default())
                .expect("canonical provenance must resolve");

        for chunk in output
            .chunks
            .iter()
            .filter(|chunk| matches!(chunk.chunk_type, ChunkType::Child | ChunkType::Parent))
        {
            assert!(
                !(chunk.evidence_unit_ids.contains(&john_id)
                    && chunk.evidence_unit_ids.contains(&romans_id)),
                "chunk {:?} spans books whose ordinals are missing",
                chunk.id
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
        let output = chunk_canonical_units(&sid, &verses, &config)
            .expect("canonical provenance must resolve");
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
        let output = chunk_canonical_units(&sid, &[], &CanonicalChunkerConfig::default())
            .expect("empty canonical evidence must resolve");
        assert!(output.chunks.is_empty());
        assert!(output.links.is_empty());
    }
}
