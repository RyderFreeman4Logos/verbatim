//! AnswerPlan, draft, and published GroundedAnswer artifacts.

use serde::{Deserialize, Serialize};

use super::citation::RenderedCitationSet;
use super::claim::{require_digest, require_non_empty, ClaimId, DraftClaim};
use super::error::{WorkflowError, WorkflowResult};

/// Plan describing how a draft answer should be produced from a ContextPack.
///
/// Walking skeleton: instruction + selected unit ids + optional budget hints.
/// Full IR (templates, delimiter policy, tool calls) is residual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerPlan {
    pub plan_id: String,
    /// Content hash of the ContextPack this plan is bound to.
    pub context_pack_hash: String,
    /// Generation instruction (never alone a cache key).
    pub instruction: String,
    /// Evidence unit ids the draft is allowed to cite (ContextPack subset).
    pub allowed_evidence_unit_ids: Vec<String>,
    /// Maximum claims the draft may emit (fail closed if exceeded).
    pub max_claims: u32,
    /// Opaque model/deployment fingerprint for the planned generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
}

/// Field bundle for [`AnswerPlan::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerPlanFields {
    pub plan_id: String,
    pub context_pack_hash: String,
    pub instruction: String,
    pub allowed_evidence_unit_ids: Vec<String>,
    pub max_claims: u32,
    pub model_fingerprint: Option<String>,
}

impl AnswerPlan {
    pub fn new(fields: AnswerPlanFields) -> WorkflowResult<Self> {
        let plan = Self {
            plan_id: fields.plan_id,
            context_pack_hash: fields.context_pack_hash,
            instruction: fields.instruction,
            allowed_evidence_unit_ids: fields.allowed_evidence_unit_ids,
            max_claims: fields.max_claims,
            model_fingerprint: fields.model_fingerprint,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("plan_id", &self.plan_id)?;
        require_digest("context_pack_hash", &self.context_pack_hash)?;
        require_non_empty("instruction", &self.instruction)?;
        if self.allowed_evidence_unit_ids.is_empty() {
            return Err(WorkflowError::validation(
                "answer plan requires at least one allowed_evidence_unit_id",
            ));
        }
        for id in &self.allowed_evidence_unit_ids {
            require_non_empty("allowed_evidence_unit_id", id)?;
        }
        if self.max_claims == 0 {
            return Err(WorkflowError::validation("max_claims must be >= 1"));
        }
        if let Some(m) = &self.model_fingerprint {
            require_non_empty("model_fingerprint", m)?;
        }
        Ok(())
    }
}

/// Unverified draft answer produced under an AnswerPlan.
///
/// Never publishable on its own; must pass claim verification and citation
/// rendering first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerDraft {
    pub draft_id: String,
    /// Content hash of the AnswerPlan used to produce this draft.
    pub answer_plan_hash: String,
    /// Content hash of the ContextPack (must match the plan).
    pub context_pack_hash: String,
    pub model_fingerprint: String,
    pub body: String,
    pub claims: Vec<DraftClaim>,
}

impl AnswerDraft {
    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("draft_id", &self.draft_id)?;
        require_digest("answer_plan_hash", &self.answer_plan_hash)?;
        require_digest("context_pack_hash", &self.context_pack_hash)?;
        require_non_empty("model_fingerprint", &self.model_fingerprint)?;
        require_non_empty("draft.body", &self.body)?;
        if self.claims.is_empty() {
            return Err(WorkflowError::validation(
                "answer draft requires at least one claim (or must abstain)",
            ));
        }
        for claim in &self.claims {
            claim.validate()?;
        }
        Ok(())
    }

    /// True when every claim cites only allowed evidence unit ids.
    pub fn cites_only_allowed(&self, plan: &AnswerPlan) -> bool {
        self.claims
            .iter()
            .all(|claim| evidence_ids_only_allowed(plan, &claim.cited_evidence_unit_ids))
    }
}

/// Shared allowlist predicate for drafts and constrained evidence selection.
pub(super) fn evidence_ids_only_allowed(plan: &AnswerPlan, ids: &[String]) -> bool {
    let allowed: std::collections::BTreeSet<&str> = plan
        .allowed_evidence_unit_ids
        .iter()
        .map(String::as_str)
        .collect();
    ids.iter().all(|id| allowed.contains(id.as_str()))
}

/// One publishable factual claim with resolvable evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundedClaim {
    pub claim_id: ClaimId,
    pub text: String,
    pub evidence_unit_ids: Vec<String>,
}

/// Field bundle for [`GroundedClaim::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundedClaimFields {
    pub claim_id: String,
    pub text: String,
    pub evidence_unit_ids: Vec<String>,
}

impl GroundedClaim {
    pub fn new(fields: GroundedClaimFields) -> WorkflowResult<Self> {
        let claim = Self {
            claim_id: ClaimId::new(fields.claim_id)?,
            text: fields.text,
            evidence_unit_ids: fields.evidence_unit_ids,
        };
        claim.validate()?;
        Ok(claim)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        self.claim_id.validate()?;
        require_non_empty("grounded_claim.text", &self.text)?;
        if self.evidence_unit_ids.is_empty() {
            return Err(WorkflowError::validation(
                "grounded claim requires at least one evidence_unit_id",
            ));
        }
        for id in &self.evidence_unit_ids {
            require_non_empty("evidence_unit_id", id)?;
        }
        Ok(())
    }
}

/// Published grounded answer: only verified supported claims + citations.
///
/// Construction fails closed if claims or citations are empty or inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundedAnswer {
    pub answer_id: String,
    pub context_pack_hash: String,
    pub query_plan_hash: String,
    pub model_fingerprint: String,
    pub claims: Vec<GroundedClaim>,
    pub citations: RenderedCitationSet,
    /// Final user-visible text (must match citations.rendered_text).
    pub text: String,
}

/// Field bundle for [`GroundedAnswer::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundedAnswerFields {
    pub answer_id: String,
    pub context_pack_hash: String,
    pub query_plan_hash: String,
    pub model_fingerprint: String,
    pub claims: Vec<GroundedClaim>,
    pub citations: RenderedCitationSet,
}

impl GroundedAnswer {
    pub fn new(fields: GroundedAnswerFields) -> WorkflowResult<Self> {
        let answer = Self {
            answer_id: fields.answer_id,
            context_pack_hash: fields.context_pack_hash,
            query_plan_hash: fields.query_plan_hash,
            model_fingerprint: fields.model_fingerprint,
            claims: fields.claims,
            text: fields.citations.rendered_text.clone(),
            citations: fields.citations,
        };
        answer.validate()?;
        Ok(answer)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("answer_id", &self.answer_id)?;
        require_digest("context_pack_hash", &self.context_pack_hash)?;
        require_digest("query_plan_hash", &self.query_plan_hash)?;
        require_non_empty("model_fingerprint", &self.model_fingerprint)?;
        if self.claims.is_empty() {
            return Err(WorkflowError::validation(
                "grounded answer requires at least one verified claim",
            ));
        }
        for claim in &self.claims {
            claim.validate()?;
        }
        self.citations.validate()?;
        if self.text != self.citations.rendered_text {
            return Err(WorkflowError::validation(
                "grounded answer text must equal citations.rendered_text",
            ));
        }
        // Every citation claim_id must map to a grounded claim, and the cited
        // evidence_unit_id must be among that claim's evidence_unit_ids.
        for c in &self.citations.citations {
            let Some(claim) = self.claims.iter().find(|gc| gc.claim_id == c.claim_id) else {
                return Err(WorkflowError::validation(format!(
                    "citation claim_id {} has no grounded claim",
                    c.claim_id.as_str()
                )));
            };
            if !claim
                .evidence_unit_ids
                .iter()
                .any(|id| id == &c.evidence_unit_id)
            {
                return Err(WorkflowError::validation(format!(
                    "citation evidence_unit_id {} is not bound to claim {}",
                    c.evidence_unit_id,
                    c.claim_id.as_str()
                )));
            }
        }
        // Every grounded claim must have at least one citation.
        for claim in &self.claims {
            if !self
                .citations
                .citations
                .iter()
                .any(|c| c.claim_id == claim.claim_id)
            {
                return Err(WorkflowError::validation(format!(
                    "grounded claim {} has no citation",
                    claim.claim_id.as_str()
                )));
            }
        }
        Ok(())
    }
}
