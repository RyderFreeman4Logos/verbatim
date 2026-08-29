use crate::store::{
    VectorJsonCleanupCleared, VectorJsonCleanupReport, VectorJsonCleanupTableStats,
    VectorJsonCleanupTables,
};
use crate::wire_schemas::{wire_content_hash, WIRE_SCHEMA_VERSION};
use serde_json::json;

fn response(dry_run: bool) -> super::VectorJsonCleanupResponse {
    super::VectorJsonCleanupResponse::new(
        dry_run,
        VectorJsonCleanupReport {
            tables: VectorJsonCleanupTables {
                chunk_vectors: VectorJsonCleanupTableStats {
                    eligible: 1,
                    already_clean: 2,
                    json_only: 3,
                    missing_blob: 4,
                    malformed_blob: 5,
                },
                embedding_cache: VectorJsonCleanupTableStats {
                    eligible: 6,
                    already_clean: 7,
                    json_only: 8,
                    missing_blob: 9,
                    malformed_blob: 10,
                },
            },
            cleared: VectorJsonCleanupCleared {
                chunk_vectors: u64::from(!dry_run),
                embedding_cache: u64::from(!dry_run) * 2,
            },
        },
    )
    .expect("cleanup response fixture")
}

#[test]
fn vector_json_cleanup_result_identity_is_canonical_and_fail_closed() {
    for dry_run in [true, false] {
        let response = response(dry_run);
        let wire = serde_json::to_value(&response).expect("cleanup response encodes");

        assert_eq!(wire["identity"]["kind"], "vector_json_cleanup_result");
        assert_eq!(
            wire["identity"]["schema_version"],
            json!(WIRE_SCHEMA_VERSION)
        );
        assert_eq!(wire["identity"]["artifact_id"], "index-vector-json-cleanup");
        assert_eq!(
            serde_json::from_value::<super::VectorJsonCleanupResponse>(wire.clone())
                .expect("cleanup response decodes"),
            response
        );

        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(
            serde_json::from_value::<super::VectorJsonCleanupResponse>(missing_identity).is_err()
        );

        let mut dry_run_mutation = wire.clone();
        dry_run_mutation["dry_run"] = json!(!dry_run);
        assert!(
            serde_json::from_value::<super::VectorJsonCleanupResponse>(dry_run_mutation).is_err()
        );

        for table in ["chunk_vectors", "embedding_cache"] {
            for field in [
                "eligible",
                "already_clean",
                "json_only",
                "missing_blob",
                "malformed_blob",
            ] {
                let mut mutated = wire.clone();
                mutated["report"]["tables"][table][field] = json!(99);
                assert!(
                    serde_json::from_value::<super::VectorJsonCleanupResponse>(mutated).is_err(),
                    "{table}.{field} mutation must be rejected"
                );
            }
        }

        for field in ["chunk_vectors", "embedding_cache"] {
            let mut mutated = wire.clone();
            mutated["report"]["cleared"][field] = json!(99);
            assert!(
                serde_json::from_value::<super::VectorJsonCleanupResponse>(mutated).is_err(),
                "cleared.{field} mutation must be rejected"
            );
        }

        for (field, value) in [
            ("kind", json!("ingest_result")),
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
                serde_json::from_value::<super::VectorJsonCleanupResponse>(mutated).is_err(),
                "identity.{field} mutation must be rejected"
            );
        }

        let mut serialization_mutation = response;
        serialization_mutation.report.cleared.chunk_vectors += 1;
        assert!(serde_json::to_value(serialization_mutation).is_err());
    }
}
