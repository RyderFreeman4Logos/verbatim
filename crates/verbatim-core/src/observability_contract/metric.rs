//! Low-cardinality metric schemas and cardinality guards.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use super::common::{require_non_empty, validate_schema_version};
use super::OBSERVABILITY_CONTRACT_SCHEMA_VERSION;

/// Default maximum distinct values per metric label key.
pub const DEFAULT_MAX_LABEL_CARDINALITY: u32 = 64;

/// Metric kind for low-cardinality instrumentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Counter,
    Histogram,
    Gauge,
}

/// Privacy review state for a metric label key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelPrivacy {
    /// Approved low-cardinality operational label (stage, status class, …).
    Approved,
    /// Forbidden as a metric label (query text, evidence id, path, token, …).
    Prohibited,
}

/// One privacy-reviewed label key on a metric.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MetricLabelSpec {
    pub key: String,
    pub privacy: LabelPrivacy,
    /// Soft cardinality budget for this key (enforced by [`CardinalityGuard`]).
    pub max_cardinality: u32,
}

impl MetricLabelSpec {
    pub fn approved(key: impl Into<String>, max_cardinality: u32) -> Result<Self> {
        let key = key.into();
        require_non_empty("metric label key", &key)?;
        if max_cardinality == 0 {
            bail!("metric label max_cardinality must be >= 1");
        }
        Ok(Self {
            key,
            privacy: LabelPrivacy::Approved,
            max_cardinality,
        })
    }

    pub fn prohibited(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        require_non_empty("metric label key", &key)?;
        Ok(Self {
            key,
            privacy: LabelPrivacy::Prohibited,
            max_cardinality: 0,
        })
    }
}

/// Low-cardinality metric specification (schema only; not a live exporter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricSpec {
    pub schema_version: u32,
    pub name: String,
    pub kind: MetricKind,
    pub description: String,
    /// Unit string (`ms`, `1`, `bytes`, …); empty allowed for unitless counters.
    pub unit: String,
    pub labels: Vec<MetricLabelSpec>,
}

impl MetricSpec {
    pub fn new(
        name: impl Into<String>,
        kind: MetricKind,
        description: impl Into<String>,
        unit: impl Into<String>,
        labels: Vec<MetricLabelSpec>,
    ) -> Result<Self> {
        let spec = Self {
            schema_version: OBSERVABILITY_CONTRACT_SCHEMA_VERSION,
            name: name.into(),
            kind,
            description: description.into(),
            unit: unit.into(),
            labels,
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_schema()?;
        require_non_empty("metric name", &self.name)?;
        require_non_empty("metric description", &self.description)?;
        let mut seen = BTreeSet::new();
        for label in &self.labels {
            require_non_empty("metric label key", &label.key)?;
            if !seen.insert(label.key.clone()) {
                bail!("duplicate metric label key: {}", label.key);
            }
            if matches!(label.privacy, LabelPrivacy::Approved) && label.max_cardinality == 0 {
                bail!(
                    "approved label {} must declare max_cardinality >= 1",
                    label.key
                );
            }
        }
        Ok(())
    }

    /// Reject sample label maps that use prohibited keys or unknown keys.
    pub fn validate_sample_labels(&self, sample: &BTreeMap<String, String>) -> Result<()> {
        self.validate()?;
        let allowed: BTreeMap<&str, &MetricLabelSpec> =
            self.labels.iter().map(|l| (l.key.as_str(), l)).collect();
        for key in sample.keys() {
            let Some(spec) = allowed.get(key.as_str()) else {
                bail!("metric sample uses undeclared label key: {key}");
            };
            if matches!(spec.privacy, LabelPrivacy::Prohibited) {
                bail!("metric sample uses prohibited label key: {key}");
            }
        }
        Ok(())
    }
}

/// Enforces per-label cardinality budgets for metric samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardinalityGuard {
    pub schema_version: u32,
    /// Observed distinct values per label key.
    observed: BTreeMap<String, BTreeSet<String>>,
    /// Per-key budgets; missing keys use [`DEFAULT_MAX_LABEL_CARDINALITY`].
    budgets: BTreeMap<String, u32>,
    default_budget: u32,
}

impl CardinalityGuard {
    pub fn new(default_budget: u32) -> Result<Self> {
        if default_budget == 0 {
            bail!("cardinality default_budget must be >= 1");
        }
        Ok(Self {
            schema_version: OBSERVABILITY_CONTRACT_SCHEMA_VERSION,
            observed: BTreeMap::new(),
            budgets: BTreeMap::new(),
            default_budget,
        })
    }

    pub fn with_default_budget() -> Self {
        Self::new(DEFAULT_MAX_LABEL_CARDINALITY).expect("default budget is non-zero")
    }

    pub fn set_budget(&mut self, key: impl Into<String>, max: u32) -> Result<()> {
        if max == 0 {
            bail!("cardinality budget must be >= 1");
        }
        self.budgets.insert(key.into(), max);
        Ok(())
    }

    pub fn budget_for(&self, key: &str) -> u32 {
        self.budgets
            .get(key)
            .copied()
            .unwrap_or(self.default_budget)
    }

    pub fn distinct_count(&self, key: &str) -> usize {
        self.observed.get(key).map(|s| s.len()).unwrap_or(0)
    }

    /// Record a label value. Returns `Ok(true)` when newly seen, `Ok(false)`
    /// when already known, and `Err` when accepting it would exceed the budget.
    pub fn observe(&mut self, key: &str, value: &str) -> Result<bool> {
        require_non_empty("cardinality label key", key)?;
        let budget = self.budget_for(key) as usize;
        let set = self.observed.entry(key.to_string()).or_default();
        if set.contains(value) {
            return Ok(false);
        }
        if set.len() >= budget {
            bail!("label cardinality exceeded for '{key}': budget {budget}, refusing new value");
        }
        set.insert(value.to_string());
        Ok(true)
    }

    /// Observe every label in a sample map; fail closed if any key overflows.
    pub fn observe_labels(&mut self, labels: &BTreeMap<String, String>) -> Result<()> {
        for (key, value) in labels {
            self.observe(key, value)?;
        }
        Ok(())
    }
}

/// Decode JSON — fail closed on unknown schema versions.
pub fn decode_metric_spec_json(bytes: &[u8]) -> Result<MetricSpec> {
    let metric: MetricSpec = serde_json::from_slice(bytes)?;
    metric.validate()?;
    Ok(metric)
}
