//! Metric kernels, vector validation, and raw-distance / normalized-score separation.
//!
//! This is a **portable scalar reference implementation** — the scalar fallback
//! and independent validation kernel described in issue #376. No SIMD dispatch,
//! no architecture-specific intrinsics. The reference distances computed here
//! are shared by production exact scan and by the offline ground-truth path.
//! Tests additionally provide an *independent* reference calculation to catch
//! bugs in this primary reference.

use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// The immutable original-vector dimension for the target profile.
pub const EXACT_VECTOR_DIMENSION: usize = 4_096;

/// Maximum tolerated absolute error around unit cosine normalization.
pub const COSINE_UNIT_LENGTH_TOLERANCE: f64 = 1.0e-4;

/// Similarity metric whose normalization rule is fixed at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExactMetric {
    /// Unit-length vectors compared by cosine similarity.
    Cosine,
    /// Dot-product vectors whose original magnitudes remain meaningful.
    Dot,
    /// Euclidean-distance vectors whose original magnitudes remain meaningful.
    L2,
}

/// Normalization behavior enforced before a vector reaches the scan engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorNormalization {
    /// Vectors must already have unit L2 norm.
    UnitL2,
    /// The scan engine preserves the original vector magnitude.
    PreserveMagnitude,
}

impl ExactMetric {
    /// Returns the only normalization behavior permitted for this metric.
    pub const fn normalization(self) -> VectorNormalization {
        match self {
            Self::Cosine => VectorNormalization::UnitL2,
            Self::Dot | Self::L2 => VectorNormalization::PreserveMagnitude,
        }
    }

    /// Returns the fixed full-precision `f32` dimension.
    pub const fn dimension(self) -> usize {
        EXACT_VECTOR_DIMENSION
    }

    /// Rejects wrong-dimensional, non-finite, zero, and wrongly normalized vectors.
    ///
    /// All vectors that enter the exact-scan pipeline — query vectors,
    /// candidate originals, and ground-truth samples — must pass this check.
    pub fn validate_vector(self, vector: &[f32]) -> ExactScanResult<()> {
        if vector.len() != EXACT_VECTOR_DIMENSION {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::VectorDimensionMismatch,
            ));
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::NonFiniteVector,
            ));
        }
        if vector
            .iter()
            .all(|value| value.to_bits() & 0x7fff_ffff == 0)
        {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::ZeroVector,
            ));
        }
        if self.normalization() == VectorNormalization::UnitL2 {
            let norm = vector
                .iter()
                .map(|value| f64::from(*value).powi(2))
                .sum::<f64>()
                .sqrt();
            if (norm - 1.0).abs() > COSINE_UNIT_LENGTH_TOLERANCE {
                return Err(ExactScanError::contract(
                    ExactScanDiagnosticCode::MetricNormalizationMismatch,
                ));
            }
        }
        Ok(())
    }
}

/// Metric-labelled raw distance plus a metric-native normalized score.
///
/// The raw distance is the value used for ranking (smaller = closer). The
/// normalized score is the metric-native similarity (higher = closer) for
/// reporting and gating. They are deliberately separate so that a reporting
/// score can never be mistaken for a ranking distance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricScore {
    metric: ExactMetric,
    raw_distance: f32,
    normalized_score: f32,
}

impl MetricScore {
    /// Builds a validated score, separating raw distance from normalized score.
    ///
    /// Rejects non-finite values. For L2 the raw distance must be non-negative.
    pub fn new(
        metric: ExactMetric,
        raw_distance: f32,
        normalized_score: f32,
    ) -> ExactScanResult<Self> {
        if !raw_distance.is_finite() || !normalized_score.is_finite() {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidDistance,
            ));
        }
        if metric == ExactMetric::L2 && raw_distance < 0.0 {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidDistance,
            ));
        }
        Ok(Self {
            metric,
            raw_distance,
            normalized_score,
        })
    }

    /// Returns the metric that gives meaning to `raw_distance`.
    pub const fn metric(self) -> ExactMetric {
        self.metric
    }

    /// Returns the untransformed ranking distance (smaller = closer).
    pub const fn raw_distance(self) -> f32 {
        self.raw_distance
    }

    /// Returns the metric-native similarity score (higher = closer).
    pub const fn normalized_score(self) -> f32 {
        self.normalized_score
    }
}

/// Portable scalar reference distance between two validated vectors.
///
/// This is the **scalar fallback** and **shared reference kernel**. It computes
/// the exact full-precision distance for the declared metric. Production and
/// ground-truth paths share this kernel; an independent cross-check lives in the
/// test module to catch bugs.
///
/// Both vectors must already have been validated by [`ExactMetric::validate_vector`].
pub fn reference_distance(
    metric: ExactMetric,
    a: &[f32],
    b: &[f32],
) -> ExactScanResult<MetricScore> {
    if a.len() != EXACT_VECTOR_DIMENSION || b.len() != EXACT_VECTOR_DIMENSION {
        return Err(ExactScanError::contract(
            ExactScanDiagnosticCode::VectorDimensionMismatch,
        ));
    }
    let score = match metric {
        ExactMetric::L2 => {
            let mut sum_sq = 0.0_f64;
            for (va, vb) in a.iter().zip(b.iter()) {
                let diff = f64::from(va - vb);
                sum_sq += diff * diff;
            }
            let dist = sum_sq.sqrt() as f32;
            let normalized = (1.0_f64 / (1.0 + f64::from(dist))) as f32;
            MetricScore::new(metric, dist, normalized)?
        }
        ExactMetric::Cosine => {
            let mut dot = 0.0_f64;
            let mut norm_a_sq = 0.0_f64;
            let mut norm_b_sq = 0.0_f64;
            for (va, vb) in a.iter().zip(b.iter()) {
                let va = f64::from(*va);
                let vb = f64::from(*vb);
                dot += va * vb;
                norm_a_sq += va * va;
                norm_b_sq += vb * vb;
            }
            let denom = norm_a_sq.sqrt() * norm_b_sq.sqrt();
            if denom == 0.0 {
                return Err(ExactScanError::contract(
                    ExactScanDiagnosticCode::ZeroVector,
                ));
            }
            let cosine_sim = (dot / denom) as f32;
            MetricScore::new(metric, 1.0 - cosine_sim, cosine_sim)?
        }
        ExactMetric::Dot => {
            let mut dot = 0.0_f64;
            for (va, vb) in a.iter().zip(b.iter()) {
                dot += f64::from(*va) * f64::from(*vb);
            }
            let dot_f = dot as f32;
            MetricScore::new(metric, -dot_f, dot_f)?
        }
    };
    Ok(score)
}
