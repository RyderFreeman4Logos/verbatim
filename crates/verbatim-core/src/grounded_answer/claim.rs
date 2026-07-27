//! Claim-level verification contract (IDs, quotations, support/conflict).

use serde::{Deserialize, Serialize};

use super::error::{WorkflowError, WorkflowResult};

/// Opaque claim identifier within a draft or grounded answer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClaimId(pub String);

impl ClaimId {
    pub fn new(raw: impl Into<String>) -> WorkflowResult<Self> {
        let raw = raw.into();
        require_non_empty("claim_id", &raw)?;
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("claim_id", &self.0)
    }
}

/// How a claim relates to resolvable evidence units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSupportClass {
    /// Directly supported by at least one evidence unit quotation/ID.
    Supported,
    /// Partially supported; missing required coverage or weak binding.
    Partial,
    /// Contradicted by evidence (or by other claims against the same pack).
    Conflict,
    /// No resolvable evidence unit or quotation supports the claim.
    Unsupported,
    /// Claim is non-factual / meta (not published as a grounded claim).
    NonFactual,
}

impl ClaimSupportClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Partial => "partial",
            Self::Conflict => "conflict",
            Self::Unsupported => "unsupported",
            Self::NonFactual => "non_factual",
        }
    }

    /// Only fully supported claims may enter a published GroundedAnswer.
    pub fn is_publishable(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Status of a quotation / span check against evidence text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotationCheckStatus {
    /// Quoted span matches evidence text (exact or allowed normalization).
    Match,
    /// Span is close but not exact (residual: fuzzy policy).
    Approximate,
    /// Span not found in any bound evidence unit.
    Missing,
    /// Claim cites no quotation (ID-only).
    NotProvided,
    /// Citation id is unknown / not in the ContextPack.
    UnknownEvidence,
}

impl QuotationCheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Approximate => "approximate",
            Self::Missing => "missing",
            Self::NotProvided => "not_provided",
            Self::UnknownEvidence => "unknown_evidence",
        }
    }

    /// Strict publish path accepts only exact matches (walking skeleton).
    pub fn is_publishable(self) -> bool {
        matches!(self, Self::Match)
    }
}

/// Per-claim quotation check result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotationCheck {
    pub evidence_unit_id: String,
    pub status: QuotationCheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl QuotationCheck {
    pub fn new(
        evidence_unit_id: impl Into<String>,
        status: QuotationCheckStatus,
    ) -> WorkflowResult<Self> {
        let check = Self {
            evidence_unit_id: evidence_unit_id.into(),
            status,
            detail: None,
        };
        check.validate()?;
        Ok(check)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("evidence_unit_id", &self.evidence_unit_id)?;
        if let Some(d) = &self.detail {
            require_non_empty("quotation_check.detail", d)?;
        }
        Ok(())
    }
}

/// Unverified claim extracted from a model draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftClaim {
    pub claim_id: ClaimId,
    pub text: String,
    /// Evidence unit ids cited by the draft (may be empty or invalid).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cited_evidence_unit_ids: Vec<String>,
    /// Optional quotation the model claimed to support the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotation: Option<String>,
}

/// Field bundle for [`DraftClaim::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftClaimFields {
    pub claim_id: String,
    pub text: String,
    pub cited_evidence_unit_ids: Vec<String>,
    pub quotation: Option<String>,
}

impl DraftClaim {
    pub fn new(fields: DraftClaimFields) -> WorkflowResult<Self> {
        let claim = Self {
            claim_id: ClaimId::new(fields.claim_id)?,
            text: fields.text,
            cited_evidence_unit_ids: fields.cited_evidence_unit_ids,
            quotation: fields.quotation,
        };
        claim.validate()?;
        Ok(claim)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        self.claim_id.validate()?;
        require_non_empty("draft_claim.text", &self.text)?;
        for id in &self.cited_evidence_unit_ids {
            require_non_empty("cited_evidence_unit_id", id)?;
        }
        if let Some(q) = &self.quotation {
            require_non_empty("draft_claim.quotation", q)?;
        }
        Ok(())
    }
}

/// Per-claim verification verdict after checking IDs, quotations, and support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimVerdict {
    pub claim_id: ClaimId,
    pub support: ClaimSupportClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotation_checks: Vec<QuotationCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl ClaimVerdict {
    pub fn validate(&self) -> WorkflowResult<()> {
        self.claim_id.validate()?;
        for check in &self.quotation_checks {
            check.validate()?;
        }
        for note in &self.notes {
            require_non_empty("claim_verdict.note", note)?;
        }
        Ok(())
    }

    pub fn is_publishable(&self) -> bool {
        self.support.is_publishable()
            && self
                .quotation_checks
                .iter()
                .all(|c| c.status.is_publishable())
            && !self.quotation_checks.is_empty()
    }
}

/// Aggregate claim verification report for a draft against a ContextPack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimVerificationReport {
    /// Content hash of the ContextPack used for verification.
    pub context_pack_hash: String,
    /// Content hash of the draft / derived artifact verified.
    pub draft_hash: String,
    pub verdicts: Vec<ClaimVerdict>,
    /// True only when every factual claim is publishable.
    pub all_publishable: bool,
    /// Whether one bounded revise pass is still allowed.
    pub revise_allowed: bool,
}

/// Field bundle for [`ClaimVerificationReport::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimVerificationReportFields {
    pub context_pack_hash: String,
    pub draft_hash: String,
    pub verdicts: Vec<ClaimVerdict>,
    pub revise_allowed: bool,
}

impl ClaimVerificationReport {
    pub fn new(fields: ClaimVerificationReportFields) -> WorkflowResult<Self> {
        let all_publishable = compute_all_publishable(&fields.verdicts);
        let report = Self {
            context_pack_hash: fields.context_pack_hash,
            draft_hash: fields.draft_hash,
            verdicts: fields.verdicts,
            all_publishable,
            revise_allowed: fields.revise_allowed,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_digest("context_pack_hash", &self.context_pack_hash)?;
        require_digest("draft_hash", &self.draft_hash)?;
        if self.verdicts.is_empty() {
            return Err(WorkflowError::validation(
                "claim verification report must include at least one verdict",
            ));
        }
        for v in &self.verdicts {
            v.validate()?;
        }
        let expected = compute_all_publishable(&self.verdicts);
        if self.all_publishable != expected {
            return Err(WorkflowError::validation(
                "all_publishable flag does not match claim verdicts",
            ));
        }
        Ok(())
    }

    /// Publishable claim ids only (empty when report is not fully publishable).
    pub fn publishable_claim_ids(&self) -> Vec<&ClaimId> {
        self.verdicts
            .iter()
            .filter(|v| v.is_publishable())
            .map(|v| &v.claim_id)
            .collect()
    }
}

/// Aggregate: every **factual** claim is publishable and at least one exists.
///
/// [`ClaimSupportClass::NonFactual`] verdicts are excluded — they are never
/// published as grounded claims and must not force `all_publishable=false`.
fn compute_all_publishable(verdicts: &[ClaimVerdict]) -> bool {
    let mut saw_factual = false;
    for verdict in verdicts {
        if verdict.support == ClaimSupportClass::NonFactual {
            continue;
        }
        saw_factual = true;
        if !verdict.is_publishable() {
            return false;
        }
    }
    saw_factual
}

pub(crate) fn require_non_empty(field: &str, value: &str) -> WorkflowResult<()> {
    if value.trim().is_empty() {
        return Err(WorkflowError::validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

pub(crate) fn require_digest(field: &str, value: &str) -> WorkflowResult<()> {
    require_non_empty(field, value)?;
    if value.chars().any(|c| c.is_whitespace()) {
        return Err(WorkflowError::validation(format!(
            "{field} must not contain whitespace"
        )));
    }
    Ok(())
}
