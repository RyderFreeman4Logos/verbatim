use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    is_false, AskRequest, AuditReceipt, CollectionFilterRequest, ResponseTextTaxonomy,
    RetrieveControlsResponse, RetrieveRequest, RetrieveResponse, RetrieveResultResponse,
    AUDIT_RECEIPT_VERSION,
};
use crate::wire_schemas::{
    ContextPackEnvelope, ContextPackFields, EvidencePackEnvelope, EvidencePackFields,
    QueryPlanEnvelope, QueryPlanFields,
};

const LIVE_RETRIEVE_QUERY_PLAN_ID: &str = "live-retrieve";
const LIVE_RETRIEVE_EVIDENCE_PACK_ID: &str = "live-retrieve-evidence";
const LIVE_ASK_CONTEXT_PACK_ID: &str = "live-ask-context";

pub(super) fn query_plan_from_question(
    question: &str,
    embedding_profile_id: Option<&str>,
) -> Result<QueryPlanEnvelope> {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: LIVE_RETRIEVE_QUERY_PLAN_ID.into(),
        query_text: question.to_string(),
        steps: Vec::new(),
        generation: None,
        profile_ref: embedding_profile_id.map(str::to_string),
    })
}

pub(super) fn bind_query_plan_to_question(
    question: &str,
    embedding_profile_id: Option<&str>,
    plan: Option<QueryPlanEnvelope>,
) -> Result<QueryPlanEnvelope> {
    let expected = query_plan_from_question(question, embedding_profile_id)?;
    if let Some(plan) = plan {
        plan.validate()?;
        if plan.header.identity.content_hash != expected.header.identity.content_hash {
            anyhow::bail!("query plan identity does not match the executed question");
        }
        if plan.header.profile_ref != expected.header.profile_ref {
            anyhow::bail!("query plan profile_ref does not match the executed embedding profile");
        }
    }
    Ok(expected)
}

pub(super) fn bind_evidence_pack_to_retrieve(
    query: &str,
    results: &[RetrieveResultResponse],
    embedding_profile_id: &str,
    generation: Option<&str>,
    pack: &EvidencePackEnvelope,
) -> Result<()> {
    pack.validate()?;
    let Some(expected) =
        evidence_pack_from_retrieve(query, results, embedding_profile_id, generation)?
    else {
        anyhow::bail!("evidence pack must match returned evidence");
    };
    if pack.query_plan_hash != expected.query_plan_hash {
        anyhow::bail!("evidence pack query_plan_hash does not match the adjacent query");
    }
    if pack.evidence_unit_ids != expected.evidence_unit_ids {
        anyhow::bail!("evidence pack evidence_unit_ids do not match results");
    }
    if pack.header.profile_ref != expected.header.profile_ref {
        anyhow::bail!("evidence pack profile_ref does not match the executed embedding profile");
    }
    if pack.header.generation != expected.header.generation {
        anyhow::bail!("evidence pack generation does not match the executed index generation");
    }
    Ok(())
}

pub(super) fn context_pack_from_ask_context(
    context: Option<&RetrieveResponse>,
) -> Result<Option<ContextPackEnvelope>> {
    let Some(context) = context else {
        return Ok(None);
    };
    let Some(evidence_pack) = evidence_pack_from_retrieve(
        &context.query,
        &context.results,
        &context.embedding_profile_id,
        context.generation.as_deref(),
    )?
    else {
        return Ok(None);
    };
    ContextPackEnvelope::new(ContextPackFields {
        artifact_id: LIVE_ASK_CONTEXT_PACK_ID.into(),
        evidence_pack_hash: evidence_pack.header.identity.content_hash.as_str().into(),
        selected_unit_ids: evidence_pack.evidence_unit_ids,
        model_fingerprint: None,
        generation: context.generation.clone(),
        profile_ref: Some(context.embedding_profile_id.clone()),
    })
    .map(Some)
}

pub(super) fn bind_context_pack_to_ask_context(
    context: Option<&RetrieveResponse>,
    pack: Option<&ContextPackEnvelope>,
) -> Result<()> {
    let expected = context_pack_from_ask_context(context)?;
    let Some(pack) = pack else {
        return Ok(());
    };
    pack.validate()?;
    let Some(expected) = expected else {
        if context.is_none() {
            return Ok(());
        }
        anyhow::bail!("context pack must match returned context");
    };
    if pack.evidence_pack_hash != expected.evidence_pack_hash {
        anyhow::bail!("context pack evidence_pack_hash does not match returned context");
    }
    if pack.selected_unit_ids != expected.selected_unit_ids {
        anyhow::bail!("context pack selected_unit_ids do not match context results");
    }
    if pack.header.identity != expected.header.identity {
        anyhow::bail!("context pack identity does not match returned context");
    }
    if pack.header.profile_ref != expected.header.profile_ref {
        anyhow::bail!("context pack profile_ref does not match the executed embedding profile");
    }
    if pack.header.generation != expected.header.generation {
        anyhow::bail!("context pack generation does not match the executed index generation");
    }
    Ok(())
}

pub fn generated_ask_stream_context_pack(
    context: Option<&RetrieveResponse>,
    supplied: Option<&ContextPackEnvelope>,
) -> Result<Option<ContextPackEnvelope>> {
    bind_context_pack_to_ask_context(context, supplied)?;
    context_pack_from_ask_context(context)
}

fn require_non_blank_result_evidence_ids(results: &[RetrieveResultResponse]) -> Result<()> {
    if results
        .iter()
        .any(|result| result.evidence_id.trim().is_empty())
    {
        anyhow::bail!("results[].evidence_id must not be blank");
    }
    Ok(())
}

pub(super) fn evidence_pack_from_retrieve(
    query: &str,
    results: &[RetrieveResultResponse],
    embedding_profile_id: &str,
    generation: Option<&str>,
) -> Result<Option<EvidencePackEnvelope>> {
    require_non_blank_result_evidence_ids(results)?;
    if results.is_empty() {
        return Ok(None);
    }
    let evidence_unit_ids: Vec<String> = results
        .iter()
        .map(|result| result.evidence_id.clone())
        .collect();
    let plan = query_plan_from_question(query, Some(embedding_profile_id))?;
    EvidencePackEnvelope::new(EvidencePackFields {
        artifact_id: LIVE_RETRIEVE_EVIDENCE_PACK_ID.into(),
        evidence_unit_ids,
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        generation: generation.map(str::to_string),
        profile_ref: Some(embedding_profile_id.to_string()),
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
            query_plan_from_question(&self.question, self.embedding_profile_id.as_deref())
                .map_err(serde::ser::Error::custom)?;
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
        bind_query_plan_to_question(
            &wire.question,
            wire.embedding_profile_id.as_deref(),
            wire.query_plan,
        )
        .map_err(serde::de::Error::custom)?;
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

#[derive(Debug, Serialize, Deserialize)]
struct AskRequestWire {
    question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query_plan: Option<QueryPlanEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_id: Option<String>,
    #[serde(default, skip_serializing_if = "CollectionFilterRequest::is_empty")]
    collection_filter: CollectionFilterRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedding_profile_id: Option<String>,
    #[serde(default)]
    show_retrieval: bool,
    #[serde(default)]
    context_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
}

impl Serialize for AskRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let query_plan =
            query_plan_from_question(&self.question, self.embedding_profile_id.as_deref())
                .map_err(serde::ser::Error::custom)?;
        AskRequestWire {
            question: self.question.clone(),
            query_plan: Some(query_plan),
            source_id: self.source_id.clone(),
            collection_filter: self.collection_filter.clone(),
            embedding_profile_id: self.embedding_profile_id.clone(),
            show_retrieval: self.show_retrieval,
            context_only: self.context_only,
            limit: self.limit,
            page_size: self.page_size,
            page: self.page,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AskRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AskRequestWire::deserialize(deserializer)?;
        bind_query_plan_to_question(
            &wire.question,
            wire.embedding_profile_id.as_deref(),
            wire.query_plan,
        )
        .map_err(serde::de::Error::custom)?;
        Ok(Self {
            question: wire.question,
            source_id: wire.source_id,
            collection_filter: wire.collection_filter,
            embedding_profile_id: wire.embedding_profile_id,
            show_retrieval: wire.show_retrieval,
            context_only: wire.context_only,
            limit: wire.limit,
            page_size: wire.page_size,
            page: wire.page,
        })
    }
}

impl RetrieveResponse {
    pub fn from_executed_ask_units(
        query: impl Into<String>,
        embedding_profile_id: impl Into<String>,
        generation: Option<String>,
        evidence_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let embedding_profile_id = embedding_profile_id.into();
        let results: Vec<RetrieveResultResponse> = evidence_ids
            .into_iter()
            .enumerate()
            .map(|(index, evidence_id)| RetrieveResultResponse {
                index,
                rank: index + 1,
                label: format!("E{}", index + 1),
                evidence_id: evidence_id.into(),
                text_hash: String::new(),
                source_id: String::new(),
                source_hash: String::new(),
                source_path: None,
                collections: Vec::new(),
                chunk_id: String::new(),
                kind: "text".into(),
                role: "original_text".into(),
                score: 0.0,
                locator: String::new(),
                structured_locator: None,
                provenance: None,
                derived_from: None,
                snippet: String::new(),
            })
            .collect();
        let returned_results = results.len();
        let controls = RetrieveControlsResponse {
            fast: false,
            rerank_enabled: false,
            dense_top_k: 0,
            bm25_top_k: 0,
            rrf_k: 0,
            rerank_top_n: 0,
        };
        Self {
            task_id: String::new(),
            query: query.into(),
            text_taxonomy: ResponseTextTaxonomy::retrieve_response(),
            source_id: None,
            collection_filter: None,
            embedding_profile_id: embedding_profile_id.clone(),
            generation,
            limit: returned_results,
            page_size: returned_results.max(1),
            page: 1,
            total_results: returned_results,
            returned_results,
            source_bounded: true,
            controls: controls.clone(),
            audit_receipt: AuditReceipt {
                version: AUDIT_RECEIPT_VERSION,
                embedding_profile_id,
                source_bounded: true,
                controls,
                results: Vec::new(),
            },
            timings: Vec::new(),
            results,
            debug: None,
        }
    }
}
