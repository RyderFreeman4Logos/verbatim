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

fn relation_package(tempdir: &tempfile::TempDir, relation: &str) -> PathBuf {
    let package = tempdir.path().join("relation");
    fs::create_dir(&package).unwrap();
    for file in ["manifest.json", "units.jsonl"] {
        fs::copy(fixture("verse-footnote").join(file), package.join(file)).unwrap();
    }
    fs::write(package.join("relations.jsonl"), relation).unwrap();
    package
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
fn canonical_package_parse_exposes_conversion_envelope() {
    let (units, conversion) = CanonicalPackageParser
        .parse_with_derived_metadata(&fixture("valid"))
        .unwrap();
    let report = validate_package(&fixture("valid"));

    assert_eq!(units.len(), 2);
    assert_eq!(conversion, report.conversion);
    let conversion = conversion.expect("fixture conversion envelope");
    assert_eq!(conversion.adapter, "fixture-adapter");
    assert_eq!(
        conversion.original_source_hash,
        "0bc1dd60d3bb6082799548fd022a62108d510b59523e04de6071e396b79d018c"
    );
    assert_eq!(
        conversion.output_hash,
        "10e35c9b02b6297550a7c4009b2a9620bc9360331b90895d40f0a1bd5963dcdd"
    );
}

#[test]
fn canonical_package_rejects_empty_conversion_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("empty-conversion");
    fs::create_dir(&package).unwrap();
    let manifest = fs::read_to_string(fixture("valid").join("manifest.json"))
        .unwrap()
        .replace(
            "10e35c9b02b6297550a7c4009b2a9620bc9360331b90895d40f0a1bd5963dcdd",
            "",
        );
    fs::write(package.join("manifest.json"), manifest).unwrap();
    fs::copy(
        fixture("valid").join("units.jsonl"),
        package.join("units.jsonl"),
    )
    .unwrap();

    let report = validate_package(&package);

    assert!(!report.valid);
    assert_eq!(
        report.diagnostics[0].code,
        "CANONICAL_PACKAGE_CONVERSION_INVALID"
    );
    assert_eq!(
        report.diagnostics[0].message,
        "conversion output_hash is required"
    );
    assert!(CanonicalPackageParser.parse(&package).is_err());
}

#[test]
fn canonical_package_persists_package_unit_id() {
    let units = CanonicalPackageParser.parse(&fixture("valid")).unwrap();

    assert_eq!(
        units.into_iter().map(|unit| unit.id.0).collect::<Vec<_>>(),
        ["pkg:john-3-16", "pkg:john-4-1"]
    );
}

#[test]
fn canonical_package_rejects_unknown_versification() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("unknown-versification");
    fs::create_dir(&package).unwrap();
    let manifest = fs::read_to_string(fixture("valid").join("manifest.json"))
        .unwrap()
        .replace(
            "\"version_id\": \"public-domain\"",
            "\"version_id\": \"public-domain\",\n  \"canon_id\": \"protestant-66/v1\",\n  \"versification_id\": \"unknown\"",
        );
    fs::write(package.join("manifest.json"), manifest).unwrap();
    fs::copy(
        fixture("valid").join("units.jsonl"),
        package.join("units.jsonl"),
    )
    .unwrap();
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let error = pipeline.add_source(&package).unwrap_err().to_string();

    assert!(error.contains("CANONICAL_PACKAGE_VERSIFICATION_UNKNOWN"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
}

#[test]
fn canonical_package_persists_canon_and_versification_ids() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("versioned-locator");
    fs::create_dir(&package).unwrap();
    let manifest = fs::read_to_string(fixture("valid").join("manifest.json"))
        .unwrap()
        .replace(
            "\"version_id\": \"public-domain\"",
            "\"version_id\": \"public-domain\",\n  \"canon_id\": \"protestant-66/v1\",\n  \"versification_id\": \"protestant-66/v1\"",
        );
    fs::write(package.join("manifest.json"), manifest).unwrap();
    let units = fs::read_to_string(fixture("valid").join("units.jsonl"))
        .unwrap()
        .replace(
            "\"version_id\":\"public-domain\"",
            "\"version_id\":\"public-domain\",\"canon_id\":\"protestant-66/v1\",\"versification_id\":\"protestant-66/v1\"",
        );
    fs::write(package.join("units.jsonl"), units).unwrap();

    let units = CanonicalPackageParser.parse(&package).unwrap();
    let locator = match &units[0].locator {
        verbatim_core::types::SourceLocator::Canonical { locator } => locator,
        _ => panic!("expected canonical locator"),
    };

    assert_eq!(locator.canon_id.as_deref(), Some("protestant-66/v1"));
    assert_eq!(
        locator.versification_id.as_deref(),
        Some("protestant-66/v1")
    );
}

#[test]
fn canonical_package_rejects_out_of_bound_verse() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("out-of-bound-verse");
    fs::create_dir(&package).unwrap();
    fs::copy(
        fixture("valid").join("manifest.json"),
        package.join("manifest.json"),
    )
    .unwrap();
    let units = fs::read_to_string(fixture("valid").join("units.jsonl"))
        .unwrap()
        .replace("John 3:16", "John 3:99")
        .replace("JHN 3:16", "JHN 3:99")
        .replace("\"value\":\"16\"", "\"value\":\"99\"");
    fs::write(package.join("units.jsonl"), units).unwrap();
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let error = pipeline.add_source(&package).unwrap_err().to_string();

    assert!(error.contains("CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
}

#[test]
fn canonical_package_rejects_malformed_verse() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("malformed-verse");
    fs::create_dir(&package).unwrap();
    fs::copy(
        fixture("valid").join("manifest.json"),
        package.join("manifest.json"),
    )
    .unwrap();
    let units = fs::read_to_string(fixture("valid").join("units.jsonl"))
        .unwrap()
        .replace("John 3:16", "John 3:999999")
        .replace("JHN 3:16", "JHN 3:999999")
        .replace("\"value\":\"16\"", "\"value\":\"999999\"");
    fs::write(package.join("units.jsonl"), units).unwrap();
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let error = pipeline.add_source(&package).unwrap_err().to_string();

    assert!(error.contains("CANONICAL_PACKAGE_REFERENCE_OUT_OF_BOUNDS"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
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
    assert_eq!(
        ingested_ids.into_iter().map(|id| id.0).collect::<Vec<_>>(),
        ["pkg:john-3-16", "pkg:john-4-1"]
    );
}

#[tokio::test]
async fn canonical_package_ingests_verse_and_footnote_as_distinct_kinds() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();
    let source_id = pipeline.add_source(&fixture("verse-footnote")).unwrap();

    pipeline.ingest_source(&source_id).await.unwrap();
    let evidence = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()
        .into_iter()
        .map(|unit| (unit.id.0.clone(), unit))
        .collect::<BTreeMap<_, _>>();

    let verse = &evidence["pkg:john-3-16"];
    let footnote = &evidence["pkg:john-3-16-note-1"];
    assert_eq!(serde_json::to_value(verse.kind).unwrap(), "Verse");
    assert_eq!(serde_json::to_value(footnote.kind).unwrap(), "Footnote");
    assert_eq!(verse.text, "For God so loved the world.");
    assert!(!verse.text.contains(&footnote.text));
}

#[tokio::test]
async fn canonical_package_footnote_text_stays_out_of_verse_chunks() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();
    let source_id = pipeline.add_source(&fixture("verse-footnote")).unwrap();

    pipeline.ingest_source(&source_id).await.unwrap();
    let footnote_text = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()
        .into_iter()
        .find(|unit| unit.id.0 == "pkg:john-3-16-note-1")
        .unwrap()
        .text;
    let verse_chunks = pipeline
        .store()
        .list_chunks_by_source(&source_id)
        .unwrap()
        .into_iter()
        .filter(|chunk| {
            chunk
                .evidence_unit_ids
                .iter()
                .any(|id| id.0 == "pkg:john-3-16")
        })
        .collect::<Vec<_>>();

    assert!(!verse_chunks.is_empty());
    assert!(verse_chunks
        .iter()
        .all(|chunk| !chunk.text.contains(&footnote_text)));
}

#[tokio::test]
async fn canonical_package_footnote_relation_resolves_to_verse_anchor() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();
    let source_id = pipeline.add_source(&fixture("verse-footnote")).unwrap();

    pipeline.ingest_source(&source_id).await.unwrap();
    let note_node = verbatim_core::types::GraphNodeId::new(
        &source_id,
        verbatim_core::types::GraphNodeKind::EvidenceUnit,
        "pkg:john-3-16-note-1",
    );
    let verse_node = verbatim_core::types::GraphNodeId::new(
        &source_id,
        verbatim_core::types::GraphNodeKind::EvidenceUnit,
        "pkg:john-3-16",
    );

    assert!(pipeline
        .store()
        .list_graph_edges_by_source(&source_id)
        .unwrap()
        .iter()
        .any(|edge| {
            serde_json::to_value(edge.edge_type).unwrap() == "footnote_references_verse"
                && edge.from_node_id == note_node
                && edge.to_node_id == verse_node
        }));
}

#[test]
fn canonical_package_rejects_unknown_content_kind() {
    let tempdir = tempfile::tempdir().unwrap();
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let error = pipeline
        .add_source(&fixture("unknown-content-kind"))
        .unwrap_err()
        .to_string();

    assert!(error.contains("CANONICAL_PACKAGE_UNIT_CONTENT_KIND_UNKNOWN"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
}

#[test]
fn canonical_package_rejects_relation_to_missing_unit() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = relation_package(
        &tempdir,
        r#"{"relation_type":"footnote_references_verse","from_unit_id":"pkg:john-3-16-note-1","to_unit_id":"pkg:missing"}"#,
    );
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let error = pipeline.add_source(&package).unwrap_err().to_string();

    assert!(error.contains("CANONICAL_PACKAGE_RELATION_ENDPOINT_UNKNOWN"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
}

#[test]
fn canonical_package_rejects_reversed_footnote_relation() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = relation_package(
        &tempdir,
        r#"{"relation_type":"footnote_references_verse","from_unit_id":"pkg:john-3-16","to_unit_id":"pkg:john-3-16-note-1"}"#,
    );
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let error = pipeline.add_source(&package).unwrap_err().to_string();

    assert!(error.contains("CANONICAL_PACKAGE_RELATION_KIND_INVALID"));
    assert!(pipeline.store().list_sources().unwrap().is_empty());
}

#[test]
fn canonical_package_rejects_duplicate_unit_id() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("duplicate-unit-id");
    fs::create_dir(&package).unwrap();
    fs::copy(
        fixture("valid").join("manifest.json"),
        package.join("manifest.json"),
    )
    .unwrap();
    let units = fs::read_to_string(fixture("valid").join("units.jsonl"))
        .unwrap()
        .replace("pkg:john-4-1", "pkg:john-3-16");
    fs::write(package.join("units.jsonl"), units).unwrap();
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();

    let report = validate_package(&package);

    assert!(!report.valid);
    assert!(report
        .diagnostics
        .iter()
        .any(|item| item.code == "CANONICAL_PACKAGE_UNIT_ID_DUPLICATE"));
    assert!(pipeline.add_source(&package).is_err());
    assert!(pipeline.store().list_sources().unwrap().is_empty());
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
        r#"{"unit_id":"pkg:invalid-components","source_profile":"bible","work_id":"KJV","version_id":"public-domain","language":"en","components":[{}],"text":"text","backing_selectors":[{"type":"LineRange","start":1,"end":1}]}"#,
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
            r#"{"unit_id":"pkg:invalid-schema","source_profile":"bible","work_id":"KJV","version_id":"public-domain","language":"en","components":[{"level":"book","value":"John"}],"text":"text","backing_selectors":[{"type":"LineRange","start":1,"end":1}]}"#,
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
