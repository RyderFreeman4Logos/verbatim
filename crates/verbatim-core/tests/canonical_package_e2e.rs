use std::collections::BTreeMap;
use std::fs;
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

fn write_package(path: &Path, schema_version: &str, units: &str) {
    fs::create_dir(path).unwrap();
    fs::write(
        path.join("manifest.json"),
        format!(
            r#"{{"schema_version":"{schema_version}","profile":"bible","content_kind":"text","work_id":"KJV","version_id":"public-domain","language":"en"}}"#
        ),
    )
    .unwrap();
    fs::write(path.join("units.jsonl"), units).unwrap();
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
fn canonical_package_preserves_source_native_selectors() {
    let package = fixture("valid");
    let original = CanonicalPackageParser.parse(&package).unwrap();
    let tempdir = tempfile::tempdir().unwrap();
    let reordered = tempdir.path().join("reordered");
    fs::create_dir(&reordered).unwrap();
    fs::copy(
        package.join("manifest.json"),
        reordered.join("manifest.json"),
    )
    .unwrap();
    let units = fs::read_to_string(package.join("units.jsonl")).unwrap();
    let mut lines = units.lines().collect::<Vec<_>>();
    lines.reverse();
    fs::write(
        reordered.join("units.jsonl"),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();
    let reordered_units = CanonicalPackageParser.parse(&reordered).unwrap();

    let records = |units: &[verbatim_core::types::EvidenceUnit]| {
        units
            .iter()
            .map(|unit| match &unit.locator {
                verbatim_core::types::SourceLocator::Canonical { locator } => (
                    locator.normalized.clone(),
                    (locator.backing_selectors.clone(), unit.text_hash.clone()),
                ),
                _ => panic!("expected canonical locator"),
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(records(&original), records(&reordered_units));
    assert_eq!(
        records(&original)["john:3:16"].0,
        vec![verbatim_core::types::BackingSelector::SourceNative {
            scheme: "usfm".into(),
            value: "JHN 3:16".into(),
        }]
    );

    let report = serde_json::to_value(validate_package(&package)).unwrap();
    assert_eq!(
        report["original_source_hash"],
        "0bc1dd60d3bb6082799548fd022a62108d510b59523e04de6071e396b79d018c"
    );
    assert_eq!(report["conversion"]["converter"], "fixture-converter");
    assert_eq!(
        report["units"][0]["locator"]["backing_selectors"][0]["type"],
        "SourceNative"
    );
}

#[test]
fn canonical_package_verify_rejects_broken_source_native_selector() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("broken-selector");
    fs::create_dir(&package).unwrap();
    fs::copy(
        fixture("valid").join("manifest.json"),
        package.join("manifest.json"),
    )
    .unwrap();
    let units = fs::read_to_string(fixture("valid").join("units.jsonl"))
        .unwrap()
        .replace("JHN 3:16", "JHN 3:99");
    fs::write(package.join("units.jsonl"), units).unwrap();

    let report = validate_package(&package);
    assert!(!report.valid);
    assert!(report
        .diagnostics
        .iter()
        .any(|item| item.code == "CANONICAL_PACKAGE_SOURCE_NATIVE_SELECTOR_INVALID"));
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();
    assert!(pipeline.add_source(&package).is_err());
    assert!(pipeline.store().list_sources().unwrap().is_empty());
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

#[test]
fn canonical_package_rejects_empty_component_objects_before_persist() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("invalid-components");
    write_package(
        &package,
        "1.0.0",
        r#"{"source_profile":"bible","work_id":"KJV","version_id":"public-domain","language":"en","components":[{}],"text":"text","backing_selectors":[{"type":"LineRange","start":1,"end":1}]}"#,
    );
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let result = pipeline.add_source(&package);

    let error = result.unwrap_err().to_string();
    assert!(error.contains("CANONICAL_PACKAGE_UNIT_INVALID"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
}

#[test]
fn canonical_package_rejects_non_semver_schema_version() {
    for schema_version in ["1", "1.x"] {
        let tempdir = tempfile::tempdir().unwrap();
        let package = tempdir.path().join("invalid-schema-version");
        write_package(
            &package,
            schema_version,
            r#"{"source_profile":"bible","work_id":"KJV","version_id":"public-domain","language":"en","components":[{"level":"book","value":"John"}],"text":"text","backing_selectors":[{"type":"LineRange","start":1,"end":1}]}"#,
        );
        let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

        let report = validate_package(&package);
        assert!(!report.valid, "{schema_version}");
        assert_eq!(
            report.diagnostics[0].code,
            "CANONICAL_PACKAGE_SCHEMA_VERSION_INVALID"
        );
        let error = pipeline.add_source(&package).unwrap_err().to_string();
        assert!(error.contains("CANONICAL_PACKAGE_SCHEMA_VERSION_INVALID"));
        assert!(pipeline.store().list_sources().unwrap().is_empty());
    }
}
