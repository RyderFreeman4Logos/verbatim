use crate::collection::{CollectionSyncReport, CollectionSyncSkip, CollectionSyncSkipReason};
use crate::wire_schemas::encode_wire_document;
use crate::wire_schemas::{
    CanonicalIdentity as SyncCanonicalIdentity, ContentHash as SyncContentHash,
    WireArtifactKind as SyncWireArtifactKind, WireSchemaVersion as SyncWireSchemaVersion,
    WIRE_SCHEMA_VERSION,
};

use serde::Serialize;

#[derive(Serialize)]
struct CollectionSyncReportBody<'a> {
    report: &'a CollectionSyncReport,
}

fn collection_sync_report(skipped: Vec<CollectionSyncSkip>) -> CollectionSyncReport {
    CollectionSyncReport {
        member_count: 1,
        added: 1,
        removed: 0,
        unchanged: 0,
        scanned_roots: 1,
        max_depth: 32,
        skipped,
    }
}

fn non_empty_skip() -> CollectionSyncSkip {
    CollectionSyncSkip {
        path: "/tmp/articles/ignored.md".into(),
        logical_path: Some("ignored.md".into()),
        reason: CollectionSyncSkipReason::Ignored,
        message: "ignored by collection rules".into(),
    }
}

#[test]
fn collection_sync_result_identity() {
    for skipped in [Vec::new(), vec![non_empty_skip()]] {
        let report = collection_sync_report(skipped);
        let response = super::CollectionSyncResponse::new("articles", report.clone()).unwrap();
        let wire = serde_json::to_value(&response).unwrap();
        let expected = SyncCanonicalIdentity::from_body(
            SyncWireArtifactKind::CollectionSyncResult,
            WIRE_SCHEMA_VERSION,
            "articles",
            &encode_wire_document(&CollectionSyncReportBody { report: &report }).unwrap(),
        )
        .unwrap();

        assert_eq!(wire["identity"]["kind"], "collection_sync_result");
        assert_eq!(wire["identity"]["artifact_id"], "articles");
        assert_eq!(
            wire["identity"]["schema_version"],
            serde_json::json!({"major": 1, "minor": 0, "patch": 0})
        );
        assert_eq!(wire["identity"]["content_hash"], expected.content_hash.as_str());
        assert_eq!(serde_json::from_value::<super::CollectionSyncResponse>(wire.clone()).unwrap(), response);

        let missing_identity = serde_json::json!({"report": wire["report"]});
        assert!(serde_json::from_value::<super::CollectionSyncResponse>(missing_identity).is_err());

        for (field, value) in [
            ("member_count", serde_json::json!(2)),
            ("added", serde_json::json!(2)),
            ("removed", serde_json::json!(2)),
            ("unchanged", serde_json::json!(2)),
            ("scanned_roots", serde_json::json!(2)),
            ("max_depth", serde_json::json!(2)),
        ] {
            let mut mutated = wire.clone();
            mutated["report"][field] = value;
            assert!(
                serde_json::from_value::<super::CollectionSyncResponse>(mutated).is_err(),
                "report field {field} mutation must be rejected"
            );
        }

        let mut mutated = wire.clone();
        mutated["report"]["skipped"] = if report.skipped.is_empty() {
            serde_json::json!([non_empty_skip()])
        } else {
            serde_json::json!([])
        };
        assert!(serde_json::from_value::<super::CollectionSyncResponse>(mutated).is_err());

        for (field, value) in [
            ("kind", serde_json::json!("source_record")),
            (
                "schema_version",
                serde_json::json!({"major": 2, "minor": 0, "patch": 0}),
            ),
            ("artifact_id", serde_json::json!("other/name")),
            (
                "content_hash",
                serde_json::json!("0000000000000000000000000000000000000000000000000000000000000000"),
            ),
        ] {
            let mut mutated = wire.clone();
            mutated["identity"][field] = value;
            assert!(
                serde_json::from_value::<super::CollectionSyncResponse>(mutated).is_err(),
                "identity field {field} mutation must be rejected"
            );
        }

        for (field, value) in [
            ("member_count", 2),
            ("added", 2),
            ("removed", 2),
            ("unchanged", 2),
            ("scanned_roots", 2),
            ("max_depth", 2),
        ] {
            let mut mutated = response.clone();
            match field {
                "member_count" => mutated.report.member_count = value,
                "added" => mutated.report.added = value,
                "removed" => mutated.report.removed = value,
                "unchanged" => mutated.report.unchanged = value,
                "scanned_roots" => mutated.report.scanned_roots = value,
                "max_depth" => mutated.report.max_depth = value,
                _ => unreachable!(),
            }
            assert!(serde_json::to_value(mutated).is_err());
        }

        let mut mutated = response.clone();
        mutated.report.skipped = if report.skipped.is_empty() {
            vec![non_empty_skip()]
        } else {
            Vec::new()
        };
        assert!(serde_json::to_value(mutated).is_err());

        let mut mutated = response.clone();
        mutated.identity.kind = SyncWireArtifactKind::SourceRecord;
        assert!(serde_json::to_value(mutated).is_err());

        let mut mutated = response.clone();
        mutated.identity.schema_version = SyncWireSchemaVersion::new(2, 0, 0);
        assert!(serde_json::to_value(mutated).is_err());

        let mut mutated = response.clone();
        mutated.identity.artifact_id = "other/name".into();
        assert!(serde_json::to_value(mutated).is_err());

        let mut mutated = response;
        mutated.identity.content_hash = SyncContentHash::new("0".repeat(64)).unwrap();
        assert!(serde_json::to_value(mutated).is_err());
    }
}
