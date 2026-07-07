use verbatim_core::parser::canonical_jsonl::CanonicalJsonlParser;
use verbatim_core::traits::Parser;
use verbatim_core::types::{EvidenceKind, SourceLocator};

#[test]
fn parse_csb_bible_jsonl_end_to_end() {
    let path = std::path::Path::new("/tmp/csb_bible.jsonl");
    if !path.exists() {
        eprintln!("skipping: /tmp/csb_bible.jsonl not found");
        return;
    }

    let parser = CanonicalJsonlParser;
    let units = parser.parse(path).unwrap();

    // Should have ~31K verses
    assert!(
        units.len() > 30000,
        "expected 30K+ units, got {}",
        units.len()
    );

    // First verse should be Genesis 1:1
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

    // Find John 3:16
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

    // Count unique books
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
    assert_eq!(books.len(), 66, "expected 66 books, got {}", books.len());
}
