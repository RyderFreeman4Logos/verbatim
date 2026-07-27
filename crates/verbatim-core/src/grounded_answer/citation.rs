//! Deterministic citation rendering contract for publishable claims.

use serde::{Deserialize, Serialize};

use super::claim::{require_non_empty, ClaimId};
use super::error::{WorkflowError, WorkflowResult};

/// Citation label style for deterministic rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationStyle {
    /// Bracketed sequential labels: `[E1]`, `[E2]`, …
    BracketedSequential,
    /// Evidence-unit id as the label (opaque).
    EvidenceUnitId,
}

impl CitationStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BracketedSequential => "bracketed_sequential",
            Self::EvidenceUnitId => "evidence_unit_id",
        }
    }
}

/// One rendered citation binding a claim to an evidence unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedCitation {
    pub claim_id: ClaimId,
    pub evidence_unit_id: String,
    /// Deterministic label inserted into the answer text.
    pub label: String,
    /// Zero-based order among rendered citations for this answer.
    pub ordinal: u32,
}

impl RenderedCitation {
    pub fn validate(&self) -> WorkflowResult<()> {
        self.claim_id.validate()?;
        require_non_empty("evidence_unit_id", &self.evidence_unit_id)?;
        require_non_empty("citation.label", &self.label)?;
        Ok(())
    }
}

/// Full set of citations for a grounded answer body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedCitationSet {
    pub style: CitationStyle,
    pub citations: Vec<RenderedCitation>,
    /// Answer body with deterministic citation labels applied.
    pub rendered_text: String,
}

impl RenderedCitationSet {
    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("rendered_text", &self.rendered_text)?;
        for (idx, c) in self.citations.iter().enumerate() {
            c.validate()?;
            if c.ordinal as usize != idx {
                return Err(WorkflowError::validation(format!(
                    "citation ordinal {got} must equal position {idx}",
                    got = c.ordinal
                )));
            }
        }
        Ok(())
    }
}

/// Request to render citations for publishable claims only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationRenderRequest {
    pub style: CitationStyle,
    /// Draft or verified answer body without final labels (or with placeholders).
    pub body: String,
    /// Publishable claims with ordered evidence unit ids (first id is primary).
    pub claim_bindings: Vec<ClaimCitationBinding>,
}

/// Field bundle for [`CitationRenderRequest::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationRenderRequestFields {
    pub style: CitationStyle,
    pub body: String,
    pub claim_bindings: Vec<ClaimCitationBinding>,
}

/// Binding of one publishable claim to ordered evidence unit ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCitationBinding {
    pub claim_id: ClaimId,
    pub evidence_unit_ids: Vec<String>,
}

impl ClaimCitationBinding {
    pub fn new(
        claim_id: impl Into<String>,
        evidence_unit_ids: Vec<String>,
    ) -> WorkflowResult<Self> {
        let binding = Self {
            claim_id: ClaimId::new(claim_id)?,
            evidence_unit_ids,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        self.claim_id.validate()?;
        if self.evidence_unit_ids.is_empty() {
            return Err(WorkflowError::validation(
                "claim citation binding requires at least one evidence_unit_id",
            ));
        }
        for id in &self.evidence_unit_ids {
            require_non_empty("evidence_unit_id", id)?;
        }
        Ok(())
    }
}

impl CitationRenderRequest {
    pub fn new(fields: CitationRenderRequestFields) -> WorkflowResult<Self> {
        let req = Self {
            style: fields.style,
            body: fields.body,
            claim_bindings: fields.claim_bindings,
        };
        req.validate()?;
        Ok(req)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("citation body", &self.body)?;
        if self.claim_bindings.is_empty() {
            return Err(WorkflowError::validation(
                "citation render request requires at least one claim binding",
            ));
        }
        for b in &self.claim_bindings {
            b.validate()?;
        }
        Ok(())
    }
}

/// Deterministically render citations for a request.
///
/// Pure function: same inputs always produce the same labels and ordinals.
/// Does not invent evidence; fails closed when bindings are empty/invalid.
pub fn render_citations(request: &CitationRenderRequest) -> WorkflowResult<RenderedCitationSet> {
    request.validate()?;

    let mut citations = Vec::new();
    let mut ordinal: u32 = 0;
    // Stable unique evidence order for sequential labels.
    let mut sequential_labels: Vec<(String, String)> = Vec::new();

    for binding in &request.claim_bindings {
        let primary = &binding.evidence_unit_ids[0];
        let label = match request.style {
            CitationStyle::BracketedSequential => {
                let seq = match sequential_labels.iter().position(|(eu, _)| eu == primary) {
                    Some(idx) => idx + 1,
                    None => {
                        let next = sequential_labels.len() + 1;
                        sequential_labels.push((primary.clone(), format!("[E{next}]")));
                        next
                    }
                };
                format!("[E{seq}]")
            }
            CitationStyle::EvidenceUnitId => format!("[{primary}]"),
        };

        citations.push(RenderedCitation {
            claim_id: binding.claim_id.clone(),
            evidence_unit_id: primary.clone(),
            label: label.clone(),
            ordinal,
        });
        ordinal = ordinal.saturating_add(1);
    }

    // Append a deterministic citation footer; body text is preserved as-is.
    // Full in-body marker substitution is residual.
    let mut rendered_text = request.body.trim_end().to_string();
    if !rendered_text.is_empty() {
        rendered_text.push_str("\n\n");
    }
    rendered_text.push_str("Citations:");
    for c in &citations {
        rendered_text.push_str(&format!(
            "\n- {} {} -> {}",
            c.label,
            c.claim_id.as_str(),
            c.evidence_unit_id
        ));
    }

    let set = RenderedCitationSet {
        style: request.style,
        citations,
        rendered_text,
    };
    set.validate()?;
    Ok(set)
}
