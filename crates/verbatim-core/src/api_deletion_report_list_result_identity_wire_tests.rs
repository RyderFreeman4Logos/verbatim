use crate::deletion::{DeletionReport, PersistedDeletionReport, RetentionPolicy};
use crate::types::SourceId;
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
struct DeletionReportListResponseBody<'a> {
    reports: &'a [PersistedDeletionReport],
}

fn report(source_id: &str, recorded_at: &str) -> PersistedDeletionReport {
    PersistedDeletionReport {
        source_id: SourceId(source_id.into()),
        recorded_at: recorded_at.into(),
        retention_policy: RetentionPolicy::until_backup_expiry(1_800_000_000),
        report: DeletionReport::new(),
    }
}

#[test]
fn deletion_report_list_result_identity_is_canonical_and_fail_closed() {
    for reports in [
        Vec::new(),
        vec![
            report("first-source", "2026-08-30T12:00:00Z"),
            report("second-source", "2026-08-30T12:01:00Z"),
        ],
    ] {
        let response = super::DeletionReportListResponse::new(reports.clone()).unwrap();
        assert_eq!(
            response.identity.kind.as_str(),
            "deletion_report_list_result"
        );
        assert_eq!(response.identity.schema_version, WIRE_SCHEMA_VERSION);
        assert_eq!(response.identity.artifact_id, "deletion-reports");

        let canonical_body =
            encode_wire_document(&DeletionReportListResponseBody { reports: &reports }).unwrap();
        assert!(canonical_body.starts_with(b"{\"reports\":"));
        assert!(!canonical_body
            .windows(b"\"identity\"".len())
            .any(|window| window == b"\"identity\""));
        assert_eq!(
            response.identity.content_hash.as_str(),
            wire_content_hash(&canonical_body)
        );

        let encoded = serde_json::to_string(&response).unwrap();
        assert!(encoded.starts_with("{\"reports\":"));
        assert!(encoded.find("\"reports\"").unwrap() < encoded.find("\"identity\"").unwrap());
        if let Some(first_report) = reports.first() {
            let nested = serde_json::to_string(first_report).unwrap();
            assert!(nested.starts_with("{\"source_id\":"));
            assert!(
                nested.find("\"source_id\"").unwrap() < nested.find("\"recorded_at\"").unwrap()
            );
            assert!(
                nested.find("\"recorded_at\"").unwrap()
                    < nested.find("\"retention_policy\"").unwrap()
            );
            assert!(
                nested.find("\"retention_policy\"").unwrap() < nested.find("\"report\"").unwrap()
            );
            assert!(!nested.contains("\"identity\""));
        }
        let decoded: super::DeletionReportListResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.reports, reports);

        let wire: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let mut body_mutation = wire.clone();
        if reports.is_empty() {
            body_mutation["reports"] = json!([report("added", "2026-08-30T12:02:00Z")]);
        } else {
            body_mutation["reports"][0]["source_id"] = json!("changed");
        }
        assert!(
            serde_json::from_value::<super::DeletionReportListResponse>(body_mutation).is_err(),
            "changed reports with a stale identity must fail closed"
        );

        for (field, value) in [
            ("kind", json!("deletion_report_result")),
            (
                "schema_version",
                json!({"major": 9, "minor": 0, "patch": 0}),
            ),
            ("artifact_id", json!("other")),
            ("content_hash", json!(wire_content_hash(&[0]))),
        ] {
            let mut identity_mutation = wire.clone();
            identity_mutation["identity"][field] = value;
            assert!(
                serde_json::from_value::<super::DeletionReportListResponse>(identity_mutation)
                    .is_err(),
                "identity field {field} mutation must fail closed"
            );
        }

        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(
            serde_json::from_value::<super::DeletionReportListResponse>(missing_identity).is_err()
        );

        let mut unknown_field = wire;
        unknown_field["unexpected"] = json!(true);
        assert!(
            serde_json::from_value::<super::DeletionReportListResponse>(unknown_field).is_err()
        );

        let mut stale_response = response;
        stale_response.identity.artifact_id = "other".into();
        assert!(serde_json::to_string(&stale_response).is_err());
    }
}
