use super::*;
use crate::types::{EvidenceKind, MarkdownBlockKind, MarkdownHeadingLocator, SourceLocator};
use proptest::prelude::*;
use std::collections::HashMap;

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
fn conservative_token_estimator_covers_cjk_and_is_deterministic() {
    let cjk = "中文测试";
    let previous_bytes_per_four = cjk.len() / 4;
    let letters = "abcdefghijkl";
    let punctuation = ":/?#[]{}!@&=";
    let code = "fn main(){x+=1;}";
    let url = "https://x.test/a?b=c";

    assert_eq!(estimate_tokens(cjk), 4);
    assert!(estimate_tokens(cjk) as usize > previous_bytes_per_four);
    assert_eq!(estimate_tokens("a"), 1);
    assert_eq!(estimate_tokens(" "), 1);
    assert!(estimate_tokens(punctuation) > estimate_tokens(letters));
    assert!(estimate_tokens(code) > estimate_tokens(&"a".repeat(code.len())));
    assert!(estimate_tokens(url) > estimate_tokens(&"a".repeat(url.len())));
    assert_eq!(estimate_tokens(url), estimate_tokens(url));
}

#[test]
fn conservative_token_estimator_covers_long_hex_runs() {
    let hex = "0123456789abcdef";

    assert!(estimate_tokens(&hex.repeat(4)) >= 45);
    assert!(estimate_tokens(&hex.repeat(24)) >= 265);
}

#[test]
fn conservative_token_estimator_punctuation_dense_code_exceeds_default_target() {
    let punctuation = ":/?#[]{}!@&=".repeat(120);
    let letters = "abc ".repeat(punctuation.len() / 4);
    let punctuation_tokens = estimate_tokens(&punctuation);

    assert!(punctuation_tokens >= punctuation.len() as u32 / 2);
    assert!(punctuation_tokens >= 2 * estimate_tokens(&letters));
    assert!(punctuation_tokens > ChunkerConfig::default().child_target_tokens as u32);
}

#[test]
fn conservative_token_estimator_spaced_composition_matches_joined_text() {
    let parts = ["a", "b", "c", "d"];

    assert_eq!(
        estimate_spaced_tokens(parts),
        estimate_tokens(&parts.join(" ")) as usize
    );
}

#[test]
fn conservative_token_estimator_punctuation_overlap_stays_within_budget() {
    let text = ":/?#[]{}!@&=";
    let budget_tokens = 4;
    let overlap_start = overlap_start_for_token_budget(text, budget_tokens);

    assert!(estimate_tokens(&text[overlap_start..]) as usize <= budget_tokens);

    let long_run = "0123456789abcdef".repeat(4);
    let overlap_start = overlap_start_for_token_budget(&long_run, 45);
    assert!(estimate_tokens(&long_run[overlap_start..]) <= 45);
}

#[test]
fn cjk_units_exceed_child_target_before_equivalent_ascii_units() {
    let mut cjk = make_evidence(2, "中文章节");
    cjk[0].text = "中文测试".into();
    cjk[1].text = "保守估算".into();
    let config = ChunkerConfig {
        child_target_tokens: 6,
        child_overlap_tokens: 0,
        parent_children_count: 2,
    };

    let cjk_output = chunk_evidence(&SourceId("cjk".into()), &cjk, &config);
    let cjk_children = cjk_output
        .chunks
        .iter()
        .filter(|chunk| chunk.chunk_type == ChunkType::Child)
        .collect::<Vec<_>>();

    assert_eq!(cjk_children.len(), 2);
    assert!(cjk_children.iter().all(|chunk| chunk.token_count == 4));

    let mut ascii = make_evidence(2, "English");
    ascii[0].text = "abcdefghijkl".into();
    ascii[1].text = "mnopqrstuvwx".into();
    let ascii_output = chunk_evidence(&SourceId("ascii".into()), &ascii, &config);
    assert_eq!(
        ascii_output
            .chunks
            .iter()
            .filter(|chunk| chunk.chunk_type == ChunkType::Child)
            .count(),
        1,
        "the existing English-sized fixture should keep its child structure"
    );
}

#[test]
fn utf8_overlap_retains_resolvable_evidence_spans() {
    let mut evidence = make_evidence(2, "中文章节");
    evidence[0].text = format!("{}[链接](https://example.test/路径)", "前".repeat(160));
    evidence[1].text = "后".repeat(100);
    let config = ChunkerConfig {
        child_target_tokens: 100,
        child_overlap_tokens: 50,
        parent_children_count: 2,
    };

    let output = chunk_evidence(&SourceId("test".into()), &evidence, &config);
    let children = output
        .chunks
        .iter()
        .filter(|chunk| chunk.chunk_type == ChunkType::Child)
        .collect::<Vec<_>>();

    assert!(children.len() >= 2, "fixture must produce an overlap child");
    assert!(children[1].text.contains("链接"));
    assert!(output.evidence_spans.iter().any(|span| {
        span.chunk_id == children[1].id
            && span.evidence_id == evidence[0].id
            && children[1].text[span.chunk_byte_start as usize..span.chunk_byte_end as usize]
                .contains("链接")
    }));
    assert_source_content_resolves_to_evidence_spans(&output, &evidence);
}

#[test]
fn every_source_derived_non_whitespace_character_has_a_resolvable_span() {
    let fixtures = [
        (
            format!("{} [链接](https://example.test/路径)", "甲".repeat(160)),
            "乙".repeat(120),
        ),
        (
            "fn main() { println!(\"你好\"); } ".repeat(20),
            "let value = \"code overlap\"; ".repeat(20),
        ),
    ];

    for (first, second) in fixtures {
        let mut evidence = make_evidence(2, "Section");
        evidence[0].text = first;
        evidence[1].text = second;
        let output = chunk_evidence(
            &SourceId("test".into()),
            &evidence,
            &ChunkerConfig {
                child_target_tokens: 100,
                child_overlap_tokens: 50,
                parent_children_count: 2,
            },
        );

        assert_source_content_resolves_to_evidence_spans(&output, &evidence);
    }
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
    fn generic_provenance_property_preserves_utf8_repeated_text_across_boundaries(
        repeated in repeated_unicode_text(),
        leading in unicode_whitespace(),
        trailing in unicode_whitespace(),
        overlap_tokens in 0usize..4,
        parent_children_count in 1usize..4,
    ) {
        let mut evidence = make_evidence(2, "First");
        let mut second_group = make_evidence(2, "Second");
        for (index, unit) in evidence.iter_mut().chain(second_group.iter_mut()).enumerate() {
            unit.id = EvidenceId(format!("repeated-{index}"));
            unit.text = format!("{leading}{repeated}{trailing}");
            unit.text_hash = crate::types::hex_sha256(unit.text.as_bytes());
            unit.locator = SourceLocator::Pdf {
                page: index as u32 + 1,
                paragraph: index as u32,
                bbox: None,
            };
            unit.position = index as u32;
        }
        evidence.extend(second_group);

        let output = chunk_evidence(
            &SourceId("test".into()),
            &evidence,
            &ChunkerConfig {
                child_target_tokens: 1,
                child_overlap_tokens: overlap_tokens,
                parent_children_count,
            },
        );

        prop_assert!(output.chunks.iter().any(|chunk| chunk.chunk_type == ChunkType::Parent));
        assert_source_content_resolves_to_evidence_spans(&output, &evidence);
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
fn parents_do_not_cross_hard_boundary_groups() {
    let mut evidence = make_evidence(3, "First");
    let mut second_section = make_evidence(3, "Second");
    for (offset, unit) in second_section.iter_mut().enumerate() {
        unit.id = EvidenceId(format!("second-ev-{offset}"));
        unit.position = (offset + 3) as u32;
    }
    evidence.extend(second_section);
    let config = ChunkerConfig {
        child_target_tokens: 20,
        child_overlap_tokens: 0,
        parent_children_count: 2,
    };

    let output = chunk_evidence(&SourceId("test".into()), &evidence, &config);
    let parents = output
        .chunks
        .iter()
        .filter(|chunk| chunk.chunk_type == ChunkType::Parent)
        .collect::<Vec<_>>();

    for parent in parents {
        let headings = output
            .chunks
            .iter()
            .filter(|chunk| chunk.parent_chunk_id.as_ref() == Some(&parent.id))
            .map(|chunk| chunk.heading_path.first().expect("child heading"))
            .collect::<Vec<_>>();
        assert!(
            headings.iter().all(|heading| **heading == *headings[0]),
            "parent {} crossed hard boundaries: {headings:?}",
            parent.id.0
        );
    }
}

#[test]
fn hard_boundary_keys_handle_repeated_empty_and_nested_markdown_headings() {
    let source_id = SourceId("doc".into());
    let repeated_first = markdown_evidence(
        &source_id,
        "repeat",
        "first repeated heading section",
        "repeat-first",
        3,
        0,
    );
    let repeated_second = markdown_evidence(
        &source_id,
        "repeat",
        "second repeated heading section",
        "repeat-second",
        30,
        1,
    );
    assert_ne!(
        hard_boundary_key(&repeated_first),
        hard_boundary_key(&repeated_second),
        "a repeated top-level heading is a new hard-boundary group"
    );

    let output = chunk_evidence(
        &source_id,
        &[repeated_first.clone(), repeated_second.clone()],
        &ChunkerConfig {
            child_target_tokens: 1,
            child_overlap_tokens: 0,
            parent_children_count: 2,
        },
    );
    let parents = output
        .chunks
        .iter()
        .filter(|chunk| chunk.chunk_type == ChunkType::Parent)
        .collect::<Vec<_>>();
    assert_eq!(parents.len(), 2);
    assert!(parents
        .iter()
        .all(|parent| parent.evidence_unit_ids.len() == 1));

    let mut nested = repeated_first.clone();
    nested.heading_path.push("Nested subsection".into());
    let SourceLocator::Markdown { heading_path, .. } = &mut nested.locator else {
        unreachable!("fixture is markdown")
    };
    heading_path.push(MarkdownHeadingLocator {
        level: 2,
        text: "Nested subsection".into(),
        slug: "nested-subsection".into(),
        line: 4,
    });
    assert_eq!(
        hard_boundary_key(&repeated_first),
        hard_boundary_key(&nested),
        "a nested heading remains in its top-level hard-boundary group"
    );

    let mut malformed = repeated_first;
    malformed.heading_path = vec![" ".into()];
    let SourceLocator::Markdown {
        heading_path,
        heading_slug,
        ..
    } = &mut malformed.locator
    else {
        unreachable!("fixture is markdown")
    };
    *heading_slug = Some(" ".into());
    *heading_path = vec![MarkdownHeadingLocator {
        level: 0,
        text: String::new(),
        slug: " ".into(),
        line: 0,
    }];
    assert!(hard_boundary_key(&malformed).ends_with(":preamble"));
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
            chunk.chunk_type == ChunkType::Child && chunk.heading_path == vec![heading.to_string()]
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
