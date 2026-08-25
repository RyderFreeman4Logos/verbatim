use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{is_false, CollectionFilterRequest, RetrieveRequest, RetrieveResultResponse};
use crate::wire_schemas::{
    EvidencePackEnvelope, EvidencePackFields, QueryPlanEnvelope, QueryPlanFields,
};

const LIVE_RETRIEVE_QUERY_PLAN_ID: &str = "live-retrieve";
const LIVE_RETRIEVE_EVIDENCE_PACK_ID: &str = "live-retrieve-evidence";

pub(super) fn query_plan_from_question(question: &str) -> Result<QueryPlanEnvelope> {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: LIVE_RETRIEVE_QUERY_PLAN_ID.into(),
        query_text: question.to_string(),
        steps: Vec::new(),
        generation: None,
        profile_ref: None,
    })
}

pub(super) fn evidence_pack_from_retrieve(
    query: &str,
    results: &[RetrieveResultResponse],
) -> Result<Option<EvidencePackEnvelope>> {
    let evidence_unit_ids: Vec<String> = results
        .iter()
        .map(|result| result.evidence_id.clone())
        .filter(|id| !id.trim().is_empty())
        .collect();
    if evidence_unit_ids.is_empty() {
        return Ok(None);
    }
    let plan = query_plan_from_question(query)?;
    EvidencePackEnvelope::new(EvidencePackFields {
        artifact_id: LIVE_RETRIEVE_EVIDENCE_PACK_ID.into(),
        evidence_unit_ids,
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        generation: None,
        profile_ref: None,
    })
    .map(Some)
}

#[derive(Debug, Serialize, Deserialize)]
struct RetrieveRequestWire {
    question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query_plan: Option<QueryPlanEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_id: Option<String>,
    #[serde(default, skip_serializing_if = "CollectionFilterRequest::is_empty")]
    collection_filter: CollectionFilterRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedding_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    #[serde(default)]
    fast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rerank: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dense_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bm25_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rerank_top_n: Option<usize>,
    #[serde(default)]
    bypass_cache: bool,
    #[serde(default)]
    include_debug: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    include_debug_packs: bool,
    #[serde(default)]
    include_locator: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    passage: bool,
}

impl Serialize for RetrieveRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let query_plan =
            query_plan_from_question(&self.question).map_err(serde::ser::Error::custom)?;
        RetrieveRequestWire {
            question: self.question.clone(),
            query_plan: Some(query_plan),
            source_id: self.source_id.clone(),
            collection_filter: self.collection_filter.clone(),
            embedding_profile_id: self.embedding_profile_id.clone(),
            limit: self.limit,
            page_size: self.page_size,
            page: self.page,
            fast: self.fast,
            rerank: self.rerank,
            dense_top_k: self.dense_top_k,
            bm25_top_k: self.bm25_top_k,
            rerank_top_n: self.rerank_top_n,
            bypass_cache: self.bypass_cache,
            include_debug: self.include_debug,
            include_debug_packs: self.include_debug_packs,
            include_locator: self.include_locator,
            passage: self.passage,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RetrieveRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = RetrieveRequestWire::deserialize(deserializer)?;
        if let Some(plan) = wire.query_plan {
            plan.validate().map_err(serde::de::Error::custom)?;
        } else {
            query_plan_from_question(&wire.question).map_err(serde::de::Error::custom)?;
        }
        Ok(Self {
            question: wire.question,
            source_id: wire.source_id,
            collection_filter: wire.collection_filter,
            embedding_profile_id: wire.embedding_profile_id,
            limit: wire.limit,
            page_size: wire.page_size,
            page: wire.page,
            fast: wire.fast,
            rerank: wire.rerank,
            dense_top_k: wire.dense_top_k,
            bm25_top_k: wire.bm25_top_k,
            rerank_top_n: wire.rerank_top_n,
            bypass_cache: wire.bypass_cache,
            include_debug: wire.include_debug,
            include_debug_packs: wire.include_debug_packs,
            include_locator: wire.include_locator,
            passage: wire.passage,
        })
    }
}
