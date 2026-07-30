//! Typed routing plans, explicit capability outcomes, and preserved candidates.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    CandidateIndexProfileId, NamedVectorSpaceDiagnosticCode, NamedVectorSpaceError,
    NamedVectorSpaceId, NamedVectorSpaceResult, NamedVectorSpaceSpec, ObjectId,
    PublicationGeneration, QueryOperation,
};

/// Shape only: query vectors remain owned by a future execution implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum QueryVectorShape {
    Dense {
        native_dimension: u32,
    },
    LateInteraction {
        native_dimension: u32,
        query_token_count: u32,
    },
}

impl QueryVectorShape {
    pub fn dense(native_dimension: u32) -> NamedVectorSpaceResult<Self> {
        if native_dimension == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::IncompatibleQueryShape,
            ));
        }
        Ok(Self::Dense { native_dimension })
    }
    pub fn late_interaction(
        native_dimension: u32,
        query_token_count: u32,
    ) -> NamedVectorSpaceResult<Self> {
        if native_dimension == 0 || query_token_count == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::IncompatibleQueryShape,
            ));
        }
        Ok(Self::LateInteraction {
            native_dimension,
            query_token_count,
        })
    }
    const fn native_dimension(self) -> u32 {
        match self {
            Self::Dense { native_dimension }
            | Self::LateInteraction {
                native_dimension, ..
            } => native_dimension,
        }
    }
    const fn is_late_interaction(self) -> bool {
        matches!(self, Self::LateInteraction { .. })
    }
}

/// A requested named-vector clause; only these clauses may generate candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedVectorClause {
    space: NamedVectorSpaceId,
    operation: QueryOperation,
    shape: QueryVectorShape,
}

impl NamedVectorClause {
    pub fn new(
        space: NamedVectorSpaceId,
        operation: QueryOperation,
        shape: QueryVectorShape,
    ) -> Self {
        Self {
            space,
            operation,
            shape,
        }
    }
    pub const fn space(&self) -> &NamedVectorSpaceId {
        &self.space
    }
    pub const fn operation(&self) -> QueryOperation {
        self.operation
    }
    pub const fn shape(&self) -> QueryVectorShape {
        self.shape
    }
}

/// One shared budget, consumed across all named spaces and modalities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchBudget {
    maximum_candidates: u32,
}
impl SearchBudget {
    pub fn new(maximum_candidates: u32) -> NamedVectorSpaceResult<Self> {
        if maximum_candidates == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidSearchBudget,
            ));
        }
        Ok(Self { maximum_candidates })
    }
    pub const fn maximum_candidates(self) -> u32 {
        self.maximum_candidates
    }
}

/// Explicit versioned identity for a #381 hybrid-fusion profile; this module does
/// not reimplement fusion.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct FusionProfileIdentity {
    id: String,
    version: u32,
}
impl<'de> Deserialize<'de> for FusionProfileIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            id: String,
            version: u32,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.id, wire.version).map_err(serde::de::Error::custom)
    }
}
impl FusionProfileIdentity {
    pub fn new(id: impl Into<String>, version: u32) -> NamedVectorSpaceResult<Self> {
        let id = id.into();
        let valid = !id.is_empty()
            && id.len() <= 128
            && id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            });
        if !valid || version == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
            ));
        }
        Ok(Self { id, version })
    }
    pub fn as_str(&self) -> &str {
        &self.id
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
}

/// Typed plan before capability selection or backend execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedVectorQueryPlan {
    clauses: Vec<NamedVectorClause>,
    shared_budget: SearchBudget,
    fusion_profile: FusionProfileIdentity,
}
impl NamedVectorQueryPlan {
    pub const MAX_CLAUSES: usize = 64;
    pub fn new(
        clauses: Vec<NamedVectorClause>,
        maximum_candidates: u32,
        fusion_profile: FusionProfileIdentity,
    ) -> NamedVectorSpaceResult<Self> {
        if clauses.is_empty() || clauses.len() > Self::MAX_CLAUSES {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
            ));
        }
        let mut seen = BTreeSet::new();
        if clauses
            .iter()
            .any(|clause| !seen.insert(clause.space.as_str()))
        {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
            ));
        }
        Ok(Self {
            clauses,
            shared_budget: SearchBudget::new(maximum_candidates)?,
            fusion_profile,
        })
    }
    pub fn clauses(&self) -> &[NamedVectorClause] {
        &self.clauses
    }
    pub const fn shared_budget(&self) -> SearchBudget {
        self.shared_budget
    }
    pub const fn fusion_profile(&self) -> &FusionProfileIdentity {
        &self.fusion_profile
    }

    /// Compiles only requested eligible clauses. Unsupported operations fail with
    /// a visible code; `degraded_profiles` are data only and are never selected implicitly.
    pub fn compile(
        &self,
        specs: &[NamedVectorSpaceSpec],
        capabilities: BackendCapabilities,
        _degraded_profiles: &[DegradedProfile],
    ) -> NamedVectorSpaceResult<CompiledNamedVectorPlan> {
        if !capabilities.supports_named_vectors {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::UnsupportedBackendCapability,
            ));
        }
        let mut available = BTreeSet::new();
        for spec in specs {
            if !available.insert(spec.name().as_str()) {
                return Err(NamedVectorSpaceError::contract(
                    NamedVectorSpaceDiagnosticCode::InvalidSpaceSpecification,
                ));
            }
        }
        for clause in &self.clauses {
            let spec = specs
                .iter()
                .find(|spec| spec.name() == clause.space())
                .ok_or_else(|| {
                    NamedVectorSpaceError::contract(
                        NamedVectorSpaceDiagnosticCode::MissingVectorSpace,
                    )
                })?;
            if !spec.supports(clause.operation) {
                return Err(NamedVectorSpaceError::contract(
                    NamedVectorSpaceDiagnosticCode::UnsupportedQueryOperation,
                ));
            }
            if clause.shape.native_dimension() != spec.native_dimension()
                || clause.shape.is_late_interaction()
                    != matches!(clause.operation, QueryOperation::LateInteractionMaxSim)
            {
                return Err(NamedVectorSpaceError::contract(
                    NamedVectorSpaceDiagnosticCode::IncompatibleQueryShape,
                ));
            }
            if matches!(clause.operation, QueryOperation::LateInteractionMaxSim)
                && !capabilities.supports_late_interaction
            {
                return Err(NamedVectorSpaceError::contract(
                    NamedVectorSpaceDiagnosticCode::UnsupportedBackendCapability,
                ));
            }
        }
        Ok(CompiledNamedVectorPlan {
            clauses: self.clauses.clone(),
            shared_budget: self.shared_budget,
            fusion_profile: self.fusion_profile.clone(),
        })
    }
}

/// Compiled route contract, intentionally without client, I/O, or fusion code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledNamedVectorPlan {
    clauses: Vec<NamedVectorClause>,
    shared_budget: SearchBudget,
    fusion_profile: FusionProfileIdentity,
}
impl CompiledNamedVectorPlan {
    pub fn clauses(&self) -> &[NamedVectorClause] {
        &self.clauses
    }
    pub const fn shared_budget(&self) -> SearchBudget {
        self.shared_budget
    }
    pub const fn fusion_profile(&self) -> &FusionProfileIdentity {
        &self.fusion_profile
    }
}

/// Capability advertisement from an adapter. It cannot cause a fallback itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    supports_named_vectors: bool,
    supports_late_interaction: bool,
}
impl BackendCapabilities {
    pub const fn named_dense_only() -> Self {
        Self {
            supports_named_vectors: true,
            supports_late_interaction: false,
        }
    }
    pub const fn named_vectors_with_late_interaction() -> Self {
        Self {
            supports_named_vectors: true,
            supports_late_interaction: true,
        }
    }
}

/// A separately selected degraded profile. Naming and allowed spaces are bounded,
/// so a backend cannot silently substitute an arbitrary retrieval mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DegradedProfile {
    name: String,
    allowed_spaces: Vec<NamedVectorSpaceId>,
}
impl DegradedProfile {
    pub fn new(
        name: impl Into<String>,
        allowed_spaces: Vec<NamedVectorSpaceId>,
    ) -> NamedVectorSpaceResult<Self> {
        let name = name.into();
        if name.is_empty()
            || name.len() > 128
            || allowed_spaces.is_empty()
            || allowed_spaces.len() > NamedVectorQueryPlan::MAX_CLAUSES
        {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
            ));
        }
        Ok(Self {
            name,
            allowed_spaces,
        })
    }
    pub fn as_str(&self) -> &str {
        &self.name
    }
    pub fn allowed_spaces(&self) -> &[NamedVectorSpaceId] {
        &self.allowed_spaces
    }
}

/// Visible availability state of a requested space or shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "generation")]
pub enum SpaceAvailability {
    Complete(PublicationGeneration),
    Optional(PublicationGeneration),
    Missing,
    Unavailable,
    Stale(PublicationGeneration),
}
impl SpaceAvailability {
    pub const fn complete(generation: PublicationGeneration) -> Self {
        Self::Complete(generation)
    }
    pub const fn stale(generation: PublicationGeneration) -> Self {
        Self::Stale(generation)
    }
    pub const fn missing() -> Self {
        Self::Missing
    }
    pub fn require_complete(self, expected: PublicationGeneration) -> NamedVectorSpaceResult<()> {
        match self {
            Self::Complete(actual) if actual == expected => Ok(()),
            Self::Complete(_) => Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::WrongGeneration,
            )),
            Self::Stale(_) => Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::StaleVectorSpace,
            )),
            Self::Missing | Self::Unavailable | Self::Optional(_) => Err(
                NamedVectorSpaceError::contract(NamedVectorSpaceDiagnosticCode::MissingVectorSpace),
            ),
        }
    }
}

/// Why a candidate entered a named-space ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateInclusionReason {
    RequestedClause,
    BoundedLateInteractionCandidate,
}

/// Raw result facts retained before fusion/hydration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceCandidate {
    object: ObjectId,
    space: NamedVectorSpaceId,
    profile: CandidateIndexProfileId,
    raw_rank: u32,
    raw_score: f64,
    inclusion_reason: CandidateInclusionReason,
}
impl SpaceCandidate {
    pub fn new(
        object: ObjectId,
        space: NamedVectorSpaceId,
        profile: CandidateIndexProfileId,
        raw_rank: u32,
        raw_score: f64,
        inclusion_reason: CandidateInclusionReason,
    ) -> NamedVectorSpaceResult<Self> {
        if raw_rank == 0 || !raw_score.is_finite() {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
            ));
        }
        Ok(Self {
            object,
            space,
            profile,
            raw_rank,
            raw_score,
            inclusion_reason,
        })
    }
    pub const fn raw_rank(&self) -> u32 {
        self.raw_rank
    }
    pub const fn raw_score(&self) -> f64 {
        self.raw_score
    }
    pub const fn inclusion_reason(&self) -> CandidateInclusionReason {
        self.inclusion_reason
    }
    pub const fn object(&self) -> &ObjectId {
        &self.object
    }
    pub const fn space(&self) -> &NamedVectorSpaceId {
        &self.space
    }
    pub const fn profile(&self) -> &CandidateIndexProfileId {
        &self.profile
    }
}
