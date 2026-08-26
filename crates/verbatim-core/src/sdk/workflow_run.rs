//! Validated workflow-run wire request and response envelopes.

use serde::{Deserialize, Serialize};

use crate::wire_schemas::{
    ContextPackEnvelope, EvidencePackEnvelope, QueryPlanEnvelope, WorkflowEnvelope,
    WorkflowEnvelopeFields, WorkflowPhase,
};

use super::{
    error::{ClientError, ClientResult},
    ops::require_non_empty,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunRequest {
    pub workflow: WorkflowEnvelope,
    pub query_plan: QueryPlanEnvelope,
    pub evidence_pack: Option<EvidencePackEnvelope>,
    pub context_pack: Option<ContextPackEnvelope>,
    pub idempotency_key: Option<String>,
}

impl WorkflowRunRequest {
    pub fn new(
        artifact_id: impl Into<String>,
        phase: WorkflowPhase,
        query_plan: QueryPlanEnvelope,
        evidence_pack: Option<EvidencePackEnvelope>,
        context_pack: Option<ContextPackEnvelope>,
    ) -> ClientResult<Self> {
        let (query_plan_hash, evidence_pack_hash, context_pack_hash) = workflow_payload_hashes(
            &query_plan,
            evidence_pack.as_ref(),
            context_pack.as_ref(),
            None,
            None,
        )?;
        let (generation, profile_ref) =
            workflow_bound_headers(&query_plan, evidence_pack.as_ref(), context_pack.as_ref())?;
        let req = Self {
            workflow: WorkflowEnvelope::new(WorkflowEnvelopeFields {
                artifact_id: artifact_id.into(),
                phase,
                query_plan_hash,
                evidence_pack_hash,
                context_pack_hash,
                generation,
                profile_ref,
            })
            .map_err(|err| ClientError::validation(err.to_string()))?,
            query_plan,
            evidence_pack,
            context_pack,
            idempotency_key: None,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> ClientResult<()> {
        validate_workflow_payload(
            &self.workflow,
            &self.query_plan,
            self.evidence_pack.as_ref(),
            self.context_pack.as_ref(),
        )?;
        if let Some(k) = &self.idempotency_key {
            require_non_empty("idempotency_key", k)?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkflowRunRequestWire {
    workflow: WorkflowEnvelope,
    query_plan: QueryPlanEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_pack: Option<EvidencePackEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_pack: Option<ContextPackEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<String>,
}

impl Serialize for WorkflowRunRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        WorkflowRunRequestWire {
            workflow: self.workflow.clone(),
            query_plan: self.query_plan.clone(),
            evidence_pack: self.evidence_pack.clone(),
            context_pack: self.context_pack.clone(),
            idempotency_key: self.idempotency_key.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WorkflowRunRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WorkflowRunRequestWire::deserialize(deserializer)?;
        let request = Self {
            workflow: wire.workflow,
            query_plan: wire.query_plan,
            evidence_pack: wire.evidence_pack,
            context_pack: wire.context_pack,
            idempotency_key: wire.idempotency_key,
        };
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunResponse {
    pub workflow: WorkflowEnvelope,
    pub run_id: String,
    pub query_plan: QueryPlanEnvelope,
    pub evidence_pack: Option<EvidencePackEnvelope>,
    pub context_pack: Option<ContextPackEnvelope>,
}

impl WorkflowRunResponse {
    pub fn new(run_id: impl Into<String>, request: WorkflowRunRequest) -> ClientResult<Self> {
        let response = Self {
            workflow: request.workflow,
            run_id: run_id.into(),
            query_plan: request.query_plan,
            evidence_pack: request.evidence_pack,
            context_pack: request.context_pack,
        };
        response.validate()?;
        Ok(response)
    }

    pub fn validate(&self) -> ClientResult<()> {
        validate_workflow_payload(
            &self.workflow,
            &self.query_plan,
            self.evidence_pack.as_ref(),
            self.context_pack.as_ref(),
        )?;
        require_non_empty("run_id", &self.run_id)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkflowRunResponseWire {
    workflow: WorkflowEnvelope,
    run_id: String,
    query_plan: QueryPlanEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_pack: Option<EvidencePackEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_pack: Option<ContextPackEnvelope>,
}

impl Serialize for WorkflowRunResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        WorkflowRunResponseWire {
            workflow: self.workflow.clone(),
            run_id: self.run_id.clone(),
            query_plan: self.query_plan.clone(),
            evidence_pack: self.evidence_pack.clone(),
            context_pack: self.context_pack.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WorkflowRunResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WorkflowRunResponseWire::deserialize(deserializer)?;
        let response = Self {
            workflow: wire.workflow,
            run_id: wire.run_id,
            query_plan: wire.query_plan,
            evidence_pack: wire.evidence_pack,
            context_pack: wire.context_pack,
        };
        response.validate().map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

fn validate_workflow_payload(
    workflow: &WorkflowEnvelope,
    query_plan: &QueryPlanEnvelope,
    evidence_pack: Option<&EvidencePackEnvelope>,
    context_pack: Option<&ContextPackEnvelope>,
) -> ClientResult<()> {
    workflow
        .validate()
        .map_err(|err| ClientError::validation(err.to_string()))?;
    let (query_plan_hash, evidence_pack_hash, context_pack_hash) = workflow_payload_hashes(
        query_plan,
        evidence_pack,
        context_pack,
        workflow.evidence_pack_hash.as_deref(),
        workflow.context_pack_hash.as_deref(),
    )?;
    let (generation, profile_ref) =
        workflow_bound_headers(query_plan, evidence_pack, context_pack)?;
    let expected = WorkflowEnvelope::new(WorkflowEnvelopeFields {
        artifact_id: workflow.header.identity.artifact_id.clone(),
        phase: workflow.phase,
        query_plan_hash,
        evidence_pack_hash,
        context_pack_hash,
        generation,
        profile_ref,
    })
    .map_err(|err| ClientError::validation(err.to_string()))?;
    if workflow.header.identity != expected.header.identity
        || workflow.header.profile_ref != expected.header.profile_ref
        || workflow.header.generation != expected.header.generation
    {
        return Err(ClientError::validation(
            "workflow envelope identity does not match the executed or returned payload",
        ));
    }
    Ok(())
}

fn workflow_bound_headers(
    query_plan: &QueryPlanEnvelope,
    evidence_pack: Option<&EvidencePackEnvelope>,
    context_pack: Option<&ContextPackEnvelope>,
) -> ClientResult<(Option<String>, Option<String>)> {
    let profile_ref = query_plan.header.profile_ref.clone();
    for (label, header) in [
        evidence_pack.map(|pack| ("evidence pack", &pack.header)),
        context_pack.map(|pack| ("context pack", &pack.header)),
    ]
    .into_iter()
    .flatten()
    {
        if header.profile_ref != profile_ref {
            return Err(ClientError::validation(format!(
                "workflow profile_ref does not match the executed {label}"
            )));
        }
    }
    let generation = match (evidence_pack, context_pack) {
        (None, None) => query_plan.header.generation.clone(),
        (Some(evidence), Some(context)) => {
            if evidence.header.generation != context.header.generation {
                return Err(ClientError::validation(
                    "workflow generation does not match the executed context pack",
                ));
            }
            evidence.header.generation.clone()
        }
        (Some(pack), None) => pack.header.generation.clone(),
        (None, Some(pack)) => pack.header.generation.clone(),
    };
    Ok((generation, profile_ref))
}

fn workflow_payload_hashes(
    query_plan: &QueryPlanEnvelope,
    evidence_pack: Option<&EvidencePackEnvelope>,
    context_pack: Option<&ContextPackEnvelope>,
    legacy_evidence_pack_hash: Option<&str>,
    legacy_context_pack_hash: Option<&str>,
) -> ClientResult<(String, Option<String>, Option<String>)> {
    query_plan
        .validate()
        .map_err(|err| ClientError::validation(err.to_string()))?;
    let query_plan_hash = query_plan.header.identity.content_hash.as_str().to_string();

    let evidence_pack_hash = if let Some(pack) = evidence_pack {
        pack.validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        if pack.query_plan_hash != query_plan_hash {
            return Err(ClientError::validation(
                "evidence pack query_plan_hash does not match workflow query plan",
            ));
        }
        Some(pack.header.identity.content_hash.as_str().to_string())
    } else {
        None
    };

    if let Some(pack) = context_pack {
        pack.validate()
            .map_err(|err| ClientError::validation(err.to_string()))?;
        if let Some(evidence_pack_hash) = evidence_pack_hash.as_deref() {
            if pack.evidence_pack_hash.as_str() != evidence_pack_hash {
                return Err(ClientError::validation(
                    "context pack evidence_pack_hash does not match workflow evidence pack",
                ));
            }
        }
    }

    let evidence_pack_hash = evidence_pack_hash
        .or_else(|| context_pack.map(|pack| pack.evidence_pack_hash.clone()))
        .or_else(|| legacy_evidence_pack_hash.map(str::to_owned));
    let context_pack_hash = context_pack
        .map(|pack| pack.header.identity.content_hash.as_str().to_string())
        .or_else(|| legacy_context_pack_hash.map(str::to_owned));
    Ok((query_plan_hash, evidence_pack_hash, context_pack_hash))
}
