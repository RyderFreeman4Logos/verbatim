//! Hard acceptance gate evaluation for EVAL-SSD-001.
//!
//! Refs #382. Promotion coupling to #379 is documented in the architecture doc;
//! this module evaluates quality/conformance/memory/budget/latency invariants
//! and can force architecture reconsideration when a reference backend wins.

use serde::{Deserialize, Serialize};

use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};
use super::metrics::{QualityMetricKind, QualityMetrics, QualityStage, ResourceMetrics};
use super::query_matrix::CacheState;
use super::system::{BackendId, BackendRole};

/// Closed gate evaluation verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    /// All hard gates satisfied for the evaluated backends.
    Pass,
    /// At least one hard gate failed (missing measurement, threshold, identity).
    Fail,
    /// A reference backend won the complete gate suite; revisit DiskANN3-first.
    ArchitectureDecisionMustBeReconsidered,
}

impl GateVerdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::ArchitectureDecisionMustBeReconsidered => {
                "architecture_decision_must_be_reconsidered"
            }
        }
    }
}

/// Program-default quality thresholds (aligned with docs/config/acceptance_gates.toml).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QualityGateThresholds {
    pub minimum_final_recall_at_10: f64,
    pub minimum_final_filtered_recall_at_10: f64,
    pub minimum_final_ndcg_at_10: f64,
    pub minimum_candidate_recall_at_10: f64,
}

impl QualityGateThresholds {
    /// Defaults from `docs/config/acceptance_gates.toml` quality section.
    pub const fn program_default() -> Self {
        Self {
            minimum_final_recall_at_10: 0.95,
            minimum_final_filtered_recall_at_10: 0.95,
            minimum_final_ndcg_at_10: 0.90,
            minimum_candidate_recall_at_10: 0.90,
        }
    }
}

/// Online cgroup memory gate (bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryGateThresholds {
    pub memory_high_bytes: u64,
    pub memory_max_bytes: u64,
}

impl MemoryGateThresholds {
    /// Defaults from `docs/config/acceptance_gates.toml` online_memory section.
    pub const fn program_default() -> Self {
        Self {
            memory_high_bytes: 201_326_592,
            memory_max_bytes: 268_435_456,
        }
    }
}

/// Latency gates by cache state (microseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyGateThresholds {
    pub warm_p99_max_micros: u64,
    pub cold_p99_max_micros: u64,
}

impl LatencyGateThresholds {
    /// Defaults from `docs/config/acceptance_gates.toml` latency section.
    pub const fn program_default() -> Self {
        Self {
            warm_p99_max_micros: 75_000,
            cold_p99_max_micros: 250_000,
        }
    }
}

/// Full hard-gate policy for a suite evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HardGatePolicy {
    pub quality: QualityGateThresholds,
    pub memory: MemoryGateThresholds,
    pub latency: LatencyGateThresholds,
    pub require_storage_growth_series: bool,
    pub require_cgroup_memory: bool,
    pub require_cold_and_warm: bool,
}

impl HardGatePolicy {
    /// Program defaults for EVAL-SSD-001 hard gates.
    pub const fn program_default() -> Self {
        Self {
            quality: QualityGateThresholds::program_default(),
            memory: MemoryGateThresholds::program_default(),
            latency: LatencyGateThresholds::program_default(),
            require_storage_growth_series: true,
            require_cgroup_memory: true,
            require_cold_and_warm: true,
        }
    }
}

/// One backend's complete-gate outcome for ranking / reconsideration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendGateOutcome {
    pub backend_id: BackendId,
    pub role: BackendRole,
    pub complete_gate_passed: bool,
}

/// Inputs for evaluating quality gates against one scenario measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScenarioGateInput<'a> {
    pub cache_state: CacheState,
    pub quality: &'a QualityMetrics,
    pub resources: &'a ResourceMetrics,
    pub role: BackendRole,
}

/// Evaluates quality thresholds; returns Fail on missing or under-threshold values.
pub fn evaluate_quality_gates(
    quality: &QualityMetrics,
    thresholds: &QualityGateThresholds,
) -> SsdVectorBenchmarkResult<()> {
    let final_recall = quality
        .get(QualityStage::FinalRescore, QualityMetricKind::RecallAt10)
        .ok_or_else(|| {
            SsdVectorBenchmarkError::contract(SsdVectorBenchmarkDiagnosticCode::MissingMeasurement)
        })?;
    if final_recall < thresholds.minimum_final_recall_at_10 {
        return Err(SsdVectorBenchmarkError::contract(
            SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
        ));
    }

    if let Some(filtered) = quality.get(
        QualityStage::FinalRescore,
        QualityMetricKind::FilteredAuthorizedSubsetRecall,
    ) {
        if filtered < thresholds.minimum_final_filtered_recall_at_10 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
            ));
        }
    }

    if let Some(ndcg) = quality.get(QualityStage::FinalRescore, QualityMetricKind::NdcgAt10) {
        if ndcg < thresholds.minimum_final_ndcg_at_10 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
            ));
        }
    }

    let candidate_recall = quality
        .get(QualityStage::Candidate, QualityMetricKind::RecallAt10)
        .ok_or_else(|| {
            SsdVectorBenchmarkError::contract(SsdVectorBenchmarkDiagnosticCode::MissingMeasurement)
        })?;
    if candidate_recall < thresholds.minimum_candidate_recall_at_10 {
        return Err(SsdVectorBenchmarkError::contract(
            SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
        ));
    }

    Ok(())
}

/// Evaluates resource gates for one scenario (cgroup + latency by cache state).
pub fn evaluate_resource_gates(
    resources: &ResourceMetrics,
    cache_state: CacheState,
    policy: &HardGatePolicy,
) -> SsdVectorBenchmarkResult<()> {
    if policy.require_cgroup_memory {
        resources.cgroup_memory().require_measured()?;
        let peak = resources.cgroup_memory().peak_bytes();
        if peak > policy.memory.memory_max_bytes {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
            ));
        }
    }

    let p99 = resources.latency().p99;
    match cache_state {
        CacheState::Cold => {
            if p99 > policy.latency.cold_p99_max_micros {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
                ));
            }
        }
        CacheState::Warm | CacheState::CacheChurn => {
            if p99 > policy.latency.warm_p99_max_micros {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
                ));
            }
        }
    }

    Ok(())
}

/// Evaluates a single scenario against the hard-gate policy.
pub fn evaluate_scenario_gates(
    input: ScenarioGateInput<'_>,
    policy: &HardGatePolicy,
) -> SsdVectorBenchmarkResult<()> {
    // Regression-only and external controls are measured but do not promote.
    evaluate_quality_gates(input.quality, &policy.quality)?;
    evaluate_resource_gates(input.resources, input.cache_state, policy)?;
    Ok(())
}

/// Aggregate suite evaluation across backend complete-gate outcomes.
///
/// Rules:
/// - any hard fail => Fail
/// - regression-only cannot be the sole promotion winner
/// - if a reference backend has complete_gate_passed and primary candidates do
///   not exclusively win, verdict is ArchitectureDecisionMustBeReconsidered
/// - otherwise Pass when at least one primary candidate complete_gate_passed
pub fn evaluate_suite_verdict(
    outcomes: &[BackendGateOutcome],
) -> SsdVectorBenchmarkResult<GateVerdict> {
    if outcomes.is_empty() {
        return Err(SsdVectorBenchmarkError::contract(
            SsdVectorBenchmarkDiagnosticCode::MissingComponent,
        ));
    }

    let any_failed = outcomes.iter().any(|o| {
        o.role.counts_against_verbatim_process_budget()
            && o.role != BackendRole::RegressionOnly
            && o.role != BackendRole::ExternalControl
            && !o.complete_gate_passed
            && matches!(
                o.role,
                BackendRole::PrimaryCandidate | BackendRole::ExactBaseline | BackendRole::Reference
            )
    });

    // Promotion attempt from regression-only alone.
    let primary_pass = outcomes
        .iter()
        .any(|o| o.role == BackendRole::PrimaryCandidate && o.complete_gate_passed);
    let regression_pass = outcomes
        .iter()
        .any(|o| o.role == BackendRole::RegressionOnly && o.complete_gate_passed);
    let reference_pass = outcomes
        .iter()
        .any(|o| o.role == BackendRole::Reference && o.complete_gate_passed);

    if regression_pass && !primary_pass && !reference_pass {
        return Err(SsdVectorBenchmarkError::contract(
            SsdVectorBenchmarkDiagnosticCode::RegressionOnlyCannotPromote,
        ));
    }

    // Reference complete-gate win forces reconsideration (falsify DiskANN3-first).
    // "Wins" means the reference complete gate passed while primary candidates
    // did not: a joint pass is not a silent architecture overturn.
    if reference_pass && !primary_pass {
        return Ok(GateVerdict::ArchitectureDecisionMustBeReconsidered);
    }

    if any_failed && !primary_pass {
        return Ok(GateVerdict::Fail);
    }

    if primary_pass {
        return Ok(GateVerdict::Pass);
    }

    // No primary pass and no reference win.
    Ok(GateVerdict::Fail)
}

/// Convenience: map a suite verdict into a contract error when not Pass.
pub fn require_pass(verdict: GateVerdict) -> SsdVectorBenchmarkResult<()> {
    match verdict {
        GateVerdict::Pass => Ok(()),
        GateVerdict::Fail => Err(SsdVectorBenchmarkError::contract(
            SsdVectorBenchmarkDiagnosticCode::GateThresholdNotMet,
        )),
        GateVerdict::ArchitectureDecisionMustBeReconsidered => {
            Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::ArchitectureDecisionMustBeReconsidered,
            ))
        }
    }
}
