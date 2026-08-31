use verbatim_core::api::{query_plan_from_retrieve_request_with_profile, RetrieveRequest};
use verbatim_core::types::EmbeddingProfileId;
use verbatim_core::wire_schemas::QueryPlanEnvelope;

pub(super) fn test_query_plan(question: &str) -> QueryPlanEnvelope {
    let request = serde_json::from_value::<RetrieveRequest>(serde_json::json!({
        "question": question,
    }))
    .unwrap();
    query_plan_from_retrieve_request_with_profile(
        &request,
        Some(EmbeddingProfileId::default_profile().as_str()),
    )
    .unwrap()
}
