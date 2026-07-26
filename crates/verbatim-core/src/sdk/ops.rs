//! Typed request/response envelopes for public SDK operations.
//!
//! These are walking-skeleton DTOs that reference wire identity hashes from
//! API-002 (`wire_schemas`) and snapshot pagination from API-003 (`pagination`).
//! They deliberately avoid Store/SQL/filesystem types.

use serde::{Deserialize, Serialize};

use crate::pagination::{SnapshotPageRequest, SnapshotPageResponse};
use crate::wire_schemas::{
    ContextPackEnvelope, DerivedArtifactEnvelope, EvidencePackEnvelope, QueryPlanEnvelope,
    WorkflowEnvelope,
};

use super::error::{ClientError, ClientResult};

// ---------------------------------------------------------------------------
// Source / upload
// ---------------------------------------------------------------------------

/// Register or upload a source path/locator without exposing local Store types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUploadRequest {
    pub locator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl SourceUploadRequest {
    pub fn new(locator: impl Into<String>) -> ClientResult<Self> {
        let req = Self {
            locator: locator.into(),
            collection: None,
            content_hash: None,
            idempotency_key: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("locator", &self.locator)?;
        if let Some(c) = &self.collection {
            require_non_empty("collection", c)?;
        }
        if let Some(h) = &self.content_hash {
            require_digest("content_hash", h)?;
        }
        if let Some(k) = &self.idempotency_key {
            require_non_empty("idempotency_key", k)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUploadResponse {
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub accepted: bool,
}

impl SourceUploadResponse {
    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("source_id", &self.source_id)?;
        if let Some(h) = &self.content_hash {
            require_digest("content_hash", h)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Search / retrieve / resolve
// ---------------------------------------------------------------------------

/// Snapshot-bound search request wrapping a query plan hash + page controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query_plan_hash: String,
    pub page: SnapshotPageRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
}

impl SearchRequest {
    pub fn new(
        query_plan_hash: impl Into<String>,
        page: SnapshotPageRequest,
    ) -> ClientResult<Self> {
        let req = Self {
            query_plan_hash: query_plan_hash.into(),
            page,
            profile_ref: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_digest("query_plan_hash", &self.query_plan_hash)?;
        self.page
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        if self.page.query_plan_hash != self.query_plan_hash {
            return Err(ClientError::validation(
                "search request query_plan_hash must match page.query_plan_hash",
            ));
        }
        if let Some(p) = &self.profile_ref {
            require_non_empty("profile_ref", p)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub evidence_unit_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<String>,
}

impl SearchResultItem {
    pub fn new(evidence_unit_id: impl Into<String>) -> ClientResult<Self> {
        let item = Self {
            evidence_unit_id: evidence_unit_id.into(),
            score: None,
        };
        item.validate()?;
        Ok(item)
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("evidence_unit_id", &self.evidence_unit_id)?;
        if let Some(s) = &self.score {
            require_non_empty("score", s)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub page: SnapshotPageResponse<SearchResultItem>,
}

impl SearchResponse {
    pub fn validate(&self) -> ClientResult<()> {
        for item in &self.page.items {
            item.validate()?;
        }
        Ok(())
    }
}

/// Retrieve an EvidencePack for a QueryPlan (R boundary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveRequest {
    pub query_plan: QueryPlanEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
}

impl RetrieveRequest {
    pub fn new(query_plan: QueryPlanEnvelope) -> ClientResult<Self> {
        let req = Self {
            query_plan,
            profile_ref: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        self.query_plan
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        if let Some(p) = &self.profile_ref {
            require_non_empty("profile_ref", p)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveResponse {
    pub evidence_pack: EvidencePackEnvelope,
}

impl RetrieveResponse {
    pub fn validate(&self) -> ClientResult<()> {
        self.evidence_pack
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))
    }
}

/// Resolve evidence unit / artifact identity to a stable locator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub artifact_ref: ArtifactRef,
}

impl ResolveRequest {
    pub fn new(artifact_ref: ArtifactRef) -> ClientResult<Self> {
        artifact_ref.validate()?;
        Ok(Self { artifact_ref })
    }

    pub fn validate(&self) -> ClientResult<()> {
        self.artifact_ref.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveResponse {
    pub artifact_ref: ArtifactRef,
    pub resolved_locator: String,
}

impl ResolveResponse {
    pub fn validate(&self) -> ClientResult<()> {
        self.artifact_ref.validate()?;
        require_non_empty("resolved_locator", &self.resolved_locator)
    }
}

// ---------------------------------------------------------------------------
// Evidence / context / generate / verify
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceGetRequest {
    pub evidence_pack_hash: String,
}

impl EvidenceGetRequest {
    pub fn new(evidence_pack_hash: impl Into<String>) -> ClientResult<Self> {
        let req = Self {
            evidence_pack_hash: evidence_pack_hash.into(),
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_digest("evidence_pack_hash", &self.evidence_pack_hash)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceGetResponse {
    pub evidence_pack: EvidencePackEnvelope,
}

impl EvidenceGetResponse {
    pub fn validate(&self) -> ClientResult<()> {
        self.evidence_pack
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBuildRequest {
    pub evidence_pack: EvidencePackEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
}

impl ContextBuildRequest {
    pub fn new(evidence_pack: EvidencePackEnvelope) -> ClientResult<Self> {
        let req = Self {
            evidence_pack,
            model_fingerprint: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        self.evidence_pack
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        if let Some(m) = &self.model_fingerprint {
            require_non_empty("model_fingerprint", m)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBuildResponse {
    pub context_pack: ContextPackEnvelope,
}

impl ContextBuildResponse {
    pub fn validate(&self) -> ClientResult<()> {
        self.context_pack
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub context_pack: ContextPackEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

impl GenerateRequest {
    pub fn new(context_pack: ContextPackEnvelope) -> ClientResult<Self> {
        let req = Self {
            context_pack,
            instruction: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        self.context_pack
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        if let Some(i) = &self.instruction {
            require_non_empty("instruction", i)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub artifact: DerivedArtifactEnvelope,
}

impl GenerateResponse {
    pub fn validate(&self) -> ClientResult<()> {
        self.artifact
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub artifact: DerivedArtifactEnvelope,
    pub evidence_pack_hash: String,
}

impl VerifyRequest {
    pub fn new(
        artifact: DerivedArtifactEnvelope,
        evidence_pack_hash: impl Into<String>,
    ) -> ClientResult<Self> {
        let req = Self {
            artifact,
            evidence_pack_hash: evidence_pack_hash.into(),
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        self.artifact
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        require_digest("evidence_pack_hash", &self.evidence_pack_hash)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Workflow / task / artifact
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRunRequest {
    pub workflow: WorkflowEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl WorkflowRunRequest {
    pub fn new(workflow: WorkflowEnvelope) -> ClientResult<Self> {
        let req = Self {
            workflow,
            idempotency_key: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        self.workflow
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        if let Some(k) = &self.idempotency_key {
            require_non_empty("idempotency_key", k)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRunResponse {
    pub workflow: WorkflowEnvelope,
    pub run_id: String,
}

impl WorkflowRunResponse {
    pub fn validate(&self) -> ClientResult<()> {
        self.workflow
            .validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        require_non_empty("run_id", &self.run_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSubmitRequest {
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl TaskSubmitRequest {
    pub fn new(operation: impl Into<String>) -> ClientResult<Self> {
        let req = Self {
            operation: operation.into(),
            payload_hash: None,
            idempotency_key: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("operation", &self.operation)?;
        if let Some(h) = &self.payload_hash {
            require_digest("payload_hash", h)?;
        }
        if let Some(k) = &self.idempotency_key {
            require_non_empty("idempotency_key", k)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSubmitResponse {
    pub task_id: String,
}

impl TaskSubmitResponse {
    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("task_id", &self.task_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGetRequest {
    pub task_id: String,
}

impl TaskGetRequest {
    pub fn new(task_id: impl Into<String>) -> ClientResult<Self> {
        let req = Self {
            task_id: task_id.into(),
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("task_id", &self.task_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGetResponse {
    pub task_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_hash: Option<String>,
}

impl TaskGetResponse {
    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("task_id", &self.task_id)?;
        require_non_empty("status", &self.status)?;
        if let Some(h) = &self.result_hash {
            require_digest("result_hash", h)?;
        }
        Ok(())
    }
}

/// Stable public artifact reference (kind + id + optional content hash).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

impl ArtifactRef {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> ClientResult<Self> {
        let r = Self {
            kind: kind.into(),
            id: id.into(),
            content_hash: None,
        };
        r.validate()?;
        Ok(r)
    }

    pub fn with_content_hash(mut self, hash: impl Into<String>) -> ClientResult<Self> {
        let hash = hash.into();
        require_digest("content_hash", &hash)?;
        self.content_hash = Some(hash);
        Ok(self)
    }

    pub fn validate(&self) -> ClientResult<()> {
        require_non_empty("kind", &self.kind)?;
        require_non_empty("id", &self.id)?;
        if let Some(h) = &self.content_hash {
            require_digest("content_hash", h)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactGetRequest {
    pub artifact_ref: ArtifactRef,
}

impl ArtifactGetRequest {
    pub fn new(artifact_ref: ArtifactRef) -> ClientResult<Self> {
        artifact_ref.validate()?;
        Ok(Self { artifact_ref })
    }

    pub fn validate(&self) -> ClientResult<()> {
        self.artifact_ref.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactGetResponse {
    pub artifact_ref: ArtifactRef,
    /// Opaque content digest of the returned artifact body when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
}

impl ArtifactGetResponse {
    pub fn validate(&self) -> ClientResult<()> {
        self.artifact_ref.validate()?;
        if let Some(h) = &self.body_hash {
            require_digest("body_hash", h)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_non_empty(field: &str, value: &str) -> ClientResult<()> {
    if value.trim().is_empty() {
        return Err(ClientError::validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn require_digest(field: &str, value: &str) -> ClientResult<()> {
    require_non_empty(field, value)?;
    if value.chars().any(|c| c.is_whitespace()) {
        return Err(ClientError::validation(format!(
            "{field} must not contain whitespace"
        )));
    }
    Ok(())
}
