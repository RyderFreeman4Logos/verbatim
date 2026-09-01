//! Locator-based routing and deterministic merge for ingest chunk outputs.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};

use crate::canonical_chunker::{chunk_canonical_units, CanonicalChunkerConfig};
use crate::chunker::{chunk_evidence, ChunkOutput, ChunkerConfig};
use crate::types::{EvidenceUnit, SourceId, SourceLocator};

pub(super) struct PartitionedChunkOutput {
    pub(super) output: ChunkOutput,
    pub(super) canonical_evidence_count: usize,
    pub(super) noncanonical_evidence_count: usize,
    pub(super) strategies_used: Vec<&'static str>,
}

pub(super) fn chunk_searchable_evidence_by_locator(
    source_id: &SourceId,
    evidence: &[EvidenceUnit],
    config: &ChunkerConfig,
    canonical_config: &CanonicalChunkerConfig,
) -> Result<PartitionedChunkOutput> {
    let canonical_evidence_count = evidence
        .iter()
        .filter(|unit| matches!(unit.locator, SourceLocator::Canonical { .. }))
        .count();
    let noncanonical_evidence_count = evidence.len() - canonical_evidence_count;
    let (output, strategies_used) = match (canonical_evidence_count, noncanonical_evidence_count) {
        (0, 0) => (
            ChunkOutput {
                chunks: Vec::new(),
                links: Vec::new(),
                evidence_spans: Vec::new(),
            },
            Vec::new(),
        ),
        (0, _) => (
            chunk_evidence(source_id, evidence, config),
            vec!["chunk_evidence"],
        ),
        (_, 0) => (
            chunk_canonical_units(source_id, evidence, canonical_config)?,
            vec!["chunk_canonical_units"],
        ),
        _ => (
            chunk_mixed_evidence(source_id, evidence, config, canonical_config)?,
            vec!["chunk_canonical_units", "chunk_evidence"],
        ),
    };
    let mut chunk_ids = HashSet::with_capacity(output.chunks.len());
    if !output
        .chunks
        .iter()
        .all(|chunk| chunk_ids.insert(&chunk.id))
    {
        bail!("locator-partitioned chunking produced duplicate chunk ids");
    }

    Ok(PartitionedChunkOutput {
        output,
        canonical_evidence_count,
        noncanonical_evidence_count,
        strategies_used,
    })
}

fn chunk_mixed_evidence(
    source_id: &SourceId,
    evidence: &[EvidenceUnit],
    config: &ChunkerConfig,
    canonical_config: &CanonicalChunkerConfig,
) -> Result<ChunkOutput> {
    let (canonical, noncanonical): (Vec<_>, Vec<_>) = evidence
        .iter()
        .cloned()
        .partition(|unit| matches!(unit.locator, SourceLocator::Canonical { .. }));
    let canonical_output = chunk_canonical_units(source_id, &canonical, canonical_config)?;
    let noncanonical_output = chunk_evidence(source_id, &noncanonical, config);
    let [mut output, other] = if evidence
        .first()
        .is_some_and(|unit| matches!(unit.locator, SourceLocator::Canonical { .. }))
    {
        [canonical_output, noncanonical_output]
    } else {
        [noncanonical_output, canonical_output]
    };
    output.chunks.extend(other.chunks);
    output.links.extend(other.links);
    output.evidence_spans.extend(other.evidence_spans);

    let evidence_positions = evidence
        .iter()
        .enumerate()
        .map(|(index, unit)| (&unit.id, index))
        .collect::<HashMap<_, _>>();
    // Stable sorting retains each chunker's parent-before-child order on ties.
    output.chunks.sort_by_key(|chunk| {
        chunk
            .evidence_unit_ids
            .iter()
            .filter_map(|id| evidence_positions.get(id).copied())
            .min()
            .unwrap_or(usize::MAX)
    });
    output.links.sort_by_key(|(_, evidence_id)| {
        evidence_positions
            .get(evidence_id)
            .copied()
            .unwrap_or(usize::MAX)
    });
    output.evidence_spans.sort_by_key(|span| {
        evidence_positions
            .get(&span.evidence_id)
            .copied()
            .unwrap_or(usize::MAX)
    });
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CanonicalLocator, ChunkType, EvidenceId, EvidenceKind, ReferenceComponent};

    fn text_evidence(source_id: &SourceId, id: &str, text: &str, position: u32) -> EvidenceUnit {
        EvidenceUnit {
            id: EvidenceId(id.into()),
            source_id: source_id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: source_id.0.clone(),
                line_start: position,
                line_end: None,
            },
            text: text.into(),
            text_hash: format!("hash-{id}"),
            heading_path: Vec::new(),
            language: None,
            position,
            annotations: Default::default(),
        }
    }

    fn canonical_evidence(
        source_id: &SourceId,
        id: &str,
        text: &str,
        position: u32,
    ) -> EvidenceUnit {
        EvidenceUnit {
            locator: SourceLocator::Canonical {
                locator: CanonicalLocator::single_unit(
                    "bible",
                    "test",
                    vec![
                        ReferenceComponent {
                            level: "book".into(),
                            value: "John".into(),
                            ordinal: Some(43),
                        },
                        ReferenceComponent {
                            level: "chapter".into(),
                            value: "3".into(),
                            ordinal: Some(3),
                        },
                        ReferenceComponent {
                            level: "verse".into(),
                            value: position.to_string(),
                            ordinal: Some(position),
                        },
                    ],
                    format!("John 3:{position}"),
                    format!("john:3:{position}"),
                ),
            },
            annotations: Default::default(),
            ..text_evidence(source_id, id, text, position)
        }
    }

    #[test]
    fn mixed_locator_partitions_stay_separate_in_both_start_orders() {
        let source_id = SourceId("mixed-locator-evidence".into());
        let evidence = vec![
            canonical_evidence(&source_id, "verse-1", "Canonical verse one", 1),
            text_evidence(
                &source_id,
                "ocr-2",
                "OCR_ONLY_TEXT must stay outside scripture chunks",
                2,
            ),
            canonical_evidence(&source_id, "verse-3", "Canonical verse three", 3),
        ];
        let partitioned = chunk_searchable_evidence_by_locator(
            &source_id,
            &evidence,
            &ChunkerConfig::default(),
            &CanonicalChunkerConfig::default(),
        )
        .unwrap();

        assert_eq!(partitioned.canonical_evidence_count, 2);
        assert_eq!(partitioned.noncanonical_evidence_count, 1);
        assert_eq!(
            partitioned.strategies_used,
            ["chunk_canonical_units", "chunk_evidence"]
        );
        assert!(partitioned.output.chunks.iter().all(|chunk| {
            let has_canonical = chunk
                .evidence_unit_ids
                .iter()
                .any(|id| id.0.starts_with("verse-"));
            let has_noncanonical = chunk.evidence_unit_ids.iter().any(|id| id.0 == "ocr-2");
            !(has_canonical && has_noncanonical)
                && (!has_canonical || !chunk.text.contains("OCR_ONLY_TEXT"))
        }));
        assert_eq!(
            partitioned
                .output
                .chunks
                .iter()
                .map(|chunk| &chunk.id)
                .collect::<HashSet<_>>()
                .len(),
            partitioned.output.chunks.len()
        );

        let noncanonical_first = chunk_searchable_evidence_by_locator(
            &source_id,
            &[
                text_evidence(&source_id, "text-1", "Leading plain text", 1),
                canonical_evidence(&source_id, "verse-2", "Middle verse", 2),
                text_evidence(&source_id, "text-3", "Trailing plain text", 3),
            ],
            &ChunkerConfig::default(),
            &CanonicalChunkerConfig::default(),
        )
        .unwrap();
        assert!(noncanonical_first.output.chunks[0]
            .evidence_unit_ids
            .iter()
            .all(|id| id.0.starts_with("text-")));
        assert!(noncanonical_first
            .output
            .chunks
            .last()
            .unwrap()
            .evidence_unit_ids
            .iter()
            .all(|id| id.0 == "verse-2"));
    }

    #[test]
    fn mixed_locator_merge_orders_chunks_links_and_spans_by_source_evidence() {
        let source_id = SourceId("ordered-locator-evidence".into());
        let mut evidence = (1..=20)
            .map(|position| {
                canonical_evidence(
                    &source_id,
                    &format!("verse-{position}"),
                    &format!("Verse {position}"),
                    position,
                )
            })
            .collect::<Vec<_>>();
        evidence.push(text_evidence(
            &source_id,
            "text-21",
            "Middle plain text",
            21,
        ));
        evidence.push(canonical_evidence(&source_id, "verse-22", "Verse 22", 22));
        let ordered = chunk_searchable_evidence_by_locator(
            &source_id,
            &evidence,
            &ChunkerConfig::default(),
            &CanonicalChunkerConfig::default(),
        )
        .unwrap();

        let middle_chunk = ordered
            .output
            .chunks
            .iter()
            .position(|chunk| chunk.evidence_unit_ids.iter().any(|id| id.0 == "text-21"))
            .unwrap();
        let late_canonical_chunk = ordered
            .output
            .chunks
            .iter()
            .position(|chunk| {
                chunk.chunk_type == ChunkType::Child
                    && chunk.evidence_unit_ids.iter().any(|id| id.0 == "verse-22")
            })
            .unwrap();
        assert!(middle_chunk < late_canonical_chunk);
        let middle_link = ordered
            .output
            .links
            .iter()
            .position(|(_, evidence_id)| evidence_id.0 == "text-21")
            .unwrap();
        let late_canonical_link = ordered
            .output
            .links
            .iter()
            .position(|(_, evidence_id)| evidence_id.0 == "verse-22")
            .unwrap();
        assert!(middle_link < late_canonical_link);
        let middle_span = ordered
            .output
            .evidence_spans
            .iter()
            .position(|span| span.evidence_id.0 == "text-21")
            .unwrap();
        let late_canonical_span = ordered
            .output
            .evidence_spans
            .iter()
            .position(|span| span.evidence_id.0 == "verse-22")
            .unwrap();
        assert!(middle_span < late_canonical_span);
    }

    #[test]
    fn canonical_partition_uses_provided_chunker_config() {
        let source_id = SourceId("configured-canonical-partition".into());
        let evidence = (1..=4)
            .map(|position| {
                canonical_evidence(
                    &source_id,
                    &format!("verse-{position}"),
                    &format!("Verse {position}"),
                    position,
                )
            })
            .collect::<Vec<_>>();
        let canonical_config = CanonicalChunkerConfig {
            target_tokens: usize::MAX,
            overlap_units: 0,
            max_units_per_child: 1,
        };

        let partitioned = chunk_searchable_evidence_by_locator(
            &source_id,
            &evidence,
            &ChunkerConfig::default(),
            &canonical_config,
        )
        .unwrap();
        let child_sizes = partitioned
            .output
            .chunks
            .iter()
            .filter(|chunk| chunk.chunk_type == ChunkType::Child)
            .map(|chunk| chunk.evidence_unit_ids.len())
            .collect::<Vec<_>>();

        assert_eq!(child_sizes, [1, 1, 1, 1]);
    }

    #[test]
    fn pure_and_empty_locator_partitions_use_only_the_needed_strategy() {
        let source_id = SourceId("single-locator-partition".into());
        let canonical = vec![canonical_evidence(
            &source_id,
            "verse-1",
            "Canonical verse",
            1,
        )];
        let noncanonical = vec![text_evidence(
            &source_id,
            "text-1",
            "Plain noncanonical text",
            1,
        )];

        for (evidence, expected_counts, expected_strategies) in [
            (canonical.as_slice(), (1, 0), vec!["chunk_canonical_units"]),
            (noncanonical.as_slice(), (0, 1), vec!["chunk_evidence"]),
            (&[][..], (0, 0), Vec::new()),
        ] {
            let partitioned = chunk_searchable_evidence_by_locator(
                &source_id,
                evidence,
                &ChunkerConfig::default(),
                &CanonicalChunkerConfig::default(),
            )
            .unwrap();
            assert_eq!(
                (
                    partitioned.canonical_evidence_count,
                    partitioned.noncanonical_evidence_count,
                ),
                expected_counts
            );
            assert_eq!(partitioned.strategies_used, expected_strategies);
            assert_eq!(partitioned.output.chunks.is_empty(), evidence.is_empty());
        }
    }
}
