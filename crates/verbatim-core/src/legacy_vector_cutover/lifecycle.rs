//! Fail-closed cutover, shadow, promotion, retention, and maintenance state.

use std::collections::BTreeSet;

use super::{
    CutoverManifest, LegacyRetirementDiagnosticCode, LegacyRetirementError, LegacyRetirementResult,
    PublicationGeneration,
};

/// Evidence class that must pass before legacy serving retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GateClass {
    /// Common `VectorSearch` behavior is conformant.
    VectorSearchConformance,
    /// Metric, profile, and generation bindings match exactly.
    ExactIdentityValidation,
    /// Filtered, authorized-subset recall meets exact-reference gates.
    AuthorizedSubsetRecall,
    /// Cgroup memory and SSD-I/O budgets are satisfied.
    ResourceBudget,
    /// Cold/warm latency and concurrency requirements are satisfied.
    LatencyAndConcurrency,
    /// Update/delete/compaction/recovery behavior was exercised.
    MutationRecovery,
    /// Candidate and incumbent generations were compared in shadow traffic.
    DualGenerationShadow,
    /// Rollback and disaster recovery were exercised.
    RollbackDisasterRecovery,
    /// Operator documentation and migration tooling are ready.
    OperatorReadiness,
}

impl GateClass {
    /// Every mandatory retirement gate.
    pub const ALL: [Self; 9] = [
        Self::VectorSearchConformance,
        Self::ExactIdentityValidation,
        Self::AuthorizedSubsetRecall,
        Self::ResourceBudget,
        Self::LatencyAndConcurrency,
        Self::MutationRecovery,
        Self::DualGenerationShadow,
        Self::RollbackDisasterRecovery,
        Self::OperatorReadiness,
    ];

    /// Returns the exact closed code used when this evidence class is absent.
    pub const fn missing_diagnostic_code(self) -> LegacyRetirementDiagnosticCode {
        match self {
            Self::VectorSearchConformance => {
                LegacyRetirementDiagnosticCode::MissingVectorSearchConformance
            }
            Self::ExactIdentityValidation => {
                LegacyRetirementDiagnosticCode::MissingExactIdentityValidation
            }
            Self::AuthorizedSubsetRecall => {
                LegacyRetirementDiagnosticCode::MissingAuthorizedSubsetRecall
            }
            Self::ResourceBudget => LegacyRetirementDiagnosticCode::MissingResourceGate,
            Self::LatencyAndConcurrency => {
                LegacyRetirementDiagnosticCode::MissingLatencyConcurrencyGate
            }
            Self::MutationRecovery => LegacyRetirementDiagnosticCode::MissingMutationRecoveryGate,
            Self::DualGenerationShadow => {
                LegacyRetirementDiagnosticCode::MissingDualGenerationShadow
            }
            Self::RollbackDisasterRecovery => {
                LegacyRetirementDiagnosticCode::MissingRollbackDisasterRecovery
            }
            Self::OperatorReadiness => LegacyRetirementDiagnosticCode::MissingOperatorReadiness,
        }
    }
}

/// Aggregated, independently attested retirement-gate evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutoverGates {
    satisfied: BTreeSet<GateClass>,
    diskann3_compiled_only: bool,
}

impl CutoverGates {
    /// Constructs gate evidence from the gate classes that passed.
    pub fn new(satisfied: impl IntoIterator<Item = GateClass>) -> Self {
        Self {
            satisfied: satisfied.into_iter().collect(),
            diskann3_compiled_only: false,
        }
    }

    /// Models the explicitly insufficient "DiskANN3 compiles" observation.
    pub fn compile_only() -> Self {
        Self {
            satisfied: BTreeSet::new(),
            diskann3_compiled_only: true,
        }
    }

    /// Fails unless every independent gate has passed.
    pub fn require_complete(&self) -> LegacyRetirementResult<()> {
        if self.diskann3_compiled_only {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::DiskAnn3CompileOnly,
            ));
        }
        for gate in GateClass::ALL {
            if !self.satisfied.contains(&gate) {
                return Err(LegacyRetirementError::contract(
                    gate.missing_diagnostic_code(),
                ));
            }
        }
        Ok(())
    }
}

/// Whether authoritative stored vectors can be rebuilt without re-embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorReuseDecision {
    /// Valid profile metadata and vector bytes must be reused as-is.
    ReuseValidatedAuthoritativeBytes,
    /// A separately approved re-embedding workflow is required.
    ReembeddingRequired,
}

/// Authority evidence for the stored vector bytes used to build DiskANN3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritativeVectorSource {
    source_generation: PublicationGeneration,
    profile_valid: bool,
    bytes_valid: bool,
}

impl AuthoritativeVectorSource {
    /// Records the concrete authoritative vector generation and validation of
    /// its profile metadata and canonical vector bytes.
    pub const fn new(
        source_generation: PublicationGeneration,
        profile_valid: bool,
        bytes_valid: bool,
    ) -> Self {
        Self {
            source_generation,
            profile_valid,
            bytes_valid,
        }
    }

    /// Returns the concrete authoritative vector generation bound to validation.
    pub const fn source_generation(&self) -> &PublicationGeneration {
        &self.source_generation
    }

    /// Returns the required explicit reuse/re-embedding branch.
    pub const fn reuse_decision(&self) -> VectorReuseDecision {
        if self.profile_valid && self.bytes_valid {
            VectorReuseDecision::ReuseValidatedAuthoritativeBytes
        } else {
            VectorReuseDecision::ReembeddingRequired
        }
    }

    /// Returns whether the source is invalid for direct byte reuse.
    pub const fn requires_reembedding(&self) -> bool {
        matches!(
            self.reuse_decision(),
            VectorReuseDecision::ReembeddingRequired
        )
    }

    /// Rejects continuation that would silently re-embed or substitute vectors.
    pub fn require_reusable_bytes(&self) -> LegacyRetirementResult<()> {
        if self.requires_reembedding() {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::SilentReembeddingForbidden,
            ));
        }
        Ok(())
    }
}

/// Field-level migration validation evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationValidationFields {
    /// Source counts and content hashes match the build input.
    pub counts_and_hashes_valid: bool,
    /// Metric, profile, and source-generation identities match exactly.
    pub metric_profile_generation_valid: bool,
    /// Target dimensionality preserves the authoritative vector dimensionality.
    pub full_dimension_preserved: bool,
    /// Normalization and stable IDs were validated.
    pub normalization_and_ids_valid: bool,
    /// Filter/ACL bindings were validated.
    pub filters_valid: bool,
    /// Sampled recall was checked against an exact reference.
    pub sampled_exact_recall_valid: bool,
    /// Update/delete/compaction/recovery artifacts were exercised.
    pub mutation_recovery_valid: bool,
    /// Candidate resource evidence was bound to this build.
    pub resource_evidence_valid: bool,
    /// The staged candidate has a complete publication manifest.
    pub publication_manifest_valid: bool,
}

/// Complete migration validation artifact.
///
/// Its private binding prevents external construction and ties the evidence to
/// the exact authoritative source and durable manifest checked at retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationValidation {
    source: AuthoritativeVectorSource,
    manifest: CutoverManifest,
}

impl MigrationValidation {
    /// Builds only after every migration artifact is valid, full dimension is
    /// preserved, and reusable authoritative bytes are bound to a manifest.
    pub fn new(
        fields: MigrationValidationFields,
        source: &AuthoritativeVectorSource,
        manifest: &CutoverManifest,
    ) -> LegacyRetirementResult<Self> {
        if !fields.full_dimension_preserved {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::DimensionReductionForbidden,
            ));
        }
        let all_other_valid = fields.counts_and_hashes_valid
            && fields.metric_profile_generation_valid
            && fields.normalization_and_ids_valid
            && fields.filters_valid
            && fields.sampled_exact_recall_valid
            && fields.mutation_recovery_valid
            && fields.resource_evidence_valid
            && fields.publication_manifest_valid;
        if !all_other_valid {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::MigrationValidationIncomplete,
            ));
        }
        source.require_reusable_bytes()?;
        Ok(Self {
            source: source.clone(),
            manifest: manifest.clone(),
        })
    }

    /// Fails unless final authorization uses the source and manifest this
    /// constructor validated.
    fn require_binding(
        &self,
        source: &AuthoritativeVectorSource,
        manifest: &CutoverManifest,
    ) -> LegacyRetirementResult<()> {
        source.require_reusable_bytes()?;
        if self.source != *source || self.manifest != *manifest {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::MigrationValidationBindingMismatch,
            ));
        }
        Ok(())
    }
}

/// Result state of a mirrored incumbent/candidate query comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowComparisonState {
    /// Not all required staged shadow traffic completed.
    Incomplete,
    /// Completed but results or resources did not satisfy the comparison policy.
    Failed,
    /// Completed and satisfied the comparison policy.
    Passed,
}

/// A dual-generation shadow comparison bound to both generations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowComparison {
    incumbent_generation: PublicationGeneration,
    candidate_generation: PublicationGeneration,
    state: ShadowComparisonState,
}

impl ShadowComparison {
    /// Records a shadow comparison; the state is checked before promotion.
    pub fn new(
        incumbent_generation: PublicationGeneration,
        candidate_generation: PublicationGeneration,
        state: ShadowComparisonState,
    ) -> LegacyRetirementResult<Self> {
        if incumbent_generation == candidate_generation {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::ShadowComparisonSameGeneration,
            ));
        }
        Ok(Self {
            incumbent_generation,
            candidate_generation,
            state,
        })
    }

    /// Rejects incomplete or failing shadow traffic.
    pub fn require_promotable(&self) -> LegacyRetirementResult<()> {
        match self.state {
            ShadowComparisonState::Incomplete => Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::ShadowComparisonIncomplete,
            )),
            ShadowComparisonState::Failed => Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::ShadowComparisonFailed,
            )),
            ShadowComparisonState::Passed => Ok(()),
        }
    }

    /// Binds promotion to exactly the candidate that was shadowed.
    pub fn bind_promotion(
        &self,
        gates: &CutoverGates,
        promoted_generation: &PublicationGeneration,
    ) -> LegacyRetirementResult<PublicationBinding> {
        gates.require_complete()?;
        self.require_promotable()?;
        if &self.candidate_generation != promoted_generation {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::PromotionGenerationMismatch,
            ));
        }
        Ok(PublicationBinding {
            incumbent_generation: self.incumbent_generation.clone(),
            candidate_generation: self.candidate_generation.clone(),
        })
    }

    /// Returns the candidate generation observed by the comparison.
    pub const fn candidate_generation(&self) -> &PublicationGeneration {
        &self.candidate_generation
    }
}

/// Promotion binding that prevents an unshadowed generation becoming active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationBinding {
    incumbent_generation: PublicationGeneration,
    candidate_generation: PublicationGeneration,
}

impl PublicationBinding {
    /// Returns the retained incumbent generation.
    pub const fn incumbent_generation(&self) -> &PublicationGeneration {
        &self.incumbent_generation
    }

    /// Returns the candidate generation eligible for publication.
    pub const fn candidate_generation(&self) -> &PublicationGeneration {
        &self.candidate_generation
    }
}

/// Declared inclusive logical retirement window for the incumbent generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackWindow {
    starts_at: u64,
    ends_at: u64,
}

impl RollbackWindow {
    /// Constructs a non-empty retention window.
    pub fn new(starts_at: u64, ends_at: u64) -> LegacyRetirementResult<Self> {
        if ends_at <= starts_at {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::InvalidManifest,
            ));
        }
        Ok(Self { starts_at, ends_at })
    }

    /// Returns whether retirement is now eligible.
    pub const fn has_elapsed(self, now: u64) -> bool {
        now >= self.ends_at
    }

    /// Rejects destructive retirement before the declared end.
    pub fn require_elapsed(self, now: u64) -> LegacyRetirementResult<()> {
        if !self.has_elapsed(now) {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::RollbackWindowActive,
            ));
        }
        Ok(())
    }

    /// Returns the first logical time at which retirement is eligible.
    pub const fn ends_at(self) -> u64 {
        self.ends_at
    }

    /// Returns the logical start of the retention window.
    pub const fn starts_at(self) -> u64 {
        self.starts_at
    }
}

/// Explicit legacy artifact that needs backup-aware maintenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LegacyArtifact {
    /// Serialized resident-HNSW artifact built by `instant-distance`.
    SerializedHnsw,
    /// Stale JSON copy of vector payloads.
    StaleVectorJson,
}

/// Backup-aware maintenance plan for destructive legacy artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyArtifactRemovalPlan {
    artifacts: BTreeSet<LegacyArtifact>,
}

impl LegacyArtifactRemovalPlan {
    /// Requires verified backup and explicit disposition of every legacy artifact.
    pub fn new(
        backups_verified: bool,
        artifacts: impl IntoIterator<Item = LegacyArtifact>,
    ) -> LegacyRetirementResult<Self> {
        if !backups_verified {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::BackupRequired,
            ));
        }
        let artifacts: BTreeSet<_> = artifacts.into_iter().collect();
        if !artifacts.contains(&LegacyArtifact::SerializedHnsw)
            || !artifacts.contains(&LegacyArtifact::StaleVectorJson)
        {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::LegacyArtifactPlanIncomplete,
            ));
        }
        Ok(Self { artifacts })
    }

    /// Returns whether the plan explicitly removes an artifact.
    pub fn removes(&self, artifact: LegacyArtifact) -> bool {
        self.artifacts.contains(&artifact)
    }
}

/// Non-forgeable approval that permits destructive legacy-artifact removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleasePolicyApproval {
    _approved: (),
}

impl ReleasePolicyApproval {
    /// Constructs approval only after the authoritative release policy permits removal.
    pub fn new(policy_allows_removal: bool) -> LegacyRetirementResult<Self> {
        if !policy_allows_removal {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::ReleasePolicyApprovalRequired,
            ));
        }
        Ok(Self { _approved: () })
    }

    /// Returns whether the validated policy permits artifact removal.
    pub const fn allows_legacy_artifact_removal(&self) -> bool {
        true
    }
}

/// Serving capabilities that must remain after legacy retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemainingServingCapabilities {
    authoritative_vectors_remain: bool,
    exact_small_scope_scan_remains: bool,
    conformance_fixtures_remain: bool,
    historical_benchmark_readers_remain: bool,
}

impl RemainingServingCapabilities {
    /// Rejects a retirement plan that would discard required rebuild/reference capabilities.
    pub fn new(
        authoritative_vectors_remain: bool,
        exact_small_scope_scan_remains: bool,
        conformance_fixtures_remain: bool,
        historical_benchmark_readers_remain: bool,
    ) -> LegacyRetirementResult<Self> {
        if !authoritative_vectors_remain
            || !exact_small_scope_scan_remains
            || !conformance_fixtures_remain
        {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::RequiredCapabilityMissing,
            ));
        }
        Ok(Self {
            authoritative_vectors_remain,
            exact_small_scope_scan_remains,
            conformance_fixtures_remain,
            historical_benchmark_readers_remain,
        })
    }

    /// Returns whether canonical rebuild bytes/profile metadata remain.
    pub const fn authoritative_vectors_remain(self) -> bool {
        self.authoritative_vectors_remain
    }

    /// Returns whether bounded exact small-scope scan remains available.
    pub const fn exact_small_scope_scan_remains(self) -> bool {
        self.exact_small_scope_scan_remains
    }

    /// Returns whether conformance fixtures remain available.
    pub const fn conformance_fixtures_remain(self) -> bool {
        self.conformance_fixtures_remain
    }

    /// Returns whether historical benchmark readers remain available.
    pub const fn historical_benchmark_readers_remain(self) -> bool {
        self.historical_benchmark_readers_remain
    }
}

/// Explicit production serving identities that are retired by this cutover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyPath {
    /// `low_memory` SQLite whole-table vector serving.
    SqliteLowMemoryWholeTableScan,
    /// `resident_hnsw` using `instant-distance`.
    ResidentHnswInstantDistance,
    /// Local search that always runs before a chosen remote backend.
    UnconditionalLocalPreSearch,
}

impl LegacyPath {
    /// All explicit legacy production serving identities.
    pub const ALL: [Self; 3] = [
        Self::SqliteLowMemoryWholeTableScan,
        Self::ResidentHnswInstantDistance,
        Self::UnconditionalLocalPreSearch,
    ];
}

/// Final typed authorization for legacy serving retirement and artifact removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetirementAuthorization {
    candidate_generation: PublicationGeneration,
}

impl RetirementAuthorization {
    /// Returns the promoted DiskANN3 generation that was authorized.
    pub const fn candidate_generation(&self) -> &PublicationGeneration {
        &self.candidate_generation
    }

    /// This authorization includes explicit legacy-artifact removal eligibility.
    pub const fn legacy_artifacts_may_be_removed(&self) -> bool {
        true
    }
}

/// Complete final-retirement boundary inputs.
#[derive(Debug, Clone, Copy)]
pub struct RetirementInputs<'a> {
    /// The authoritative source bound into migration validation.
    pub source: &'a AuthoritativeVectorSource,
    /// The concrete durable cutover manifest.
    pub manifest: &'a CutoverManifest,
    /// Non-forgeable migration validation evidence.
    pub validation: &'a MigrationValidation,
    /// Passed dual-generation shadow evidence.
    pub shadow: &'a ShadowComparison,
    /// Declared rollback-retention window.
    pub rollback_window: RollbackWindow,
    /// Current logical authorization time.
    pub now: u64,
    /// Validated approval for destructive maintenance.
    pub release_policy: &'a ReleasePolicyApproval,
    /// Verified, explicit legacy-artifact disposition.
    pub removal_plan: &'a LegacyArtifactRemovalPlan,
    /// Capabilities retained after retirement.
    pub remaining_capabilities: &'a RemainingServingCapabilities,
}

/// Authorizes retirement only after complete gates and every final boundary input.
pub fn authorize_retirement(
    gates: &CutoverGates,
    inputs: &RetirementInputs<'_>,
) -> LegacyRetirementResult<RetirementAuthorization> {
    inputs
        .validation
        .require_binding(inputs.source, inputs.manifest)?;
    gates.require_complete()?;
    let promotion = inputs
        .shadow
        .bind_promotion(gates, inputs.manifest.candidate_generation())?;
    if promotion.incumbent_generation() != inputs.manifest.incumbent_generation()
        || promotion.candidate_generation() != inputs.manifest.candidate_generation()
    {
        return Err(LegacyRetirementError::contract(
            LegacyRetirementDiagnosticCode::MigrationValidationBindingMismatch,
        ));
    }
    inputs.rollback_window.require_elapsed(inputs.now)?;
    if !inputs.release_policy.allows_legacy_artifact_removal() {
        return Err(LegacyRetirementError::contract(
            LegacyRetirementDiagnosticCode::ReleasePolicyApprovalRequired,
        ));
    }
    Ok(RetirementAuthorization {
        candidate_generation: promotion.candidate_generation,
    })
}
