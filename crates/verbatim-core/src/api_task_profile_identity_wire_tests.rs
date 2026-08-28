use super::TaskProfileResponse;
use crate::task::{TaskId, TaskKind, TaskProfile, TaskStatus};
use crate::wire_schemas::{CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION};
use serde_json::Value;

fn sample_profile() -> TaskProfile {
    TaskProfile {
        schema_version: crate::task::TASK_PROFILE_SCHEMA_VERSION,
        task_id: TaskId("task-profile-fixture".into()),
        task_kind: TaskKind::Retrieve,
        status: TaskStatus::Succeeded,
        queue_wait_ms: 3,
        total_wall_ms: 12,
        controls: Default::default(),
        resources: Default::default(),
        endpoints: Vec::new(),
        retrieve: None,
        ask: None,
    }
}

#[test]
fn task_profile_response_stamps_task_profile_identity() {
    let profile = sample_profile();
    let response = TaskProfileResponse::new(profile.clone()).expect("identity fixture stamps");
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::TaskProfile,
        WIRE_SCHEMA_VERSION,
        profile.task_id.0.clone(),
        &serde_json::to_vec(&profile).unwrap(),
    )
    .unwrap();

    assert_eq!(response.identity, expected);
    let encoded = serde_json::to_value(&response).expect("identity fixture encodes");
    assert_eq!(encoded["profile"], serde_json::to_value(profile).unwrap());
    assert_eq!(encoded["identity"]["kind"], "task_profile");
    assert_eq!(
        encoded["identity"]["schema_version"],
        serde_json::json!({
            "major": 1,
            "minor": 0,
            "patch": 0,
        })
    );
    assert!(!encoded["identity"]["content_hash"]
        .as_str()
        .unwrap_or_default()
        .is_empty());
}

#[test]
fn task_profile_response_round_trips_through_json_fixture() {
    let response = TaskProfileResponse::new(sample_profile()).unwrap();
    let fixture = serde_json::to_string(&response).unwrap();
    let decoded: TaskProfileResponse = serde_json::from_str(&fixture).unwrap();

    assert_eq!(decoded, response);
}

#[test]
fn task_profile_response_rejects_identity_mismatches() {
    for (field, replacement) in [
        ("kind", Value::String("task_run".into())),
        ("artifact_id", Value::String("other-task".into())),
        ("content_hash", Value::String("deadbeef".into())),
    ] {
        let response = TaskProfileResponse::new(sample_profile()).unwrap();
        let mut fixture = serde_json::to_value(response).unwrap();
        fixture["identity"][field] = replacement;
        let error = serde_json::from_value::<TaskProfileResponse>(fixture)
            .expect_err("mismatched task-profile identity must fail closed");
        assert!(
            error.to_string().contains("task-profile identity"),
            "{field}: {error}"
        );
    }

    let response = TaskProfileResponse::new(sample_profile()).unwrap();
    let mut fixture = serde_json::to_value(response).unwrap();
    fixture["identity"]["schema_version"]["major"] = serde_json::json!(9);
    let error = serde_json::from_value::<TaskProfileResponse>(fixture)
        .expect_err("unsupported task-profile identity schema must fail closed");
    assert!(error
        .to_string()
        .contains("unsupported wire schema version"));
}

#[test]
fn task_profile_response_serialization_rejects_stale_identity() {
    let mut response = TaskProfileResponse::new(sample_profile()).unwrap();
    response.identity.artifact_id = "other-task".into();

    let error = serde_json::to_value(response)
        .expect_err("stale task-profile identity must fail closed during serialization");
    assert!(error.to_string().contains("task-profile identity"));
}
