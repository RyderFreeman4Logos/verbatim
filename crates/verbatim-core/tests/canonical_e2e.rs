use verbatim_core::canonical_chunker::{chunk_canonical_units, CanonicalChunkerConfig};
use verbatim_core::parser::canonical_jsonl::CanonicalJsonlParser;
use verbatim_core::traits::Parser;
use verbatim_core::types::{EvidenceKind, SourceLocator};

#[test]
fn parse_repository_canonical_bible_fixture_end_to_end() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/canonical_bible.jsonl");
    assert!(
        path.is_file(),
        "required canonical Bible fixture is missing or unreadable: {}",
        path.display()
    );

    let parser = CanonicalJsonlParser;
    let units = parser.parse(&path).unwrap();

    assert_eq!(units.len(), 7, "fixture records must all be parsed");

    // First verse should be Genesis 1:1.
    let first = &units[0];
    assert_eq!(first.kind, EvidenceKind::Text);
    match &first.locator {
        SourceLocator::Canonical { locator } => {
            assert_eq!(locator.display, "Genesis 1:1");
            assert!(!first.text.is_empty());
            assert_eq!(locator.start[0].value, "Genesis");
            assert_eq!(locator.start[0].ordinal, Some(1));
        }
        _ => panic!("expected Canonical locator for first unit"),
    }

    // Find John 3:16.
    let jn316 = units.iter().find(|u| match &u.locator {
        SourceLocator::Canonical { locator } => locator.display == "John 3:16",
        _ => false,
    });
    assert!(jn316.is_some(), "John 3:16 should be present");
    let unit = jn316.unwrap();
    assert!(
        unit.text.to_lowercase().contains("loved") || unit.text.to_lowercase().contains("world"),
        "John 3:16 text should mention love/world: {}",
        unit.text
    );

    let range = units
        .iter()
        .find(|unit| {
            matches!(
                &unit.locator,
                SourceLocator::Canonical { locator } if locator.display == "2 Timothy 4:7-8"
            )
        })
        .expect("the fixture must include a canonical range");
    assert!(range.text.contains("crown of righteousness"));

    assert!(units.iter().any(|unit| unit.text.contains("ἰδοὺ")));

    let reparsed = parser.parse(&path).unwrap();
    assert_eq!(
        units.iter().map(|unit| &unit.id).collect::<Vec<_>>(),
        reparsed.iter().map(|unit| &unit.id).collect::<Vec<_>>(),
        "canonical evidence IDs must be stable"
    );

    let chunks = chunk_canonical_units(
        &units[0].source_id,
        &units,
        &CanonicalChunkerConfig::default(),
    )
    .unwrap();
    assert!(
        chunks.chunks.len() > units.len(),
        "expected parent and child chunks"
    );
    assert!(chunks
        .chunks
        .iter()
        .any(|chunk| chunk.text.contains("crown of righteousness")));

    // The fixture crosses multiple books.
    let mut books = std::collections::HashSet::new();
    for u in &units {
        if let SourceLocator::Canonical { locator } = &u.locator {
            if let Some(book) = locator.start.first() {
                if book.level == "book" {
                    books.insert(book.value.clone());
                }
            }
        }
    }
    assert!(
        books.len() >= 3,
        "expected multiple books, got {}",
        books.len()
    );
}
