//! Minimal QueryPlan / EvidencePack / ContextPack / Workflow envelopes.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::common::WireArtifactKind;
use super::identity::{CanonicalIdentity, WireEnvelopeHeader, WireEnvelopeHeaderFields};
use super::ser::{encode_wire_document, wire_content_hash};

/// QueryPlan wire envelope. Retrieval controls are part of the plan identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryPlanEnvelope {
    pub header: WireEnvelopeHeader,
    /// Human/debug query text (not alone sufficient as a cache key).
    pub query_text: String,
    /// Ordered opaque retrieval step labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_filter: Option<QueryPlanCollectionFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dense_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bm25_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_top_n: Option<usize>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bypass_cache: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_debug: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_debug_packs: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_locator: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub passage: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryPlanCollectionFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collection_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_fresh: bool,
}

impl QueryPlanCollectionFilter {
    pub fn is_empty(&self) -> bool {
        self.collection_ids.is_empty() && self.names.is_empty() && !self.require_fresh
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct QueryPlanControls {
    pub source_id: Option<String>,
    pub collection_filter: Option<QueryPlanCollectionFilter>,
    pub limit: Option<usize>,
    pub page_size: Option<usize>,
    pub page: Option<usize>,
    pub fast: bool,
    pub rerank: Option<bool>,
    pub dense_top_k: Option<usize>,
    pub bm25_top_k: Option<usize>,
    pub rerank_top_n: Option<usize>,
    pub bypass_cache: bool,
    pub include_debug: bool,
    pub include_debug_packs: bool,
    pub include_locator: bool,
    pub passage: bool,
}

/// Field bundle for [`QueryPlanEnvelope::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryPlanFields {
    pub artifact_id: String,
    pub query_text: String,
    pub steps: Vec<String>,
    pub generation: Option<String>,
    pub profile_ref: Option<String>,
}

impl QueryPlanEnvelope {
    pub fn new(fields: QueryPlanFields) -> Result<Self> {
        Self::new_with_controls(fields, QueryPlanControls::default())
    }

    pub fn new_with_controls(fields: QueryPlanFields, controls: QueryPlanControls) -> Result<Self> {
        if fields.query_text.trim().is_empty() {
            bail!("query_text must not be empty");
        }
        for step in &fields.steps {
            if step.trim().is_empty() {
                bail!("query plan steps must not contain empty entries");
            }
        }
        let body = QueryPlanBody {
            query_text: fields.query_text.clone(),
            steps: fields.steps.clone(),
            source_id: controls.source_id.clone(),
            collection_filter: controls.collection_filter.clone(),
            limit: controls.limit,
            page_size: controls.page_size,
            page: controls.page,
            fast: controls.fast,
            rerank: controls.rerank,
            dense_top_k: controls.dense_top_k,
            bm25_top_k: controls.bm25_top_k,
            rerank_top_n: controls.rerank_top_n,
            bypass_cache: controls.bypass_cache,
            include_debug: controls.include_debug,
            include_debug_packs: controls.include_debug_packs,
            include_locator: controls.include_locator,
            passage: controls.passage,
        };
        let body_bytes = encode_wire_document(&body)?;
        let identity = CanonicalIdentity::from_body(
            WireArtifactKind::QueryPlan,
            super::WIRE_SCHEMA_VERSION,
            fields.artifact_id,
            &body_bytes,
        )?;
        let header = WireEnvelopeHeader::new(WireEnvelopeHeaderFields {
            identity,
            generation: fields.generation,
            profile_ref: fields.profile_ref,
        })?;
        Ok(Self {
            header,
            query_text: fields.query_text,
            steps: fields.steps,
            source_id: controls.source_id,
            collection_filter: controls.collection_filter,
            limit: controls.limit,
            page_size: controls.page_size,
            page: controls.page,
            fast: controls.fast,
            rerank: controls.rerank,
            dense_top_k: controls.dense_top_k,
            bm25_top_k: controls.bm25_top_k,
            rerank_top_n: controls.rerank_top_n,
            bypass_cache: controls.bypass_cache,
            include_debug: controls.include_debug,
            include_debug_packs: controls.include_debug_packs,
            include_locator: controls.include_locator,
            passage: controls.passage,
        })
    }

    pub fn validate(&self) -> Result<()> {
        self.header.validate()?;
        if self.header.identity.kind != WireArtifactKind::QueryPlan {
            bail!(
                "query plan envelope identity kind must be query_plan, got {}",
                self.header.identity.kind
            );
        }
        if self.query_text.trim().is_empty() {
            bail!("query_text must not be empty");
        }
        for step in &self.steps {
            if step.trim().is_empty() {
                bail!("query plan steps must not contain empty entries");
            }
        }
        verify_body_hash(self.header.identity.content_hash.as_str(), &self.body())
    }

    fn body(&self) -> QueryPlanBody {
        QueryPlanBody {
            query_text: self.query_text.clone(),
            steps: self.steps.clone(),
            source_id: self.source_id.clone(),
            collection_filter: self.collection_filter.clone(),
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
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryPlanBody {
    query_text: String,
    steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    collection_filter: Option<QueryPlanCollectionFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    #[serde(default, skip_serializing_if = "is_false")]
    fast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rerank: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dense_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bm25_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rerank_top_n: Option<usize>,
    #[serde(default, skip_serializing_if = "is_false")]
    bypass_cache: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    include_debug: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    include_debug_packs: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    include_locator: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    passage: bool,
}

/// Minimal EvidencePack wire envelope.
///
/// Distinguishes direct evidence units from expanded/generated material via
/// explicit unit ids only in this skeleton; richer locators are residual.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidencePackEnvelope {
    pub header: WireEnvelopeHeader,
    /// Ordered direct evidence unit ids (non-empty pack).
    pub evidence_unit_ids: Vec<String>,
    /// QueryPlan identity digest this pack was retrieved for.
    pub query_plan_hash: String,
}

/// Field bundle for [`EvidencePackEnvelope::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EvidencePackFields {
    pub artifact_id: String,
    pub evidence_unit_ids: Vec<String>,
    pub query_plan_hash: String,
    pub generation: Option<String>,
    pub profile_ref: Option<String>,
}

impl EvidencePackEnvelope {
    pub fn new(fields: EvidencePackFields) -> Result<Self> {
        if fields.evidence_unit_ids.is_empty() {
            bail!("evidence_unit_ids must not be empty");
        }
        for id in &fields.evidence_unit_ids {
            if id.trim().is_empty() {
                bail!("evidence_unit_ids must not contain empty entries");
            }
        }
        if fields.query_plan_hash.trim().is_empty() {
            bail!("query_plan_hash must not be empty");
        }
        if fields.query_plan_hash.chars().any(|c| c.is_whitespace()) {
            bail!("query_plan_hash must not contain whitespace");
        }
        let body = EvidencePackBody {
            evidence_unit_ids: fields.evidence_unit_ids.clone(),
            query_plan_hash: fields.query_plan_hash.clone(),
        };
        let body_bytes = encode_wire_document(&body)?;
        let identity = CanonicalIdentity::from_body(
            WireArtifactKind::EvidencePack,
            super::WIRE_SCHEMA_VERSION,
            fields.artifact_id,
            &body_bytes,
        )?;
        let header = WireEnvelopeHeader::new(WireEnvelopeHeaderFields {
            identity,
            generation: fields.generation,
            profile_ref: fields.profile_ref,
        })?;
        Ok(Self {
            header,
            evidence_unit_ids: fields.evidence_unit_ids,
            query_plan_hash: fields.query_plan_hash,
        })
    }

    pub fn validate(&self) -> Result<()> {
        self.header.validate()?;
        if self.header.identity.kind != WireArtifactKind::EvidencePack {
            bail!(
                "evidence pack envelope identity kind must be evidence_pack, got {}",
                self.header.identity.kind
            );
        }
        if self.evidence_unit_ids.is_empty() {
            bail!("evidence_unit_ids must not be empty");
        }
        for id in &self.evidence_unit_ids {
            if id.trim().is_empty() {
                bail!("evidence_unit_ids must not contain empty entries");
            }
        }
        if self.query_plan_hash.trim().is_empty() {
            bail!("query_plan_hash must not be empty");
        }
        if self.query_plan_hash.chars().any(|c| c.is_whitespace()) {
            bail!("query_plan_hash must not contain whitespace");
        }
        verify_body_hash(
            self.header.identity.content_hash.as_str(),
            &EvidencePackBody {
                evidence_unit_ids: self.evidence_unit_ids.clone(),
                query_plan_hash: self.query_plan_hash.clone(),
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidencePackBody {
    evidence_unit_ids: Vec<String>,
    query_plan_hash: String,
}

/// Minimal ContextPack wire envelope (grounding input, not a final answer).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPackEnvelope {
    pub header: WireEnvelopeHeader,
    /// EvidencePack content hash this pack was built from.
    pub evidence_pack_hash: String,
    /// Ordered selected evidence unit ids included in the pack.
    pub selected_unit_ids: Vec<String>,
    /// Optional model/deployment fingerprint used for assembly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
}

/// Field bundle for [`ContextPackEnvelope::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextPackFields {
    pub artifact_id: String,
    pub evidence_pack_hash: String,
    pub selected_unit_ids: Vec<String>,
    pub model_fingerprint: Option<String>,
    pub generation: Option<String>,
    pub profile_ref: Option<String>,
}

impl ContextPackEnvelope {
    pub fn new(fields: ContextPackFields) -> Result<Self> {
        if fields.evidence_pack_hash.trim().is_empty() {
            bail!("evidence_pack_hash must not be empty");
        }
        if fields.evidence_pack_hash.chars().any(|c| c.is_whitespace()) {
            bail!("evidence_pack_hash must not contain whitespace");
        }
        if fields.selected_unit_ids.is_empty() {
            bail!("selected_unit_ids must not be empty");
        }
        for id in &fields.selected_unit_ids {
            if id.trim().is_empty() {
                bail!("selected_unit_ids must not contain empty entries");
            }
        }
        if let Some(fp) = fields.model_fingerprint.as_ref() {
            if fp.trim().is_empty() {
                bail!("model_fingerprint must not be empty when present");
            }
        }
        let body = ContextPackBody {
            evidence_pack_hash: fields.evidence_pack_hash.clone(),
            selected_unit_ids: fields.selected_unit_ids.clone(),
            model_fingerprint: fields.model_fingerprint.clone(),
        };
        let body_bytes = encode_wire_document(&body)?;
        let identity = CanonicalIdentity::from_body(
            WireArtifactKind::ContextPack,
            super::WIRE_SCHEMA_VERSION,
            fields.artifact_id,
            &body_bytes,
        )?;
        let header = WireEnvelopeHeader::new(WireEnvelopeHeaderFields {
            identity,
            generation: fields.generation,
            profile_ref: fields.profile_ref,
        })?;
        Ok(Self {
            header,
            evidence_pack_hash: fields.evidence_pack_hash,
            selected_unit_ids: fields.selected_unit_ids,
            model_fingerprint: fields.model_fingerprint,
        })
    }

    pub fn validate(&self) -> Result<()> {
        self.header.validate()?;
        if self.header.identity.kind != WireArtifactKind::ContextPack {
            bail!(
                "context pack envelope identity kind must be context_pack, got {}",
                self.header.identity.kind
            );
        }
        if self.evidence_pack_hash.trim().is_empty() {
            bail!("evidence_pack_hash must not be empty");
        }
        if self.evidence_pack_hash.chars().any(|c| c.is_whitespace()) {
            bail!("evidence_pack_hash must not contain whitespace");
        }
        if self.selected_unit_ids.is_empty() {
            bail!("selected_unit_ids must not be empty");
        }
        for id in &self.selected_unit_ids {
            if id.trim().is_empty() {
                bail!("selected_unit_ids must not contain empty entries");
            }
        }
        if let Some(fp) = self.model_fingerprint.as_ref() {
            if fp.trim().is_empty() {
                bail!("model_fingerprint must not be empty when present");
            }
        }
        verify_body_hash(
            self.header.identity.content_hash.as_str(),
            &ContextPackBody {
                evidence_pack_hash: self.evidence_pack_hash.clone(),
                selected_unit_ids: self.selected_unit_ids.clone(),
                model_fingerprint: self.model_fingerprint.clone(),
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextPackBody {
    evidence_pack_hash: String,
    selected_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_fingerprint: Option<String>,
}

/// Workflow phase for the minimal workflow envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    Planned,
    Retrieving,
    Assembling,
    Generating,
    Verifying,
    Completed,
    Failed,
}

impl WorkflowPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Retrieving => "retrieving",
            Self::Assembling => "assembling",
            Self::Generating => "generating",
            Self::Verifying => "verifying",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Minimal WorkflowRun / workflow envelope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEnvelope {
    pub header: WireEnvelopeHeader,
    pub phase: WorkflowPhase,
    /// QueryPlan content hash bound to this run.
    pub query_plan_hash: String,
    /// Optional EvidencePack hash once retrieval completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_pack_hash: Option<String>,
    /// Optional ContextPack hash once assembly completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pack_hash: Option<String>,
}

/// Field bundle for [`WorkflowEnvelope::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkflowEnvelopeFields {
    pub artifact_id: String,
    pub phase: WorkflowPhase,
    pub query_plan_hash: String,
    pub evidence_pack_hash: Option<String>,
    pub context_pack_hash: Option<String>,
    pub generation: Option<String>,
    pub profile_ref: Option<String>,
}

impl WorkflowEnvelope {
    pub fn new(fields: WorkflowEnvelopeFields) -> Result<Self> {
        if fields.query_plan_hash.trim().is_empty() {
            bail!("query_plan_hash must not be empty");
        }
        if fields.query_plan_hash.chars().any(|c| c.is_whitespace()) {
            bail!("query_plan_hash must not contain whitespace");
        }
        validate_optional_hash("evidence_pack_hash", fields.evidence_pack_hash.as_deref())?;
        validate_optional_hash("context_pack_hash", fields.context_pack_hash.as_deref())?;
        let body = WorkflowBody {
            phase: fields.phase,
            query_plan_hash: fields.query_plan_hash.clone(),
            evidence_pack_hash: fields.evidence_pack_hash.clone(),
            context_pack_hash: fields.context_pack_hash.clone(),
        };
        let body_bytes = encode_wire_document(&body)?;
        let identity = CanonicalIdentity::from_body(
            WireArtifactKind::WorkflowEnvelope,
            super::WIRE_SCHEMA_VERSION,
            fields.artifact_id,
            &body_bytes,
        )?;
        let header = WireEnvelopeHeader::new(WireEnvelopeHeaderFields {
            identity,
            generation: fields.generation,
            profile_ref: fields.profile_ref,
        })?;
        Ok(Self {
            header,
            phase: fields.phase,
            query_plan_hash: fields.query_plan_hash,
            evidence_pack_hash: fields.evidence_pack_hash,
            context_pack_hash: fields.context_pack_hash,
        })
    }

    pub fn validate(&self) -> Result<()> {
        self.header.validate()?;
        if self.header.identity.kind != WireArtifactKind::WorkflowEnvelope {
            bail!(
                "workflow envelope identity kind must be workflow_envelope, got {}",
                self.header.identity.kind
            );
        }
        if self.query_plan_hash.trim().is_empty() {
            bail!("query_plan_hash must not be empty");
        }
        if self.query_plan_hash.chars().any(|c| c.is_whitespace()) {
            bail!("query_plan_hash must not contain whitespace");
        }
        validate_optional_hash("evidence_pack_hash", self.evidence_pack_hash.as_deref())?;
        validate_optional_hash("context_pack_hash", self.context_pack_hash.as_deref())?;
        verify_body_hash(
            self.header.identity.content_hash.as_str(),
            &WorkflowBody {
                phase: self.phase,
                query_plan_hash: self.query_plan_hash.clone(),
                evidence_pack_hash: self.evidence_pack_hash.clone(),
                context_pack_hash: self.context_pack_hash.clone(),
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowBody {
    phase: WorkflowPhase,
    query_plan_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_pack_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_pack_hash: Option<String>,
}

fn validate_optional_hash(name: &str, value: Option<&str>) -> Result<()> {
    let Some(raw) = value else {
        return Ok(());
    };
    if raw.trim().is_empty() {
        bail!("{name} must not be empty when present");
    }
    if raw.chars().any(|c| c.is_whitespace()) {
        bail!("{name} must not contain whitespace");
    }
    Ok(())
}

fn verify_body_hash<T: Serialize>(declared: &str, body: &T) -> Result<()> {
    let bytes = encode_wire_document(body)?;
    let actual = wire_content_hash(&bytes);
    if declared != actual {
        bail!("content hash mismatch: declared {declared}, actual {actual}");
    }
    Ok(())
}

/// Decode a JSON QueryPlan envelope and reject unknown schema / invalid fields.
pub fn decode_query_plan_envelope_json(bytes: &[u8]) -> Result<QueryPlanEnvelope> {
    let value: QueryPlanEnvelope = serde_json::from_slice(bytes)?;
    value.validate()?;
    Ok(value)
}

/// Decode a JSON EvidencePack envelope and reject unknown schema / invalid fields.
pub fn decode_evidence_pack_envelope_json(bytes: &[u8]) -> Result<EvidencePackEnvelope> {
    let value: EvidencePackEnvelope = serde_json::from_slice(bytes)?;
    value.validate()?;
    Ok(value)
}

/// Decode a JSON ContextPack envelope and reject unknown schema / invalid fields.
pub fn decode_context_pack_envelope_json(bytes: &[u8]) -> Result<ContextPackEnvelope> {
    let value: ContextPackEnvelope = serde_json::from_slice(bytes)?;
    value.validate()?;
    Ok(value)
}

/// Decode a JSON Workflow envelope and reject unknown schema / invalid fields.
pub fn decode_workflow_envelope_json(bytes: &[u8]) -> Result<WorkflowEnvelope> {
    let value: WorkflowEnvelope = serde_json::from_slice(bytes)?;
    value.validate()?;
    Ok(value)
}
