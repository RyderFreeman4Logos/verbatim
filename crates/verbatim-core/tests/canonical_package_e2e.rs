use std::path::{Path, PathBuf};

use verbatim_core::config::Config;
use verbatim_core::ingest::IngestPipeline;
use verbatim_core::parser::canonical_package::{validate_package, CanonicalPackageParser};
use verbatim_core::traits::Parser;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/canonical_package")
        .join(name)
}

#[test]
fn canonical_package_validates_golden_package() {
    let report = validate_package(&fixture("valid"));

    assert!(report.valid, "diagnostics: {:?}", report.diagnostics);
    assert_eq!(report.schema_version.as_deref(), Some("1.0.0"));
    assert_eq!(report.unit_count, 2);
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.report_hash.len(), 64);
}

#[test]
fn canonical_package_rejects_future_major() {
    let report = validate_package(&fixture("future-major"));

    assert!(!report.valid);
    assert_eq!(
        report.diagnostics[0].code,
        "CANONICAL_PACKAGE_UNSUPPORTED_SCHEMA_MAJOR"
    );
    assert_eq!(report.diagnostics[0].location, "manifest.json");
}

#[tokio::test]
async fn canonical_package_ingest_preserves_unit_identity() {
    let package = fixture("valid");
    let expected_ids = CanonicalPackageParser
        .parse(&package)
        .unwrap()
        .into_iter()
        .map(|unit| unit.id)
        .collect::<Vec<_>>();
    let tempdir = tempfile::tempdir().unwrap();
    let mut pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let source_id = pipeline.add_source(&package).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let ingested_ids = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()
        .into_iter()
        .map(|unit| unit.id)
        .collect::<Vec<_>>();

    assert_eq!(ingested_ids, expected_ids);
    assert!(ingested_ids.iter().all(|id| id.0.starts_with("cjson:v1:")));
}

#[test]
fn canonical_package_invalid_fails_before_persistent_mutation() {
    let tempdir = tempfile::tempdir().unwrap();
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let error = pipeline
        .add_source(&fixture("invalid-text-hash"))
        .unwrap_err()
        .to_string();

    assert!(error.contains("CANONICAL_PACKAGE_TEXT_HASH_MISMATCH"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
}
