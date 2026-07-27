//! Typed strict-filter, candidate-validation, and adaptive-overfetch policy.

use serde::{Deserialize, Deserializer, Serialize};

use super::{OverfetchError, OverfetchResult, SearchBudget};

const MAX_FILTERS: usize = 16;
const MAX_FILTER_VALUE_BYTES: usize = 512;
const MAX_CANDIDATE_ID_BYTES: usize = 512;

/// Lifecycle predicate values accepted by normal retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Active,
    Archived,
    Deleted,
}

/// A required predicate which must be applied before normal result hydration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum StrictFilter {
    Source(String),
    Collection(String),
    Tenant(String),
    Acl(String),
    Lifecycle(LifecycleState),
}

impl StrictFilter {
    pub fn source(value: impl Into<String>) -> OverfetchResult<Self> {
        Ok(Self::Source(checked_filter_value(value)?))
    }

    pub fn collection(value: impl Into<String>) -> OverfetchResult<Self> {
        Ok(Self::Collection(checked_filter_value(value)?))
    }

    pub fn tenant(value: impl Into<String>) -> OverfetchResult<Self> {
        Ok(Self::Tenant(checked_filter_value(value)?))
    }

    pub fn acl(value: impl Into<String>) -> OverfetchResult<Self> {
        Ok(Self::Acl(checked_filter_value(value)?))
    }

    pub const fn lifecycle(value: LifecycleState) -> Self {
        Self::Lifecycle(value)
    }

    fn validate(&self) -> OverfetchResult<()> {
        match self {
            Self::Source(value)
            | Self::Collection(value)
            | Self::Tenant(value)
            | Self::Acl(value) => checked_filter_value(value.clone()).map(|_| ()),
            Self::Lifecycle(_) => Ok(()),
        }
    }
}

/// A bounded conjunction of strict retrieval predicates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct RetrievalFilters {
    predicates: Vec<StrictFilter>,
}

#[derive(Deserialize)]
struct RetrievalFiltersFields {
    predicates: Vec<StrictFilter>,
}

impl<'de> Deserialize<'de> for RetrievalFilters {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = RetrievalFiltersFields::deserialize(deserializer)?;
        Self::new(fields.predicates).map_err(serde::de::Error::custom)
    }
}

impl RetrievalFilters {
    /// Builds a bounded filter set and validates untrusted filter values.
    pub fn new(predicates: Vec<StrictFilter>) -> OverfetchResult<Self> {
        if predicates.len() > MAX_FILTERS {
            return Err(OverfetchError::UnsupportedStrictFilter);
        }
        for predicate in &predicates {
            predicate.validate()?;
        }
        Ok(Self { predicates })
    }

    /// Immutable strict predicates to give to a count or retriever adapter.
    pub fn predicates(&self) -> &[StrictFilter] {
        &self.predicates
    }

    /// Whether any strict predicate constrains normal retrieval.
    pub fn is_strict(&self) -> bool {
        !self.predicates.is_empty()
    }
}

/// Opaque candidate identifier that is validated before it enters a stage list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CandidateId(String);

impl<'de> Deserialize<'de> for CandidateId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl CandidateId {
    pub fn new(value: impl Into<String>) -> OverfetchResult<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_CANDIDATE_ID_BYTES {
            return Err(OverfetchError::BudgetExceeded);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A finite-score candidate returned by one bounded retriever.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetrievalCandidate {
    id: CandidateId,
    score: f32,
}

#[derive(Deserialize)]
struct RetrievalCandidateFields {
    id: CandidateId,
    score: f32,
}

impl<'de> Deserialize<'de> for RetrievalCandidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = RetrievalCandidateFields::deserialize(deserializer)?;
        Self::new(fields.id, fields.score).map_err(serde::de::Error::custom)
    }
}

impl RetrievalCandidate {
    pub fn new(id: CandidateId, score: f32) -> OverfetchResult<Self> {
        if !score.is_finite() {
            return Err(OverfetchError::BudgetExceeded);
        }
        Ok(Self { id, score })
    }

    pub fn id(&self) -> &CandidateId {
        &self.id
    }

    pub const fn score(&self) -> f32 {
        self.score
    }
}

/// Lightweight acceptance of a candidate before any complete record is fetched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateValidation {
    candidate: RetrievalCandidate,
}

impl CandidateValidation {
    pub fn new(candidate: RetrievalCandidate) -> OverfetchResult<Self> {
        if !candidate.score().is_finite() {
            return Err(OverfetchError::BudgetExceeded);
        }
        Ok(Self { candidate })
    }

    pub fn candidate(&self) -> &RetrievalCandidate {
        &self.candidate
    }
}

/// Validated lightweight candidates, capped before reranking and hydration.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedCandidates {
    candidates: Vec<CandidateValidation>,
}

impl ValidatedCandidates {
    /// Hard-truncates validated candidates to the rerank input cap.
    pub fn new(
        mut candidates: Vec<CandidateValidation>,
        budget: &SearchBudget,
    ) -> OverfetchResult<Self> {
        budget.validate()?;
        candidates.truncate(budget.rerank_input_size as usize);
        Ok(Self { candidates })
    }

    pub fn candidates(&self) -> &[CandidateValidation] {
        &self.candidates
    }

    /// Final hydration input is capped before the hydration adapter is called.
    pub fn for_hydration(&self, budget: &SearchBudget) -> Vec<CandidateValidation> {
        self.candidates
            .iter()
            .take(budget.final_hydration_list_size as usize)
            .cloned()
            .collect()
    }

    pub(crate) fn truncate_to(self, budget: &SearchBudget) -> Self {
        let mut candidates = self.candidates;
        candidates.truncate(budget.rerank_input_size as usize);
        Self { candidates }
    }
}

/// Declarative bounded-overfetch controls for a backend without native filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveOverfetchPolicyFields {
    pub initial_candidate_k: u32,
    pub max_candidate_k: u32,
    pub growth_factor: u8,
    pub max_attempts: u8,
}

/// Bounded adaptive-overfetch policy; it can never choose corpus-size Top-K.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdaptiveOverfetchPolicy {
    pub initial_candidate_k: u32,
    pub max_candidate_k: u32,
    pub growth_factor: u8,
    pub max_attempts: u8,
}

impl<'de> Deserialize<'de> for AdaptiveOverfetchPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = AdaptiveOverfetchPolicyFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

impl AdaptiveOverfetchPolicy {
    pub fn new(fields: AdaptiveOverfetchPolicyFields) -> OverfetchResult<Self> {
        let policy = Self {
            initial_candidate_k: fields.initial_candidate_k,
            max_candidate_k: fields.max_candidate_k,
            growth_factor: fields.growth_factor,
            max_attempts: fields.max_attempts,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> OverfetchResult<()> {
        if self.initial_candidate_k == 0
            || self.max_candidate_k < self.initial_candidate_k
            || self.growth_factor < 2
            || self.max_attempts == 0
        {
            return Err(OverfetchError::BudgetExceeded);
        }
        Ok(())
    }

    /// Request cap for a bounded adaptive attempt.
    ///
    /// A zero-sized corpus needs no backend request. Every nonzero request is
    /// checked against the corpus cardinality so an adapter cannot substitute a
    /// full-corpus Top-K for a missing native strict predicate.
    pub fn candidate_k_for_attempt(
        &self,
        attempt: u8,
        retriever_cap: u32,
        corpus_size: u64,
    ) -> OverfetchResult<u32> {
        self.validate()?;
        if attempt >= self.max_attempts || retriever_cap == 0 {
            return Err(OverfetchError::BudgetExceeded);
        }
        if corpus_size == 0 {
            return Ok(0);
        }

        let mut requested = self.initial_candidate_k;
        for _ in 0..attempt {
            requested = requested
                .saturating_mul(u32::from(self.growth_factor))
                .min(self.max_candidate_k);
        }
        let requested = requested.min(self.max_candidate_k).min(retriever_cap);
        if u64::from(requested) >= corpus_size {
            return Err(OverfetchError::CorpusSizeTopKForbidden);
        }
        Ok(requested)
    }
}

/// How a backend handles a required strict predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "policy")]
pub enum StrictFilterSupport {
    Native,
    Adaptive(AdaptiveOverfetchPolicy),
    Unsupported,
}

impl StrictFilterSupport {
    /// Selects a bounded request only when the backend can safely honor it.
    pub fn candidate_k_for_attempt(
        &self,
        requested_k: u32,
        retriever_cap: u32,
        corpus_size: u64,
        attempt: u8,
    ) -> OverfetchResult<u32> {
        if requested_k == 0 || requested_k > retriever_cap {
            return Err(OverfetchError::BudgetExceeded);
        }
        match self {
            Self::Native => Ok(requested_k),
            Self::Adaptive(policy) => {
                policy.candidate_k_for_attempt(attempt, retriever_cap, corpus_size)
            }
            Self::Unsupported => Err(OverfetchError::UnsupportedStrictFilter),
        }
    }
}

fn checked_filter_value(value: impl Into<String>) -> OverfetchResult<String> {
    let value = value.into();
    if value.is_empty() || value.len() > MAX_FILTER_VALUE_BYTES {
        return Err(OverfetchError::UnsupportedStrictFilter);
    }
    Ok(value)
}
