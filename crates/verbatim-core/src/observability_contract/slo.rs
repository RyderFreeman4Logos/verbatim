//! SLO definitions and error-budget accounting.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::common::{require_non_empty, validate_schema_version};
use super::OBSERVABILITY_CONTRACT_SCHEMA_VERSION;

/// Failure domain for SLO burn classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SloFailureDomain {
    Provider,
    Queue,
    Storage,
    Cache,
    Application,
    Unknown,
}

/// Latency target for an SLO (percentile + threshold).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LatencyTarget {
    /// Percentile in (0, 100], e.g. 99 for p99.
    pub percentile: u8,
    /// Maximum allowed latency at that percentile.
    pub max_latency_ms: u64,
}

impl LatencyTarget {
    pub fn new(percentile: u8, max_latency_ms: u64) -> Result<Self> {
        let target = Self {
            percentile,
            max_latency_ms,
        };
        target.validate()?;
        Ok(target)
    }

    /// Fail closed when percentile is outside 1..=100 or max_latency_ms is zero.
    pub fn validate(&self) -> Result<()> {
        if self.percentile == 0 || self.percentile > 100 {
            bail!("latency percentile must be in 1..=100");
        }
        if self.max_latency_ms == 0 {
            bail!("max_latency_ms must be >= 1");
        }
        Ok(())
    }
}

/// Inputs for [`SloDefinition::new`].
#[derive(Debug, Clone)]
pub struct SloDefinitionParams {
    pub name: String,
    pub description: String,
    /// Success-rate target in (0.0, 1.0], e.g. 0.999 for 99.9%.
    pub success_ratio_target: f64,
    pub latency: LatencyTarget,
    /// Window length for budget accounting (seconds).
    pub window_secs: u64,
    /// Trace sampling ratio in (0.0, 1.0].
    pub sampling_ratio: f64,
    /// Retention for raw telemetry used by this SLO (seconds).
    pub retention_secs: u64,
    /// Domains this SLO attributes failures to (for burn classification).
    pub failure_domains: Vec<SloFailureDomain>,
}

/// Service-level objective with error budget and sampling/retention hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloDefinition {
    pub schema_version: u32,
    pub name: String,
    pub description: String,
    /// Success-rate target in (0.0, 1.0], e.g. 0.999 for 99.9%.
    pub success_ratio_target: f64,
    pub latency: LatencyTarget,
    /// Window length for budget accounting (seconds).
    pub window_secs: u64,
    /// Trace sampling ratio in (0.0, 1.0].
    pub sampling_ratio: f64,
    /// Retention for raw telemetry used by this SLO (seconds).
    pub retention_secs: u64,
    /// Domains this SLO attributes failures to (for burn classification).
    pub failure_domains: Vec<SloFailureDomain>,
}

impl SloDefinition {
    pub fn new(params: SloDefinitionParams) -> Result<Self> {
        let slo = Self {
            schema_version: OBSERVABILITY_CONTRACT_SCHEMA_VERSION,
            name: params.name,
            description: params.description,
            success_ratio_target: params.success_ratio_target,
            latency: params.latency,
            window_secs: params.window_secs,
            sampling_ratio: params.sampling_ratio,
            retention_secs: params.retention_secs,
            failure_domains: params.failure_domains,
        };
        slo.validate()?;
        Ok(slo)
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_schema()?;
        require_non_empty("SLO name", &self.name)?;
        require_non_empty("SLO description", &self.description)?;
        if !(self.success_ratio_target > 0.0 && self.success_ratio_target <= 1.0) {
            bail!(
                "success_ratio_target must be in (0, 1], got {}",
                self.success_ratio_target
            );
        }
        self.latency.validate()?;
        if self.window_secs == 0 {
            bail!("SLO window_secs must be >= 1");
        }
        if !(self.sampling_ratio > 0.0 && self.sampling_ratio <= 1.0) {
            bail!(
                "sampling_ratio must be in (0, 1], got {}",
                self.sampling_ratio
            );
        }
        if self.retention_secs == 0 {
            bail!("retention_secs must be >= 1");
        }
        if self.failure_domains.is_empty() {
            bail!("SLO failure_domains must not be empty");
        }
        Ok(())
    }

    /// Error budget as allowed failure ratio: `1 - success_ratio_target`.
    pub fn error_budget_ratio(&self) -> f64 {
        1.0 - self.success_ratio_target
    }

    /// Allowed failures in a window given total events (floor of budget * total).
    pub fn allowed_failures(&self, total_events: u64) -> u64 {
        if total_events == 0 {
            return 0;
        }
        // Floor so we never over-claim budget; zero budget yields zero.
        ((total_events as f64) * self.error_budget_ratio()).floor() as u64
    }

    /// Remaining budget after `failed` failures in `total` events.
    ///
    /// Over-budget remaining is reported as `0` with `burned_out = true`.
    pub fn budget_status(
        &self,
        total_events: u64,
        failed_events: u64,
    ) -> Result<ErrorBudgetStatus> {
        if failed_events > total_events {
            bail!("failed_events ({failed_events}) exceeds total_events ({total_events})");
        }
        let allowed = self.allowed_failures(total_events);
        Ok(ErrorBudgetStatus {
            total_events,
            failed_events,
            allowed_failures: allowed,
            remaining_failures: allowed.saturating_sub(failed_events),
            burned_out: failed_events > allowed,
        })
    }
}

/// Snapshot of error-budget consumption for an SLO window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ErrorBudgetStatus {
    pub total_events: u64,
    pub failed_events: u64,
    pub allowed_failures: u64,
    pub remaining_failures: u64,
    pub burned_out: bool,
}

/// Decode JSON — fail closed on unknown schema versions.
pub fn decode_slo_definition_json(bytes: &[u8]) -> Result<SloDefinition> {
    let slo: SloDefinition = serde_json::from_slice(bytes)?;
    slo.validate()?;
    Ok(slo)
}
