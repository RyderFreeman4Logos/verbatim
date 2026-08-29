use std::path::PathBuf;

use crate::index_profile_delete::{
    IndexProfileArtifactPlan, IndexProfileDeleteApplyReport, IndexProfileDeletePlan,
    IndexProfileDeleteSkippedEntry,
};
use crate::store::EmbeddingProfileStorageCounts;
use crate::wire_schemas::{wire_content_hash, WireArtifactKind, WIRE_SCHEMA_VERSION};
use serde_json::json;

fn artifact(path: &str, bytes: u64, reason: &str) -> IndexProfileArtifactPlan {
    IndexProfileArtifactPlan {
        path: PathBuf::from(path),
        approximate_bytes: bytes,
        reason: reason.into(),
    }
}

fn counts(offset: u64) -> EmbeddingProfileStorageCounts {
    EmbeddingProfileStorageCounts {
        chunk_vectors: offset + 1,
        embedding_cache_entries: offset + 2,
        source_embedding_statuses: offset + 3,
        embeddings_meta_entries: offset + 4,
        embedding_profile_index_meta_entries: offset + 5,
        embedding_profiles: offset + 6,
    }
}

fn response(dry_run: bool, populated_artifacts: bool) -> super::IndexProfileDeleteResponse {
    let planned_artifact = populated_artifacts.then(|| {
        artifact(
            "/tmp/verbatim/indexes/profiles/old-profile",
            4096,
            "obsolete",
        )
    });
    let planned_bytes = planned_artifact
        .as_ref()
        .map_or(0, |artifact| artifact.approximate_bytes);
    let plan = IndexProfileDeletePlan {
        profile_id: "old-profile".into(),
        active_profile: false,
        sqlite: counts(0),
        artifact: planned_artifact.clone(),
        skipped: populated_artifacts
            .then(|| IndexProfileDeleteSkippedEntry {
                path: PathBuf::from("/tmp/verbatim/indexes/profiles/skipped"),
                reason: "symlink".into(),
            })
            .into_iter()
            .collect(),
        approximate_reclaim_bytes: planned_bytes,
    };
    let apply = if dry_run {
        IndexProfileDeleteApplyReport::default()
    } else {
        IndexProfileDeleteApplyReport {
            sqlite: counts(10),
            removed_artifacts: planned_artifact.into_iter().collect(),
            reclaimed_bytes: planned_bytes,
        }
    };
    super::IndexProfileDeleteResponse::new(dry_run, plan, apply)
        .expect("profile deletion response fixture")
}

#[test]
fn index_profile_delete_result_identity_is_canonical_and_fail_closed() {
    for dry_run in [true, false] {
        for populated_artifacts in [false, true] {
            let response = response(dry_run, populated_artifacts);
            let wire = serde_json::to_value(&response).expect("profile deletion response encodes");

            assert_eq!(wire["identity"]["kind"], "index_profile_delete_result");
            assert_eq!(
                wire["identity"]["schema_version"],
                json!(WIRE_SCHEMA_VERSION)
            );
            assert_eq!(wire["identity"]["artifact_id"], "index-profile-delete");
            assert_eq!(
                serde_json::from_value::<super::IndexProfileDeleteResponse>(wire.clone())
                    .expect("profile deletion response decodes"),
                response
            );

            let mut missing_identity = wire.clone();
            missing_identity.as_object_mut().unwrap().remove("identity");
            assert!(
                serde_json::from_value::<super::IndexProfileDeleteResponse>(missing_identity)
                    .is_err()
            );

            let mut dry_run_mutation = wire.clone();
            dry_run_mutation["dry_run"] = json!(!dry_run);
            assert!(
                serde_json::from_value::<super::IndexProfileDeleteResponse>(dry_run_mutation)
                    .is_err()
            );

            for (field, value) in [
                ("profile_id", json!("replacement-profile")),
                ("active_profile", json!(true)),
                ("approximate_reclaim_bytes", json!(99)),
            ] {
                let mut mutated = wire.clone();
                mutated["plan"][field] = value;
                assert!(
                    serde_json::from_value::<super::IndexProfileDeleteResponse>(mutated).is_err(),
                    "plan.{field} mutation must be rejected"
                );
            }
            for field in [
                "chunk_vectors",
                "embedding_cache_entries",
                "source_embedding_statuses",
                "embeddings_meta_entries",
                "embedding_profile_index_meta_entries",
                "embedding_profiles",
            ] {
                let mut mutated = wire.clone();
                mutated["plan"]["sqlite"][field] = json!(99);
                assert!(
                    serde_json::from_value::<super::IndexProfileDeleteResponse>(mutated).is_err(),
                    "plan.sqlite.{field} mutation must be rejected"
                );
                let mut mutated = wire.clone();
                mutated["apply"]["sqlite"][field] = json!(99);
                assert!(
                    serde_json::from_value::<super::IndexProfileDeleteResponse>(mutated).is_err(),
                    "apply.sqlite.{field} mutation must be rejected"
                );
            }

            if populated_artifacts {
                let mut artifact_removal = wire.clone();
                artifact_removal["plan"]["artifact"] = serde_json::Value::Null;
                assert!(
                    serde_json::from_value::<super::IndexProfileDeleteResponse>(artifact_removal)
                        .is_err(),
                    "plan.artifact mutation must be rejected"
                );
                let mut skipped_removal = wire.clone();
                skipped_removal["plan"]["skipped"] = json!([]);
                assert!(
                    serde_json::from_value::<super::IndexProfileDeleteResponse>(skipped_removal)
                        .is_err(),
                    "plan.skipped mutation must be rejected"
                );
                for (field, value) in [
                    ("path", json!("/tmp/replacement")),
                    ("approximate_bytes", json!(99)),
                    ("reason", json!("replacement")),
                ] {
                    let mut mutated = wire.clone();
                    mutated["plan"]["artifact"][field] = value.clone();
                    assert!(
                        serde_json::from_value::<super::IndexProfileDeleteResponse>(mutated)
                            .is_err(),
                        "plan.artifact.{field} mutation must be rejected"
                    );
                }
                for (field, value) in [
                    ("path", json!("/tmp/replacement")),
                    ("reason", json!("replacement")),
                ] {
                    let mut mutated = wire.clone();
                    mutated["plan"]["skipped"][0][field] = value;
                    assert!(
                        serde_json::from_value::<super::IndexProfileDeleteResponse>(mutated)
                            .is_err(),
                        "plan.skipped.{field} mutation must be rejected"
                    );
                }
                if !dry_run {
                    let mut removed_artifacts_removal = wire.clone();
                    removed_artifacts_removal["apply"]["removed_artifacts"] = json!([]);
                    assert!(
                        serde_json::from_value::<super::IndexProfileDeleteResponse>(
                            removed_artifacts_removal
                        )
                        .is_err(),
                        "apply.removed_artifacts mutation must be rejected"
                    );
                    for (field, value) in [
                        ("path", json!("/tmp/replacement")),
                        ("approximate_bytes", json!(99)),
                        ("reason", json!("replacement")),
                    ] {
                        let mut mutated = wire.clone();
                        mutated["apply"]["removed_artifacts"][0][field] = value;
                        assert!(
                            serde_json::from_value::<super::IndexProfileDeleteResponse>(mutated)
                                .is_err(),
                            "apply.removed_artifacts.{field} mutation must be rejected"
                        );
                    }
                }
            }

            let mut reclaimed_bytes_mutation = wire.clone();
            reclaimed_bytes_mutation["apply"]["reclaimed_bytes"] = json!(99);
            assert!(serde_json::from_value::<super::IndexProfileDeleteResponse>(
                reclaimed_bytes_mutation
            )
            .is_err());

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
                    serde_json::from_value::<super::IndexProfileDeleteResponse>(mutated).is_err(),
                    "identity.{field} mutation must be rejected"
                );
            }

            let mut serialization_mutation = response;
            serialization_mutation.plan.active_profile =
                !serialization_mutation.plan.active_profile;
            assert!(serde_json::to_value(serialization_mutation).is_err());
        }
    }
}
