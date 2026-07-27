//! Subquery orchestration: requests, results, parallel batches.

use serde::{Deserialize, Serialize};

use super::decomposition::{RetrieverKind, SubQuestionId};
use super::error::{ResearchError, ResearchResult};
use super::evidence::EvidenceOrigin;
use super::util::{require_digest, require_non_empty};

/// One retrieval request for a subquestion + retriever pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubqueryRequest {
    pub request_id: String,
    pub subquestion_id: SubQuestionId,
    pub retriever: RetrieverKind,
    /// Opaque query text for this subquery (not alone a cache key).
    pub query_text: String,
    /// Optional bound QueryPlan / sub-plan content hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_plan_hash: Option<String>,
    /// Max candidates this request may consider.
    pub max_candidates: u32,
}

/// Field bundle for [`SubqueryRequest::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubqueryRequestFields {
    pub request_id: String,
    pub subquestion_id: String,
    pub retriever: RetrieverKind,
    pub query_text: String,
    pub query_plan_hash: Option<String>,
    pub max_candidates: u32,
}

impl SubqueryRequest {
    pub fn new(fields: SubqueryRequestFields) -> ResearchResult<Self> {
        let req = Self {
            request_id: fields.request_id,
            subquestion_id: SubQuestionId::new(fields.subquestion_id)?,
            retriever: fields.retriever,
            query_text: fields.query_text,
            query_plan_hash: fields.query_plan_hash,
            max_candidates: fields.max_candidates,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("request_id", &self.request_id)?;
        self.subquestion_id.validate()?;
        require_non_empty("query_text", &self.query_text)?;
        if self.max_candidates == 0 {
            return Err(ResearchError::validation("max_candidates must be >= 1"));
        }
        if let Some(h) = &self.query_plan_hash {
            require_digest("query_plan_hash", h)?;
        }
        Ok(())
    }
}

/// Per-retriever provenance for a subquery result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieverProvenance {
    pub retriever: RetrieverKind,
    /// Opaque endpoint / index fingerprint.
    pub endpoint_fingerprint: String,
    /// Number of candidates considered by this retriever.
    pub candidates_considered: u32,
    /// Evidence unit ids returned (ordered by rank).
    pub evidence_unit_ids: Vec<String>,
    /// Content hash of the EvidencePack (or partial pack) produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_pack_hash: Option<String>,
}

impl RetrieverProvenance {
    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("endpoint_fingerprint", &self.endpoint_fingerprint)?;
        for id in &self.evidence_unit_ids {
            require_non_empty("evidence_unit_id", id)?;
        }
        if let Some(h) = &self.evidence_pack_hash {
            require_digest("evidence_pack_hash", h)?;
        }
        Ok(())
    }
}

/// Result of one [`SubqueryRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubqueryResult {
    pub request_id: String,
    pub subquestion_id: SubQuestionId,
    pub provenance: RetrieverProvenance,
    /// Origins of all evidence units (injection boundary markers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_origins: Vec<EvidenceOrigin>,
    /// True when the request completed successfully (empty hits still ok=true).
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SubqueryResult {
    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("request_id", &self.request_id)?;
        self.subquestion_id.validate()?;
        self.provenance.validate()?;
        for origin in &self.evidence_origins {
            origin.validate()?;
            // Evidence results must not carry workflow-instruction origins.
            if origin.origin.may_alter_workflow_control() {
                return Err(ResearchError::injection_rejected(
                    "subquery result evidence_origins must not include control origins",
                ));
            }
        }
        if let Some(d) = &self.detail {
            require_non_empty("detail", d)?;
        }
        // Provenance retriever consistency is residual when request is available;
        // result alone requires non-empty fingerprint already checked.
        Ok(())
    }
}

/// Batch of independent subqueries intended to run in parallel.
///
/// Independence is declared: all requests in a batch must have no mutual
/// subquestion dependencies that would serialize them. Adapters may further
/// filter; the contract only requires non-empty unique request ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelRetrievalBatch {
    pub batch_id: String,
    /// Round index this batch belongs to (1-based).
    pub round_index: u32,
    pub requests: Vec<SubqueryRequest>,
}

/// Field bundle for [`ParallelRetrievalBatch::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelRetrievalBatchFields {
    pub batch_id: String,
    pub round_index: u32,
    pub requests: Vec<SubqueryRequest>,
}

impl ParallelRetrievalBatch {
    pub fn new(fields: ParallelRetrievalBatchFields) -> ResearchResult<Self> {
        let batch = Self {
            batch_id: fields.batch_id,
            round_index: fields.round_index,
            requests: fields.requests,
        };
        batch.validate()?;
        Ok(batch)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("batch_id", &self.batch_id)?;
        if self.round_index == 0 {
            return Err(ResearchError::validation("round_index must be >= 1"));
        }
        if self.requests.is_empty() {
            return Err(ResearchError::validation(
                "parallel retrieval batch requires at least one request",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for req in &self.requests {
            req.validate()?;
            if !seen.insert(req.request_id.clone()) {
                return Err(ResearchError::validation(format!(
                    "duplicate request_id {} in batch",
                    req.request_id
                )));
            }
        }
        Ok(())
    }
}

/// Collected results for one parallel batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelRetrievalBatchResult {
    pub batch_id: String,
    pub round_index: u32,
    pub results: Vec<SubqueryResult>,
}

impl ParallelRetrievalBatchResult {
    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("batch_id", &self.batch_id)?;
        if self.round_index == 0 {
            return Err(ResearchError::validation("round_index must be >= 1"));
        }
        for r in &self.results {
            r.validate()?;
        }
        Ok(())
    }

    /// Sum of candidates considered across results.
    pub fn total_candidates(&self) -> u32 {
        self.results.iter().fold(0u32, |acc, r| {
            acc.saturating_add(r.provenance.candidates_considered)
        })
    }
}
