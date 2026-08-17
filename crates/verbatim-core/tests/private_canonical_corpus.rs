use verbatim_core::parser::canonical_jsonl::CanonicalJsonlParser;
use verbatim_core::traits::Parser;
use verbatim_core::types::SourceLocator;

#[test]
#[ignore = "run with `just test-private-canonical-corpus`"]
fn private_full_corpus_has_expected_count_and_locators() {
    let path = std::env::var_os("VERBATIM_PRIVATE_CANONICAL_CORPUS")
        .map(std::path::PathBuf::from)
        .expect("private corpus recipe must provide VERBATIM_PRIVATE_CANONICAL_CORPUS");
    assert!(
        path.is_file(),
        "private canonical corpus is missing or unreadable: {}",
        path.display()
    );

    let units = CanonicalJsonlParser.parse(&path).unwrap();
    assert_eq!(units.len(), 31_102, "private corpus verse count changed");
    for expected in ["Genesis 1:1", "John 3:16", "Revelation 22:21"] {
        assert!(
            units.iter().any(|unit| {
                matches!(
                    &unit.locator,
                    SourceLocator::Canonical { locator } if locator.display == expected
                )
            }),
            "private corpus is missing {expected}"
        );
    }
}
