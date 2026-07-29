//! Lexical conformance/qrel gate contract (Refs #380).
//!
//! A backend BM25 change (Tantivy upgrade, tokenizer swap, field-scoring
//! change, or migration to Qdrant/LanceDB FTS) must pass the same lexical
//! conformance/qrel suite before publication. This module defines the gate
//! contract types — there is no live qrel dataset or evaluation runner here.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::{LexicalEngineDiagnosticCode, LexicalEngineError, LexicalEngineResult};

/// Maximum number of qrel cases in a conformance suite.
pub const MAX_QREL_CASES: usize = 10_000;

/// Stable identifier for a conformance suite version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConformanceSuiteId {
    /// Closed suite name (e.g. `lexical-v1`).
    name: SuiteName,
    /// Suite version, bumped when qrels or thresholds change.
    version: u32,
}

impl ConformanceSuiteId {
    /// Constructs a conformance suite identifier.
    pub fn new(name: impl Into<String>, version: u32) -> LexicalEngineResult<Self> {
        let name = SuiteName::new(name.into())?;
        if version == 0 {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self { name, version })
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub const fn version(&self) -> u32 {
        self.version
    }
}

/// Validated conformance suite name: non-empty, closed character set, bounded.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SuiteName(String);

impl SuiteName {
    pub fn new(value: String) -> LexicalEngineResult<Self> {
        if value.is_empty() || value.len() > 64 {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidBounds,
            ));
        }
        let valid = value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !valid {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldName,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SuiteName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Suite names are closed labels, safe to render.
        write!(formatter, "SuiteName({})", self.0)
    }
}

impl<'de> Deserialize<'de> for SuiteName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Threshold contract for a conformance metric (e.g. nDCG@10, recall@100).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConformanceThreshold {
    metric: ConformanceMetric,
    /// Minimum acceptable value (inclusive).
    minimum: f64,
}

impl ConformanceThreshold {
    /// Constructs a threshold. `minimum` must be finite and in [0.0, 1.0].
    pub fn new(metric: ConformanceMetric, minimum: f64) -> LexicalEngineResult<Self> {
        if !minimum.is_finite() || !(0.0..=1.0).contains(&minimum) {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self { metric, minimum })
    }

    pub const fn metric(&self) -> ConformanceMetric {
        self.metric
    }

    pub const fn minimum(&self) -> f64 {
        self.minimum
    }

    /// Returns `true` if the observed value meets or exceeds the threshold.
    pub fn is_satisfied(self, observed: f64) -> bool {
        observed.is_finite() && observed >= self.minimum
    }
}

/// Closed set of conformance metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceMetric {
    /// Normalized Discounted Cumulative Gain at K.
    NdcgAtK,
    /// Recall at K.
    RecallAtK,
    /// Precision at K.
    PrecisionAtK,
    /// Mean Reciprocal Rank.
    Mrr,
    /// Mean Average Precision.
    Map,
}

impl ConformanceMetric {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NdcgAtK => "ndcg_at_k",
            Self::RecallAtK => "recall_at_k",
            Self::PrecisionAtK => "precision_at_k",
            Self::Mrr => "mrr",
            Self::Map => "map",
        }
    }

    /// Returns `true` if higher values are better for this metric.
    pub const fn higher_is_better(self) -> bool {
        matches!(
            self,
            Self::NdcgAtK | Self::RecallAtK | Self::PrecisionAtK | Self::Mrr | Self::Map
        )
    }
}

/// A lexical conformance gate: suite id + thresholds + qrel case count.
///
/// This is the pass/fail contract that a backend BM25 change must satisfy
/// before publication or migration cutover.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalConformanceGate {
    suite: ConformanceSuiteId,
    thresholds: Vec<ConformanceThreshold>,
    qrel_case_count: usize,
}

impl LexicalConformanceGate {
    /// Constructs a conformance gate.
    pub fn new(
        suite: ConformanceSuiteId,
        thresholds: Vec<ConformanceThreshold>,
        qrel_case_count: usize,
    ) -> LexicalEngineResult<Self> {
        if thresholds.is_empty() {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::MissingComponent,
            ));
        }
        if qrel_case_count == 0 || qrel_case_count > MAX_QREL_CASES {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self {
            suite,
            thresholds,
            qrel_case_count,
        })
    }

    pub fn suite(&self) -> &ConformanceSuiteId {
        &self.suite
    }

    pub fn thresholds(&self) -> &[ConformanceThreshold] {
        &self.thresholds
    }

    pub const fn qrel_case_count(&self) -> usize {
        self.qrel_case_count
    }

    /// Evaluates whether a set of observed metric values satisfies all
    /// thresholds. Every declared threshold must have a matching observation.
    pub fn evaluate(&self, observations: &ConformanceObservations) -> LexicalEngineResult<()> {
        for threshold in &self.thresholds {
            let observed = observations.get(threshold.metric).ok_or_else(|| {
                LexicalEngineError::contract(LexicalEngineDiagnosticCode::MissingComponent)
            })?;
            if !threshold.is_satisfied(observed) {
                return Err(LexicalEngineError::contract(
                    LexicalEngineDiagnosticCode::ConformanceGateFailed,
                ));
            }
        }
        Ok(())
    }
}

/// Observed conformance metric values from a candidate backend run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConformanceObservations {
    values: Vec<(ConformanceMetric, f64)>,
}

impl ConformanceObservations {
    /// Constructs an empty observation set.
    pub fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Records an observed metric value. The value must be finite and in
    /// [0.0, 1.0].
    pub fn record(mut self, metric: ConformanceMetric, value: f64) -> LexicalEngineResult<Self> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidBounds,
            ));
        }
        self.values.push((metric, value));
        Ok(self)
    }

    /// Returns the observed value for a metric, if present.
    pub fn get(&self, metric: ConformanceMetric) -> Option<f64> {
        self.values
            .iter()
            .find(|(m, _)| *m == metric)
            .map(|(_, v)| *v)
    }

    /// Returns the number of recorded observations.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite() -> ConformanceSuiteId {
        ConformanceSuiteId::new("lexical-v1", 1).unwrap()
    }

    #[test]
    fn suite_id_rejects_zero_version() {
        assert!(ConformanceSuiteId::new("lexical-v1", 0).is_err());
    }

    #[test]
    fn suite_id_rejects_invalid_name() {
        assert!(ConformanceSuiteId::new("", 1).is_err());
        assert!(ConformanceSuiteId::new("1bad", 1).is_err());
        assert!(ConformanceSuiteId::new("bad name", 1).is_err());
    }

    #[test]
    fn threshold_rejects_out_of_range() {
        assert!(ConformanceThreshold::new(ConformanceMetric::NdcgAtK, -0.1).is_err());
        assert!(ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 1.1).is_err());
        assert!(ConformanceThreshold::new(ConformanceMetric::NdcgAtK, f64::NAN).is_err());
    }

    #[test]
    fn threshold_boundary_values() {
        assert!(ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.0).is_ok());
        assert!(ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 1.0).is_ok());
    }

    #[test]
    fn gate_rejects_empty_thresholds() {
        let err = LexicalConformanceGate::new(suite(), vec![], 100).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::MissingComponent
        );
    }

    #[test]
    fn gate_rejects_zero_qrel_cases() {
        let t = ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.5).unwrap();
        let err = LexicalConformanceGate::new(suite(), vec![t], 0).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidBounds
        );
    }

    #[test]
    fn gate_evaluate_passes_when_all_thresholds_met() {
        let thresholds = vec![
            ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.5).unwrap(),
            ConformanceThreshold::new(ConformanceMetric::RecallAtK, 0.7).unwrap(),
        ];
        let gate = LexicalConformanceGate::new(suite(), thresholds, 100).unwrap();
        let obs = ConformanceObservations::new()
            .record(ConformanceMetric::NdcgAtK, 0.6)
            .unwrap()
            .record(ConformanceMetric::RecallAtK, 0.8)
            .unwrap();
        assert!(gate.evaluate(&obs).is_ok());
    }

    #[test]
    fn gate_evaluate_fails_when_threshold_missed() {
        let thresholds = vec![ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.5).unwrap()];
        let gate = LexicalConformanceGate::new(suite(), thresholds, 100).unwrap();
        let obs = ConformanceObservations::new()
            .record(ConformanceMetric::NdcgAtK, 0.4)
            .unwrap();
        let err = gate.evaluate(&obs).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::ConformanceGateFailed
        );
    }

    #[test]
    fn gate_evaluate_fails_when_observation_missing() {
        let thresholds = vec![ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.5).unwrap()];
        let gate = LexicalConformanceGate::new(suite(), thresholds, 100).unwrap();
        let obs = ConformanceObservations::new()
            .record(ConformanceMetric::RecallAtK, 0.9)
            .unwrap();
        let err = gate.evaluate(&obs).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::MissingComponent
        );
    }

    #[test]
    fn observations_reject_out_of_range() {
        let err = ConformanceObservations::new()
            .record(ConformanceMetric::NdcgAtK, 1.5)
            .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidBounds
        );
    }
}
