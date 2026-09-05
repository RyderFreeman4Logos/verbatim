use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use verbatim_core::config::Config;
use verbatim_core::ingest::IngestPipeline;
use verbatim_core::parser::canonical_package::{validate_package, CanonicalPackageParser};
use verbatim_core::traits::Parser;
use verbatim_core::types::{hex_sha256, SourceLocator};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/canonical_package/valid")
}

fn package(root: &Path) -> PathBuf {
    let path = root.join("package");
    fs::create_dir(&path).unwrap();
    for name in ["manifest.json", "units.jsonl"] {
        fs::copy(fixture().join(name), path.join(name)).unwrap();
    }
    path
}

fn edit_manifest(path: &Path, edit: impl FnOnce(&mut Value)) {
    let file = path.join("manifest.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    edit(&mut value);
    fs::write(file, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn edit_units(path: &Path, edit: impl FnOnce(&mut Vec<Value>)) {
    let file = path.join("units.jsonl");
    let mut values = fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    edit(&mut values);
    let bytes = values
        .iter()
        .map(|value| format!("{value}\n"))
        .collect::<String>();
    fs::write(file, &bytes).unwrap();
    edit_manifest(path, |manifest| {
        manifest["conversion"]["output_hash"] = json!(hex_sha256(bytes.as_bytes()))
    });
}

fn rejected(path: &Path, root: &Path, code: &str, location: &str) {
    let report = validate_package(path);
    assert!(!report.valid, "accepted {code}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == code && d.location == location),
        "{code} at {location}: {:?}",
        report.diagnostics
    );
    assert!(CanonicalPackageParser.parse(path).is_err());
    let pipeline = IngestPipeline::new(&Config::default(), root).unwrap();
    assert!(pipeline.add_source(path).is_err());
    assert!(pipeline.store().list_sources().unwrap().is_empty());
    assert!(pipeline
        .store()
        .list_evidence_by_source(&verbatim_core::types::SourceId::from_path(path))
        .unwrap()
        .is_empty());
}

#[test]
fn canonical_package_requires_complete_provenance() {
    for field in [
        "canon_id",
        "versification_id",
        "original_source",
        "original_source_hash",
        "conversion",
        "converter_artifact_hash",
        "validation",
        "rights",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = package(root.path());
        edit_manifest(&path, |m| {
            m.as_object_mut().unwrap().remove(field);
        });
        rejected(
            &path,
            root.path(),
            "CANONICAL_PACKAGE_MANIFEST_REQUIRED_FIELD",
            &format!("manifest.json:{field}"),
        );
    }
}

#[test]
fn canonical_package_checks_hierarchy_order_and_ranges() {
    let cases = [
        ("missing-level", "CANONICAL_PACKAGE_HIERARCHY_INVALID"),
        ("reordered-level", "CANONICAL_PACKAGE_HIERARCHY_INVALID"),
        ("missing-ordinal", "CANONICAL_PACKAGE_ORDINAL_INVALID"),
        ("wrong-ordinal", "CANONICAL_PACKAGE_ORDINAL_INVALID"),
        ("backwards", "CANONICAL_PACKAGE_ORDER_INVALID"),
        ("duplicate-locator", "CANONICAL_PACKAGE_LOCATOR_DUPLICATE"),
        ("reversed-range", "CANONICAL_PACKAGE_RANGE_INVALID"),
        ("malformed-range", "CANONICAL_PACKAGE_RANGE_INVALID"),
        ("selector-range", "CANONICAL_PACKAGE_SELECTOR_INVALID"),
        ("hash-missing", "CANONICAL_PACKAGE_UNIT_REQUIRED_FIELD"),
        ("mixed-work", "CANONICAL_PACKAGE_UNIT_MANIFEST_MISMATCH"),
        ("mixed-edition", "CANONICAL_PACKAGE_UNIT_MANIFEST_MISMATCH"),
        ("mixed-profile", "CANONICAL_PACKAGE_UNIT_MANIFEST_MISMATCH"),
        ("annotations", "CANONICAL_PACKAGE_ANNOTATIONS_INVALID"),
    ];
    for (case, code) in cases {
        let root = tempfile::tempdir().unwrap();
        let path = package(root.path());
        edit_units(&path, |units| {
            let original = units[0].clone();
            let unit = &mut units[1];
            match case {
                "missing-level" => {
                    unit["components"].as_array_mut().unwrap().pop();
                }
                "reordered-level" => unit["components"].as_array_mut().unwrap().swap(0, 1),
                "missing-ordinal" => {
                    unit["components"][2]
                        .as_object_mut()
                        .unwrap()
                        .remove("ordinal");
                }
                "wrong-ordinal" => unit["components"][2]["ordinal"] = json!(2),
                "backwards" => {
                    units.swap(0, 1);
                }
                "duplicate-locator" => unit["components"] = original["components"].clone(),
                "reversed-range" => unit["end_components"] = original["components"].clone(),
                "malformed-range" => unit["end_components"] = json!([{}]),
                "selector-range" => {
                    unit["backing_selectors"] = json!([{"type":"LineRange","start":0,"end":0}])
                }
                "hash-missing" => {
                    unit.as_object_mut().unwrap().remove("text_hash");
                }
                "mixed-work" => unit["work_id"] = json!("other"),
                "mixed-edition" => unit["version_id"] = json!("other"),
                "mixed-profile" => unit["source_profile"] = json!("other"),
                "annotations" => unit["metadata"] = json!({"annotations":{"x":"a".repeat(65)}}),
                _ => unreachable!(),
            }
        });
        rejected(&path, root.path(), code, "units.jsonl:2");
    }
}

#[test]
fn canonical_package_preserves_range_and_annotations() {
    let root = tempfile::tempdir().unwrap();
    let path = package(root.path());
    edit_units(&path, |units| {
        let mut end = units[0]["components"].clone();
        end[2]["value"] = json!("18");
        end[2]["ordinal"] = json!(18);
        units[0]["end_components"] = end;
        units[0]["display_citation"] = json!("John 3:16-18");
        units[0]["backing_selectors"] = json!([{"type":"LineRange","start":1,"end":3}]);
        units[0]["metadata"] =
            json!({"section_heading":"New birth","annotations":{"speaker":"narrator"}});
    });
    let units = CanonicalPackageParser.parse(&path).unwrap();
    let SourceLocator::Canonical { locator } = &units[0].locator else {
        panic!()
    };
    assert_eq!(locator.end.as_ref().unwrap()[2].ordinal, Some(18));
    assert_eq!(locator.normalized, "john:3:16-john:3:18");
    assert_eq!(units[0].annotations["speaker"], "narrator");
    assert_eq!(units[0].heading_path, ["New birth"]);
}

fn bundled_package(root: &Path) -> PathBuf {
    let path = package(root);
    fs::create_dir(path.join("source")).unwrap();
    fs::create_dir(path.join("assets")).unwrap();
    fs::write(path.join("source/original.usfm"), "fixture source").unwrap();
    fs::write(path.join("assets/note.txt"), "asset, not evidence").unwrap();
    edit_manifest(&path, |m| {
        let source_hash = hex_sha256(b"fixture source");
        m["original_source"]["path"] = json!("source/original.usfm");
        m["original_source_hash"] = json!(source_hash);
        m["conversion"]["original_source_hash"] = m["original_source_hash"].clone();
        m["conversion"]["output_hash"] =
            json!(hex_sha256(&fs::read(path.join("units.jsonl")).unwrap()));
        m["files"] = json!([
            {"path":"source/original.usfm","sha256":source_hash,"media_type":"text/plain"},
            {"path":"assets/note.txt","sha256":hex_sha256(b"asset, not evidence"),"media_type":"text/plain"}
        ]);
    });
    path
}

#[test]
fn canonical_package_validates_all_files_before_admission() {
    for (case, code, location) in [
        (
            "asset-hash",
            "CANONICAL_PACKAGE_FILE_HASH_MISMATCH",
            "assets/note.txt",
        ),
        (
            "missing-asset",
            "CANONICAL_PACKAGE_FILE_MISSING",
            "assets/note.txt",
        ),
        (
            "source-hash",
            "CANONICAL_PACKAGE_SOURCE_HASH_MISMATCH",
            "source/original.usfm",
        ),
        (
            "output-hash",
            "CANONICAL_PACKAGE_OUTPUT_HASH_MISMATCH",
            "units.jsonl",
        ),
        (
            "path-traversal",
            "CANONICAL_PACKAGE_FILE_PATH_INVALID",
            "manifest.json:files.1.path",
        ),
        (
            "duplicate-file",
            "CANONICAL_PACKAGE_FILE_DUPLICATE",
            "manifest.json:files.1.path",
        ),
        (
            "unlisted",
            "CANONICAL_PACKAGE_FILE_UNDECLARED",
            "assets/extra.txt",
        ),
        ("symlink", "CANONICAL_PACKAGE_LAYOUT_INVALID", "package"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = bundled_package(root.path());
        assert!(validate_package(&path).valid);
        match case {
            "asset-hash" => fs::write(path.join("assets/note.txt"), "changed").unwrap(),
            "missing-asset" => fs::remove_file(path.join("assets/note.txt")).unwrap(),
            "source-hash" => edit_manifest(&path, |m| {
                m["original_source_hash"] = json!("a".repeat(64));
                m["conversion"]["original_source_hash"] = m["original_source_hash"].clone();
            }),
            "output-hash" => edit_manifest(&path, |m| {
                m["conversion"]["output_hash"] = json!("a".repeat(64))
            }),
            "path-traversal" => {
                edit_manifest(&path, |m| m["files"][1]["path"] = json!("../outside"))
            }
            "duplicate-file" => edit_manifest(&path, |m| m["files"][1] = m["files"][0].clone()),
            "unlisted" => fs::write(path.join("assets/extra.txt"), "extra").unwrap(),
            "symlink" => {
                fs::remove_file(path.join("assets/note.txt")).unwrap();
                std::os::unix::fs::symlink("../source/original.usfm", path.join("assets/note.txt"))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        rejected(&path, root.path(), code, location);
    }
}

#[test]
fn canonical_package_asset_bytes_bind_package_and_report_hashes() {
    let root = tempfile::tempdir().unwrap();
    let path = bundled_package(root.path());
    let before = validate_package(&path);
    fs::write(path.join("assets/note.txt"), "changed").unwrap();
    let after = validate_package(&path);
    assert_ne!(before.package_hash, after.package_hash);
    assert_ne!(before.report_hash, after.report_hash);
}

#[test]
fn canonical_package_report_is_path_independent_and_recomputable() {
    let root = tempfile::tempdir().unwrap();
    let path = bundled_package(root.path());
    let first = validate_package(&path);
    let moved = root.path().join("moved");
    fs::rename(&path, &moved).unwrap();
    let second = validate_package(&moved);
    assert_eq!(first.report_hash, second.report_hash);
    let mut payload = serde_json::to_value(&first).unwrap();
    payload.as_object_mut().unwrap().remove("report_hash");
    assert_eq!(
        first.report_hash,
        hex_sha256(&serde_json::to_vec(&payload).unwrap())
    );
}

#[tokio::test]
async fn canonical_package_complete_fixture_preserves_records_and_ingests_deterministically() {
    let path = fixture().parent().unwrap().join("complete");
    let originals: Vec<Value> = fs::read_to_string(path.join("source/original.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let records: Vec<Value> = fs::read_to_string(path.join("units.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for original in &originals {
        let record = records
            .iter()
            .find(|r| r["components"] == original["components"])
            .unwrap();
        for (key, value) in original.as_object().unwrap() {
            assert_eq!(&record[key], value, "lost {key}");
        }
    }
    let report = validate_package(&path);
    assert!(report.valid, "{:?}", report.diagnostics);
    assert!(report.warnings.is_empty());
    let expected = CanonicalPackageParser.parse(&path).unwrap();
    assert_eq!(expected.len(), 8);
    let root = tempfile::tempdir().unwrap();
    let mut pipeline = IngestPipeline::new(&Config::default(), root.path()).unwrap();
    let id = pipeline.add_source(&path).unwrap();
    pipeline.ingest_source(&id).await.unwrap();
    let first = pipeline.store().list_evidence_by_source(&id).unwrap();
    pipeline.ingest_source(&id).await.unwrap();
    let second = pipeline.store().list_evidence_by_source(&id).unwrap();
    assert_eq!(first, second);
    assert_eq!(first, expected);
    assert_eq!(validate_package(&path).report_hash, report.report_hash);
}

#[test]
fn canonical_package_migration_preserves_legacy_ids_metadata_and_raw_source() {
    use verbatim_core::parser::canonical_jsonl::CanonicalJsonlParser;
    use verbatim_core::parser::canonical_package::migrate_legacy_jsonl;
    let root = tempfile::tempdir().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/canonical_bible.jsonl");
    let template = root.path().join("identity.json");
    fs::write(
        &template,
        r#"{"language":"mul","rights":{"license":"CC0-1.0","statement":"Repository fixture"}}"#,
    )
    .unwrap();
    let output = root.path().join("package");
    let report = migrate_legacy_jsonl(&input, &template, &output).unwrap();
    assert!(report.valid);
    assert_eq!(
        fs::read(&input).unwrap(),
        fs::read(output.join("source/original.jsonl")).unwrap()
    );
    let legacy = CanonicalJsonlParser.parse(&input).unwrap();
    let migrated = CanonicalPackageParser.parse(&output).unwrap();
    for (before, after) in legacy.iter().zip(&migrated) {
        assert_eq!(before.id, after.id);
        assert_eq!(before.text, after.text);
        assert_eq!(before.text_hash, after.text_hash);
        assert_eq!(before.annotations, after.annotations);
        assert_eq!(before.heading_path, after.heading_path);
    }
    let second = root.path().join("second");
    assert_eq!(
        migrate_legacy_jsonl(&input, &template, &second)
            .unwrap()
            .report_hash,
        report.report_hash
    );
    assert!(migrate_legacy_jsonl(&input, &template, &output).is_err());
    fs::write(&template, r#"{"language":"mul"}"#).unwrap();
    let invalid = root.path().join("invalid");
    assert!(migrate_legacy_jsonl(&input, &template, &invalid).is_err());
    assert!(!invalid.exists());
    assert!(!fs::read_dir(root.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".verbatim-package-")));
}

#[test]
fn canonical_package_rejects_duplicate_relation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("package");
    fs::create_dir(&path).unwrap();
    let fixture = fixture().parent().unwrap().join("verse-footnote");
    for name in ["manifest.json", "units.jsonl"] {
        fs::copy(fixture.join(name), path.join(name)).unwrap();
    }
    let relation = fs::read_to_string(fixture.join("relations.jsonl")).unwrap();
    fs::write(
        path.join("relations.jsonl"),
        format!("{relation}{relation}"),
    )
    .unwrap();
    rejected(
        &path,
        root.path(),
        "CANONICAL_PACKAGE_RELATION_DUPLICATE",
        "relations.jsonl:2",
    );
}

#[test]
fn canonical_package_invalid_report_hash_is_recomputable() {
    let root = tempfile::tempdir().unwrap();
    let report = validate_package(root.path());
    assert!(!report.valid);
    let mut payload = serde_json::to_value(&report).unwrap();
    payload.as_object_mut().unwrap().remove("report_hash");
    assert_eq!(
        report.report_hash,
        hex_sha256(&serde_json::to_vec(&payload).unwrap())
    );
}

#[test]
fn canonical_package_manifest_and_selector_golden_rejections() {
    for (pointer, value, code) in [
        (
            "/profile",
            json!("unknown"),
            "CANONICAL_PACKAGE_PROFILE_UNSUPPORTED",
        ),
        (
            "/original_source/format",
            json!(""),
            "CANONICAL_PACKAGE_SOURCE_INVALID",
        ),
        (
            "/original_source_hash",
            json!("bad"),
            "CANONICAL_PACKAGE_HASH_INVALID",
        ),
        (
            "/converter_artifact_hash",
            json!("bad"),
            "CANONICAL_PACKAGE_HASH_INVALID",
        ),
        (
            "/conversion/original_source_hash",
            json!("a".repeat(64)),
            "CANONICAL_PACKAGE_CONVERSION_INVALID",
        ),
        (
            "/validation/status",
            json!("failed"),
            "CANONICAL_PACKAGE_VALIDATION_INVALID",
        ),
        (
            "/rights/statement",
            json!(""),
            "CANONICAL_PACKAGE_RIGHTS_INVALID",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = package(root.path());
        edit_manifest(&path, |m| *m.pointer_mut(pointer).unwrap() = value);
        let report = validate_package(&path);
        assert!(!report.valid);
        assert!(
            report.diagnostics.iter().any(|d| d.code == code),
            "{pointer}: {:?}",
            report.diagnostics
        );
        assert!(IngestPipeline::new(&Config::default(), root.path())
            .unwrap()
            .add_source(&path)
            .is_err());
    }
    for selector in [
        json!({"type":"ByteRange","start":4,"end":4}),
        json!({"type":"LineRange","start":2,"end":1}),
        json!({"type":"TextQuote","exact":" "}),
        json!({"type":"XmlId","id":"bad id"}),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = package(root.path());
        edit_units(&path, |units| {
            units[0]["backing_selectors"] = json!([selector])
        });
        rejected(
            &path,
            root.path(),
            "CANONICAL_PACKAGE_SELECTOR_INVALID",
            "units.jsonl:1",
        );
    }
}
