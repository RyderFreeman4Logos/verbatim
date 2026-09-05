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
