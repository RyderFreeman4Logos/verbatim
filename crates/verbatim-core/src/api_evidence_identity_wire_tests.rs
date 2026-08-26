use super::EvidenceResponse;
use crate::wire_schemas::WIRE_SCHEMA_VERSION;

fn sample_evidence() -> EvidenceResponse {
    serde_json::from_str(include_str!(
        "fixtures/legacy_evidence_response_without_taxonomy.json"
    ))
    .unwrap()
}

#[test]
fn get_evidence_identity_stamps_executed_row() {
    let evidence = sample_evidence();
    let encoded = serde_json::to_value(&evidence).unwrap();
    assert!(encoded.get("evidence_pack").is_none());
    assert!(encoded.get("header").is_none());
    assert_eq!(encoded["identity"]["kind"], "evidence");
    assert_eq!(encoded["identity"]["artifact_id"], evidence.id);
    assert_eq!(
        encoded["identity"]["schema_version"]["major"],
        WIRE_SCHEMA_VERSION.major
    );
    assert_eq!(
        encoded["identity"]["schema_version"]["minor"],
        WIRE_SCHEMA_VERSION.minor
    );
    assert_eq!(
        encoded["identity"]["schema_version"]["patch"],
        WIRE_SCHEMA_VERSION.patch
    );
    let content_hash = encoded["identity"]["content_hash"]
        .as_str()
        .expect("GET /api/evidence must publish content_hash");
    assert!(!content_hash.is_empty());
    assert_eq!(encoded["id"], evidence.id);
    assert_eq!(encoded["text"], evidence.text);
    assert_eq!(encoded["locator"], evidence.locator);
    assert_eq!(encoded["text_hash"], evidence.text_hash);
    let again: EvidenceResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(again.id, evidence.id);
    assert_eq!(again.text, evidence.text);
}

#[test]
fn get_evidence_identity_mismatch_is_rejected() {
    let evidence = sample_evidence();
    let encoded = serde_json::to_value(&evidence).unwrap();

    let mut wrong_id = encoded.clone();
    wrong_id["identity"]["artifact_id"] = serde_json::json!("ev-other");
    serde_json::from_value::<EvidenceResponse>(wrong_id)
        .expect_err("identity artifact_id must match the executed evidence id");

    let mut wrong_hash = encoded.clone();
    wrong_hash["identity"]["content_hash"] = serde_json::json!("deadbeefdeadbeef");
    serde_json::from_value::<EvidenceResponse>(wrong_hash)
        .expect_err("identity content_hash must match the executed evidence body");

    let mut wrong_kind = encoded;
    wrong_kind["identity"]["kind"] = serde_json::json!("evidence_pack");
    serde_json::from_value::<EvidenceResponse>(wrong_kind)
        .expect_err("GET /api/evidence must not wrap a row as EvidencePack");
}
