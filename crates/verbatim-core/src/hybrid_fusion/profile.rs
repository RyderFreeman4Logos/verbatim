//! Fusion strategies, score normalization, and versioned fusion profiles.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    FusionBudgetFields, HybridFusionDiagnosticCode, HybridFusionError, HybridFusionResult,
};

/// The fusion strategy selected for a profile.
///
/// `BackendServerFusionOptIn` records that a named profile explicitly accepts
/// reduced explainability in exchange for backend-side fusion (Qdrant/LanceDB).
/// It is only permitted when the adapter still preserves per-retriever
/// rankings/scores/filters/generation/partial-state, or when the profile
/// explicitly accepts reduced explainability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionStrategy {
    /// Reciprocal Rank Fusion. The baseline BM25+dense strategy.
    Rrf,
    /// Weighted score combination (e.g. DBSF-like normalized fusion).
    WeightedScore,
    /// Exact/reference precedence: must-include references override score.
    ExactReferencePrecedence,
    /// Opt-in to backend server-side fusion with explicit reduced explainability.
    BackendServerFusionOptIn,
}

impl FusionStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rrf => "rrf",
            Self::WeightedScore => "weighted_score",
            Self::ExactReferencePrecedence => "exact_reference_precedence",
            Self::BackendServerFusionOptIn => "backend_server_fusion_opt_in",
        }
    }

    /// Returns `true` when the strategy requires score normalization.
    pub const fn requires_score_normalization(self) -> bool {
        matches!(self, Self::WeightedScore)
    }

    /// Returns `true` when the strategy is the backend opt-in variant.
    pub const fn is_backend_opt_in(self) -> bool {
        matches!(self, Self::BackendServerFusionOptIn)
    }
}

/// The kind of score normalization applied during fusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreNormalizationKind {
    /// Min-max normalization to `[0,1]`.
    MinMax,
    /// Z-score normalization.
    ZScore,
    /// No normalization (raw scores retained; strategy must not require it).
    None,
}

impl ScoreNormalizationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MinMax => "min_max",
            Self::ZScore => "z_score",
            Self::None => "none",
        }
    }
}

/// The explainability level promised by a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainabilityLevel {
    /// Full per-retriever provenance, raw ranks/scores, and inclusion reasons.
    Full,
    /// Provenance retained but raw scores may be normalized-only.
    Reduced,
}

impl ExplainabilityLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
        }
    }
}

/// Weight assigned to one retriever within a weighted-score or RRF profile.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RetrieverWeight {
    retriever_id: String,
    weight_bits: u64,
}

impl RetrieverWeight {
    pub fn new(retriever_id: String, weight: f64) -> HybridFusionResult<Self> {
        if retriever_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverIdEmpty,
            ));
        }
        if !weight.is_finite() || weight <= 0.0 {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::ProfileWeightsMustBePositive,
            ));
        }
        Ok(Self {
            retriever_id,
            weight_bits: weight.to_bits(),
        })
    }

    pub fn retriever_id(&self) -> &str {
        &self.retriever_id
    }

    pub fn weight(&self) -> f64 {
        f64::from_bits(self.weight_bits)
    }
}

/// Field bag used to construct and validate a [`FusionProfile`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionProfileFields {
    pub version: u32,
    pub strategy: FusionStrategy,
    pub weights: Vec<RetrieverWeight>,
    pub score_normalization: ScoreNormalizationKind,
    pub rrf_constant: u32,
    pub candidate_limits: FusionBudgetFields,
    pub explainability: ExplainabilityLevel,
    pub accepts_reduced_explainability: bool,
}

/// Versioned, hash-bound fusion policy configuration.
///
/// The hash is computed from exactly the public fields; it is an audit
/// binding, not a secret or security key.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FusionProfile {
    version: u32,
    profile_hash: String,
    strategy: FusionStrategy,
    weights: Vec<RetrieverWeight>,
    score_normalization: ScoreNormalizationKind,
    rrf_constant: u32,
    candidate_limits: FusionBudgetFields,
    explainability: ExplainabilityLevel,
    accepts_reduced_explainability: bool,
}

impl<'de> Deserialize<'de> for FusionProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Shadow {
            version: u32,
            profile_hash: String,
            strategy: FusionStrategy,
            weights: Vec<RetrieverWeight>,
            score_normalization: ScoreNormalizationKind,
            rrf_constant: u32,
            candidate_limits: FusionBudgetFields,
            explainability: ExplainabilityLevel,
            accepts_reduced_explainability: bool,
        }
        let shadow = Shadow::deserialize(deserializer)?;
        let profile = FusionProfile {
            version: shadow.version,
            profile_hash: shadow.profile_hash,
            strategy: shadow.strategy,
            weights: shadow.weights,
            score_normalization: shadow.score_normalization,
            rrf_constant: shadow.rrf_constant,
            candidate_limits: shadow.candidate_limits,
            explainability: shadow.explainability,
            accepts_reduced_explainability: shadow.accepts_reduced_explainability,
        };
        profile.validate().map_err(serde::de::Error::custom)?;
        Ok(profile)
    }
}

impl FusionProfile {
    /// Builds a profile only after validating strategy/weights/limits/explainability.
    pub fn new(fields: FusionProfileFields) -> HybridFusionResult<Self> {
        validate_fields(&fields)?;
        let profile_hash = Self::hash_fields(&fields)?;
        Ok(Self {
            version: fields.version,
            profile_hash,
            strategy: fields.strategy,
            weights: fields.weights,
            score_normalization: fields.score_normalization,
            rrf_constant: fields.rrf_constant,
            candidate_limits: fields.candidate_limits,
            explainability: fields.explainability,
            accepts_reduced_explainability: fields.accepts_reduced_explainability,
        })
    }

    /// Computes the deterministic SHA-256 hash of the canonical field encoding.
    pub fn hash_fields(fields: &FusionProfileFields) -> HybridFusionResult<String> {
        validate_fields(fields)?;
        let canonical = serde_json::to_vec(fields).map_err(|_| {
            HybridFusionError::validation(HybridFusionDiagnosticCode::ProfileSerializationFailed)
        })?;
        Ok(format!("{:x}", Sha256::digest(canonical)))
    }

    /// Revalidates the profile and confirms the stored hash matches.
    pub fn validate(&self) -> HybridFusionResult<()> {
        let fields = FusionProfileFields {
            version: self.version,
            strategy: self.strategy,
            weights: self.weights.clone(),
            score_normalization: self.score_normalization,
            rrf_constant: self.rrf_constant,
            candidate_limits: self.candidate_limits,
            explainability: self.explainability,
            accepts_reduced_explainability: self.accepts_reduced_explainability,
        };
        let expected_hash = Self::hash_fields(&fields)?;
        if self.profile_hash != expected_hash {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::ProfileHashMismatch,
            ));
        }
        Ok(())
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub fn profile_hash(&self) -> &str {
        &self.profile_hash
    }

    pub const fn strategy(&self) -> FusionStrategy {
        self.strategy
    }

    pub fn weights(&self) -> &[RetrieverWeight] {
        &self.weights
    }

    pub const fn score_normalization(&self) -> ScoreNormalizationKind {
        self.score_normalization
    }

    pub const fn rrf_constant(&self) -> u32 {
        self.rrf_constant
    }

    pub const fn candidate_limits(&self) -> FusionBudgetFields {
        self.candidate_limits
    }

    pub const fn explainability(&self) -> ExplainabilityLevel {
        self.explainability
    }

    pub const fn accepts_reduced_explainability(&self) -> bool {
        self.accepts_reduced_explainability
    }
}

fn validate_fields(fields: &FusionProfileFields) -> HybridFusionResult<()> {
    if fields.version == 0 {
        return Err(HybridFusionError::validation(
            HybridFusionDiagnosticCode::ProfileVersionInvalid,
        ));
    }
    // Weights must be positive and sum to ~1.0 for WeightedScore.
    let mut sum = 0.0_f64;
    let mut seen = std::collections::BTreeSet::new();
    for weight in &fields.weights {
        if !weight.weight().is_finite() || weight.weight() <= 0.0 {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::ProfileWeightsMustBePositive,
            ));
        }
        if !seen.insert(weight.retriever_id()) {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateDuplicateContributingRetriever,
            ));
        }
        sum += weight.weight();
    }
    if fields.strategy == FusionStrategy::WeightedScore {
        let tolerance = 1e-6;
        if (sum - 1.0).abs() > tolerance {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::ProfileWeightsMustSumToUnit,
            ));
        }
    }
    if fields.strategy == FusionStrategy::Rrf && fields.rrf_constant == 0 {
        return Err(HybridFusionError::validation(
            HybridFusionDiagnosticCode::ProfileRrfConstantInvalid,
        ));
    }
    // Candidate limits: positive and monotonic.
    let caps = [
        fields.candidate_limits.max_retriever_candidates,
        fields.candidate_limits.max_fused_pool_size,
        fields.candidate_limits.max_rerank_input_size,
        fields.candidate_limits.max_final_hydration_list_size,
        fields.candidate_limits.max_debug_output_size,
    ];
    if caps.contains(&0) {
        return Err(HybridFusionError::validation(
            HybridFusionDiagnosticCode::ProfileCandidateLimitsMustBePositive,
        ));
    }
    if fields.candidate_limits.max_fused_pool_size
        > fields.candidate_limits.max_retriever_candidates
        || fields.candidate_limits.max_rerank_input_size
            > fields.candidate_limits.max_fused_pool_size
        || fields.candidate_limits.max_final_hydration_list_size
            > fields.candidate_limits.max_rerank_input_size
    {
        return Err(HybridFusionError::validation(
            HybridFusionDiagnosticCode::ProfileCandidateLimitsMustMonotonic,
        ));
    }
    // Strategy / explainability consistency.
    if fields.strategy == FusionStrategy::WeightedScore
        && fields.score_normalization == ScoreNormalizationKind::None
    {
        return Err(HybridFusionError::validation(
            HybridFusionDiagnosticCode::ScoreNormalizationUnsupportedForStrategy,
        ));
    }
    if fields.strategy.is_backend_opt_in() && !fields.accepts_reduced_explainability {
        return Err(HybridFusionError::validation(
            HybridFusionDiagnosticCode::ProfileStrategyBackendOptInRequiresAcceptance,
        ));
    }
    if fields.explainability == ExplainabilityLevel::Reduced
        && !fields.accepts_reduced_explainability
    {
        return Err(HybridFusionError::validation(
            HybridFusionDiagnosticCode::ProfileExplainabilityLevelInvalid,
        ));
    }
    Ok(())
}
