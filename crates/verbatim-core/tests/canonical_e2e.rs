use verbatim_core::canonical_chunker::{chunk_canonical_units, CanonicalChunkerConfig};
use verbatim_core::parser::canonical_jsonl::CanonicalJsonlParser;
use verbatim_core::store::Store;
use verbatim_core::traits::Parser;
use verbatim_core::types::{EvidenceKind, Source, SourceLocator, SourceStatus};

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

    // Canonical JSONL language survives parse: provided stays, absent stays absent.
    let revelation = units
        .iter()
        .find(|unit| matches!(&unit.locator, SourceLocator::Canonical { locator } if locator.display == "Revelation 7:9"))
        .expect("fixture must include Revelation 7:9");
    assert_eq!(revelation.language.as_deref(), Some("el"));
    let genesis = units
        .iter()
        .find(|unit| matches!(&unit.locator, SourceLocator::Canonical { locator } if locator.display == "Genesis 1:1"))
        .expect("fixture must include Genesis 1:1");
    assert_eq!(genesis.language, None, "absent language must stay absent");

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

#[test]
fn canonical_fixture_language_survives_store_round_trip() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/canonical_bible.jsonl");
    let units = CanonicalJsonlParser.parse(&path).unwrap();
    let revelation = units
        .iter()
        .find(|unit| matches!(&unit.locator, SourceLocator::Canonical { locator } if locator.display == "Revelation 7:9"))
        .unwrap();
    let genesis = units
        .iter()
        .find(|unit| matches!(&unit.locator, SourceLocator::Canonical { locator } if locator.display == "Genesis 1:1"))
        .unwrap();

    let dir = std::env::temp_dir().join(format!(
        "verbatim-canonical-language-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::new(&dir.join("store.db")).unwrap();
    let source = Source {
        id: units[0].source_id.clone(),
        path: path.clone(),
        hash: "fixture-hash".into(),
        status: SourceStatus::Indexed,
        parser_used: Some("canonical_jsonl".into()),
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    store.bulk_insert_evidence(&units).unwrap();

    let reloaded = store.get_evidence(&revelation.id).unwrap().unwrap();
    assert_eq!(reloaded.language.as_deref(), Some("el"));

    let genesis_reloaded = store.get_evidence(&genesis.id).unwrap().unwrap();
    assert_eq!(genesis_reloaded.language, None);

    let all = store.list_evidence_by_source(&source.id).unwrap();
    assert_eq!(all.len(), 7);
    assert!(all.iter().any(|u| u.language.as_deref() == Some("el")));

    // Reopen the database to prove language persisted on disk.
    let reopened = Store::new(&dir.join("store.db")).unwrap();
    let persisted = reopened.get_evidence(&revelation.id).unwrap().unwrap();
    assert_eq!(persisted.language.as_deref(), Some("el"));

    let _ = std::fs::remove_dir_all(&dir);
}
