use std::path::PathBuf;

use crate::index_gc::{
    IndexGcApplyReport, IndexGcArtifactKind, IndexGcConfig, IndexGcPlan, IndexGcPlanEntry,
    IndexGcSkippedEntry,
};
use crate::wire_schemas::{wire_content_hash, WireArtifactKind, WIRE_SCHEMA_VERSION};
use serde_json::json;

fn entry(path: &str, kind: IndexGcArtifactKind, bytes: u64, reason: &str) -> IndexGcPlanEntry {
    IndexGcPlanEntry {
        path: PathBuf::from(path),
        kind,
        profile_id: Some("old-profile".into()),
        generation: Some(1),
        approximate_bytes: bytes,
        reason: reason.into(),
    }
}

fn response(dry_run: bool, populated_artifacts: bool) -> super::IndexGcResponse {
    let planned_entry = populated_artifacts.then(|| {
        entry(
            "/tmp/verbatim/indexes/profiles/old-profile/gen-1",
            IndexGcArtifactKind::Generation,
            4096,
            "obsolete generation",
        )
    });
    let skipped_entry = populated_artifacts.then(|| IndexGcSkippedEntry {
        path: PathBuf::from("/tmp/verbatim/indexes/staging-fresh"),
        kind: Some(IndexGcArtifactKind::Staging),
        profile_id: None,
        generation: None,
        reason: "staging directory is fresh".into(),
    });
    let planned_bytes = planned_entry
        .as_ref()
        .map_or(0, |entry| entry.approximate_bytes);
    let plan = IndexGcPlan {
        entries: planned_entry.into_iter().collect(),
        skipped: skipped_entry.into_iter().collect(),
        approximate_reclaim_bytes: planned_bytes,
    };
    let apply = if dry_run {
        IndexGcApplyReport::default()
    } else {
        IndexGcApplyReport {
            removed: plan.entries.clone(),
            reclaimed_bytes: planned_bytes,
        }
    };
    super::IndexGcResponse::new(
        dry_run,
        IndexGcConfig {
            retain_previous_generations: 1,
            stale_staging_seconds: 86_400,
        },
        plan,
        apply,
    )
    .expect("index GC response fixture")
}

#[test]
fn index_gc_result_identity_is_canonical_and_fail_closed() {
    for dry_run in [true, false] {
        for populated_artifacts in [false, true] {
            let response = response(dry_run, populated_artifacts);
            let wire = serde_json::to_value(&response).expect("index GC response encodes");

            assert_eq!(wire["identity"]["kind"], "index_gc_result");
            assert_eq!(
                wire["identity"]["schema_version"],
                json!(WIRE_SCHEMA_VERSION)
            );
            assert_eq!(wire["identity"]["artifact_id"], "index-gc");
            assert_eq!(
                serde_json::from_value::<super::IndexGcResponse>(wire.clone())
                    .expect("index GC response decodes"),
                response
            );

            let mut missing_identity = wire.clone();
            missing_identity.as_object_mut().unwrap().remove("identity");
            assert!(serde_json::from_value::<super::IndexGcResponse>(missing_identity).is_err());

            let mut dry_run_mutation = wire.clone();
            dry_run_mutation["dry_run"] = json!(!dry_run);
            assert!(serde_json::from_value::<super::IndexGcResponse>(dry_run_mutation).is_err());

            for (field, value) in [
                ("retain_previous_generations", json!(99)),
                ("stale_staging_seconds", json!(99)),
            ] {
                let mut mutated = wire.clone();
                mutated["policy"][field] = value;
                assert!(
                    serde_json::from_value::<super::IndexGcResponse>(mutated).is_err(),
                    "policy.{field} mutation must be rejected"
                );
            }

            let mut plan_bytes_mutation = wire.clone();
            plan_bytes_mutation["plan"]["approximate_reclaim_bytes"] = json!(99);
            assert!(serde_json::from_value::<super::IndexGcResponse>(plan_bytes_mutation).is_err());

            let mut apply_bytes_mutation = wire.clone();
            apply_bytes_mutation["apply"]["reclaimed_bytes"] = json!(99);
            assert!(
                serde_json::from_value::<super::IndexGcResponse>(apply_bytes_mutation).is_err()
            );

            if populated_artifacts {
                for (field, value) in [
                    ("path", json!("/tmp/replacement")),
                    ("kind", json!("staging")),
                    ("profile_id", json!("replacement-profile")),
                    ("generation", json!(99)),
                    ("approximate_bytes", json!(99)),
                    ("reason", json!("replacement")),
                ] {
                    let mut mutated = wire.clone();
                    mutated["plan"]["entries"][0][field] = value.clone();
                    assert!(
                        serde_json::from_value::<super::IndexGcResponse>(mutated).is_err(),
                        "plan.entries.{field} mutation must be rejected"
                    );
                }
                for (field, value) in [
                    ("path", json!("/tmp/replacement")),
                    ("kind", json!("generation")),
                    ("profile_id", json!("replacement-profile")),
                    ("generation", json!(99)),
                    ("reason", json!("replacement")),
                ] {
                    let mut mutated = wire.clone();
                    mutated["plan"]["skipped"][0][field] = value;
                    assert!(
                        serde_json::from_value::<super::IndexGcResponse>(mutated).is_err(),
                        "plan.skipped.{field} mutation must be rejected"
                    );
                }
                if !dry_run {
                    for (field, value) in [
                        ("path", json!("/tmp/replacement")),
                        ("kind", json!("staging")),
                        ("profile_id", json!("replacement-profile")),
                        ("generation", json!(99)),
                        ("approximate_bytes", json!(99)),
                        ("reason", json!("replacement")),
                    ] {
                        let mut mutated = wire.clone();
                        mutated["apply"]["removed"][0][field] = value;
                        assert!(
                            serde_json::from_value::<super::IndexGcResponse>(mutated).is_err(),
                            "apply.removed.{field} mutation must be rejected"
                        );
                    }
                }
            }

            for (field, value) in [
                ("kind", json!(WireArtifactKind::VectorJsonCleanupResult)),
                (
                    "schema_version",
                    json!({"major": 2, "minor": 0, "patch": 0}),
                ),
                ("artifact_id", json!("other")),
                ("content_hash", json!(wire_content_hash(&[0]))),
            ] {
                let mut mutated = wire.clone();
                mutated["identity"][field] = value;
                assert!(
                    serde_json::from_value::<super::IndexGcResponse>(mutated).is_err(),
                    "identity.{field} mutation must be rejected"
                );
            }

            let mut serialization_mutation = response;
            serialization_mutation.policy.retain_previous_generations += 1;
            assert!(serde_json::to_value(serialization_mutation).is_err());
        }
    }
}
