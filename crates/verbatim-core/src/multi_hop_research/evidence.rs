//! Prompt-injection guard: typed evidence origin markers.

use serde::{Deserialize, Serialize};

use super::error::{ResearchError, ResearchResult};
use super::util::require_non_empty;

/// Origin class for text that may enter a research workflow context.
///
/// Workflow instructions and tool permissions are **never** derived from
/// [`Self::EvidenceText`] or [`Self::DocumentBody`]. Adapters must keep
/// instruction channels separate from evidence channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOriginKind {
    /// Trusted workflow / system instruction channel.
    WorkflowInstruction,
    /// Operator / policy configuration (not document body).
    PolicyConfig,
    /// Retrieved evidence unit text (untrusted for control flow).
    EvidenceText,
    /// Raw document body (untrusted for control flow).
    DocumentBody,
    /// Model-produced decomposition / intermediate text (untrusted for control).
    ModelIntermediate,
}

impl EvidenceOriginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkflowInstruction => "workflow_instruction",
            Self::PolicyConfig => "policy_config",
            Self::EvidenceText => "evidence_text",
            Self::DocumentBody => "document_body",
            Self::ModelIntermediate => "model_intermediate",
        }
    }

    /// Whether text of this origin may alter workflow instructions or tool
    /// permissions. Only trusted channels may.
    pub fn may_alter_workflow_control(self) -> bool {
        matches!(self, Self::WorkflowInstruction | Self::PolicyConfig)
    }
}

/// Typed marker binding a content blob to its origin class.
///
/// Walking skeleton: content is referenced by opaque id + origin; adapters
/// supply the actual bytes. The marker itself is the injection boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceOrigin {
    pub origin: EvidenceOriginKind,
    /// Opaque content handle / evidence unit id / instruction id.
    pub content_ref: String,
    /// Optional human note (never treated as instruction when origin is untrusted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Field bundle for [`EvidenceOrigin::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceOriginFields {
    pub origin: EvidenceOriginKind,
    pub content_ref: String,
    pub note: Option<String>,
}

impl EvidenceOrigin {
    pub fn new(fields: EvidenceOriginFields) -> ResearchResult<Self> {
        let marker = Self {
            origin: fields.origin,
            content_ref: fields.content_ref,
            note: fields.note,
        };
        marker.validate()?;
        Ok(marker)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("content_ref", &self.content_ref)?;
        if let Some(n) = &self.note {
            require_non_empty("note", n)?;
        }
        Ok(())
    }

    /// Reject attempts to use untrusted origin as a control channel.
    pub fn assert_may_alter_workflow_control(&self) -> ResearchResult<()> {
        self.validate()?;
        if !self.origin.may_alter_workflow_control() {
            return Err(ResearchError::injection_rejected(format!(
                "origin {} content_ref={} cannot alter workflow control",
                self.origin.as_str(),
                self.content_ref
            )));
        }
        Ok(())
    }
}

/// Guard helper: ensure a candidate instruction text is paired with a trusted origin.
pub fn guard_instruction_origin(origin: &EvidenceOrigin) -> ResearchResult<()> {
    origin.assert_may_alter_workflow_control()
}
