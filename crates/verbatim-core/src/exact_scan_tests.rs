//! Focused unit tests for the exact-scan contract module (issue #376).
//!
//! These tests cover:
//! - Golden cosine/dot/L2 fixtures (normalized and non-normalized)
//! - Zero / NaN / Inf / wrong-length vector rejection
//! - Raw distance vs normalized score separation
//! - Budget bounds and typed exhaustion
//! - Candidate recall gating (candidate recall@K vs final recall@K)
//! - Exact ground-truth exhaustive path
//! - Quality policy: scoped-exact claims, never global
//! - Crossover strategy selection by measured thresholds
//! - Diagnostic-code-only error rendering (redaction)
//!
//! The metric golden tests include an **independent** reference calculation
//! (built from first principles, not reusing the production kernel) to catch
//! bugs in `reference_distance`.

use crate::exact_scan::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds a unit-norm 4096-dim vector with the first `n` components set to
/// `1/sqrt(n)` and the rest zero. Produces a valid cosine vector.
fn unit_vector(n: usize) -> Vec<f32> {
    assert!(n > 0 && n <= 4096);
    let mut v = vec![0.0_f32; 4096];
    let val = 1.0 / (n as f32).sqrt();
    for i in 0..n {
        v[i] = val;
    }
    v
}

/// Builds a non-normalized 4096-dim vector with the first `n` components set to `val`.
fn raw_vector(n: usize, val: f32) -> Vec<f32> {
    assert!(n > 0 && n <= 4096);
    let mut v = vec![0.0_f32; 4096];
    for i in 0..n {
        v[i] = val;
    }
    v
}

/// Independent reference cosine similarity (first-principles, f64 accumulator).
fn independent_cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for i in 0..a.len() {
        let va = f64::from(a[i]);
        let vb = f64::from(b[i]);
        dot += va * vb;
        na += va * va;
        nb += vb * vb;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

/// Independent reference L2 distance (first-principles, f64 accumulator).
fn independent_l2(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0_f64;
    for i in 0..a.len() {
        let d = f64::from(a[i] - b[i]);
        sum += d * d;
    }
    sum.sqrt() as f32
}

/// Independent reference dot product (first-principles, f64 accumulator).
fn independent_dot(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0_f64;
    for i in 0..a.len() {
        dot += f64::from(a[i]) * f64::from(b[i]);
    }
    dot as f32
}

// ===========================================================================
// METRIC GOLDEN FIXTURES — normalized and non-normalized
// ===========================================================================

#[test]
fn golden_cosine_identical_normalized_vectors_distance_zero() {
    let a = unit_vector(10);
    let b = unit_vector(10);
    let score = reference_distance(ExactMetric::Cosine, &a, &b).unwrap();
    let expected_sim = independent_cosine(&a, &b);
    assert!((score.normalized_score() - expected_sim).abs() < 1e-4);
    assert!((score.raw_distance() - (1.0 - expected_sim)).abs() < 1e-4);
    // Identical unit vectors: cosine sim = 1.0, distance = 0.0
    assert!((score.normalized_score() - 1.0).abs() < 1e-4);
    assert!(score.raw_distance().abs() < 1e-4);
}

#[test]
fn golden_cosine_orthogonal_normalized_vectors() {
    // Two orthogonal unit vectors: first 10 dims set in a, dims 10-19 set in b
    let a = unit_vector(10);
    let mut b = vec![0.0_f32; 4096];
    let val = 1.0 / 10.0_f32.sqrt();
    for i in 10..20 {
        b[i] = val;
    }
    let score = reference_distance(ExactMetric::Cosine, &a, &b).unwrap();
    let expected_sim = independent_cosine(&a, &b);
    assert!((score.normalized_score() - expected_sim).abs() < 1e-4);
    // Orthogonal: cosine sim = 0.0, distance = 1.0
    assert!(score.normalized_score().abs() < 1e-4);
    assert!((score.raw_distance() - 1.0).abs() < 1e-4);
}

#[test]
fn golden_dot_product_non_normalized_vectors() {
    // Non-normalized vectors: first 5 dims = 2.0
    let a = raw_vector(5, 2.0);
    let b = raw_vector(5, 2.0);
    let score = reference_distance(ExactMetric::Dot, &a, &b).unwrap();
    let expected_dot = independent_dot(&a, &b);
    assert!((score.normalized_score() - expected_dot).abs() < 1e-3);
    // dot = 5 * (2*2) = 20.0; raw_distance = -20.0 (lower = closer)
    assert!((score.normalized_score() - 20.0).abs() < 1e-3);
    assert!((score.raw_distance() - (-20.0)).abs() < 1e-3);
}

#[test]
fn golden_l2_distance_non_normalized_vectors() {
    let a = raw_vector(3, 1.0);
    let b = raw_vector(3, 4.0);
    let score = reference_distance(ExactMetric::L2, &a, &b).unwrap();
    let expected = independent_l2(&a, &b);
    assert!((score.raw_distance() - expected).abs() < 1e-3);
    // 3 dims, diff=3 each: sqrt(3 * 9) = sqrt(27) ≈ 5.196
    assert!((score.raw_distance() - 27.0_f32.sqrt()).abs() < 1e-3);
}

#[test]
fn golden_l2_identical_vectors_distance_zero() {
    let a = unit_vector(10);
    let score = reference_distance(ExactMetric::L2, &a, &a).unwrap();
    assert!(score.raw_distance().abs() < 1e-4);
    assert!(score.normalized_score() > 0.0);
}

#[test]
fn golden_metric_independent_cross_check_matches_kernel() {
    // Cross-check the production kernel against independent reference for all 3 metrics
    let a = unit_vector(7);
    let b = unit_vector(11);

    let cosine = reference_distance(ExactMetric::Cosine, &a, &b).unwrap();
    assert!((cosine.normalized_score() - independent_cosine(&a, &b)).abs() < 1e-4);

    let dot = reference_distance(ExactMetric::Dot, &a, &b).unwrap();
    assert!((dot.normalized_score() - independent_dot(&a, &b)).abs() < 1e-3);

    let l2 = reference_distance(ExactMetric::L2, &a, &b).unwrap();
    assert!((l2.raw_distance() - independent_l2(&a, &b)).abs() < 1e-3);
}

// ===========================================================================
// VECTOR VALIDATION — zero / NaN / Inf / wrong-length
// ===========================================================================

#[test]
fn validate_rejects_wrong_length() {
    let short = vec![1.0; 100];
    assert!(ExactMetric::Cosine.validate_vector(&short).is_err());
    assert!(ExactMetric::Dot.validate_vector(&short).is_err());
    assert!(ExactMetric::L2.validate_vector(&short).is_err());
}

#[test]
fn validate_rejects_zero_vector() {
    let zero = vec![0.0_f32; 4096];
    assert!(ExactMetric::Cosine.validate_vector(&zero).is_err());
    assert!(ExactMetric::Dot.validate_vector(&zero).is_err());
    assert!(ExactMetric::L2.validate_vector(&zero).is_err());
}

#[test]
fn validate_rejects_nan_vector() {
    let mut v = unit_vector(5);
    v[100] = f32::NAN;
    assert!(ExactMetric::Cosine.validate_vector(&v).is_err());
    assert!(ExactMetric::Dot.validate_vector(&v).is_err());
    assert!(ExactMetric::L2.validate_vector(&v).is_err());
}

#[test]
fn validate_rejects_infinity_vector() {
    let mut v = unit_vector(5);
    v[100] = f32::INFINITY;
    assert!(ExactMetric::Cosine.validate_vector(&v).is_err());
    assert!(ExactMetric::Dot.validate_vector(&v).is_err());
    assert!(ExactMetric::L2.validate_vector(&v).is_err());
}

#[test]
fn validate_rejects_neg_infinity_vector() {
    let mut v = unit_vector(5);
    v[100] = f32::NEG_INFINITY;
    assert!(ExactMetric::L2.validate_vector(&v).is_err());
}

#[test]
fn validate_cosine_rejects_non_unit_norm() {
    // Magnitude is not 1.0 — cosine requires unit L2 norm
    let v = raw_vector(5, 2.0);
    assert!(ExactMetric::Cosine.validate_vector(&v).is_err());
}

#[test]
fn validate_dot_and_l2_accept_non_unit_norm() {
    let v = raw_vector(5, 2.0);
    assert!(ExactMetric::Dot.validate_vector(&v).is_ok());
    assert!(ExactMetric::L2.validate_vector(&v).is_ok());
}

#[test]
fn validate_cosine_accepts_unit_norm() {
    let v = unit_vector(10);
    assert!(ExactMetric::Cosine.validate_vector(&v).is_ok());
}

// ===========================================================================
// RAW DISTANCE vs NORMALIZED SCORE SEPARATION
// ===========================================================================

#[test]
fn raw_distance_and_normalized_score_are_separate_fields() {
    let a = unit_vector(5);
    let b = unit_vector(5);
    let score = reference_distance(ExactMetric::Cosine, &a, &b).unwrap();
    // raw_distance = 1 - cos_sim; normalized_score = cos_sim
    assert!((score.raw_distance() + score.normalized_score() - 1.0).abs() < 1e-4);
}

#[test]
fn l2_score_rejects_negative_raw_distance() {
    assert!(MetricScore::new(ExactMetric::L2, -1.0, 0.5).is_err());
    assert!(MetricScore::new(ExactMetric::L2, 0.0, 0.5).is_ok());
}

#[test]
fn metric_score_rejects_non_finite_values() {
    assert!(MetricScore::new(ExactMetric::Cosine, f32::NAN, 0.5).is_err());
    assert!(MetricScore::new(ExactMetric::Cosine, 0.5, f32::INFINITY).is_err());
}

// ===========================================================================
// BUDGET BOUNDS AND TYPED EXHAUSTION
// ===========================================================================

#[test]
fn budget_skeleton_default_is_valid() {
    let b = RescoringBudget::skeleton_default();
    assert!(b.validate().is_ok());
    assert!(b.top_k > 0);
    assert!(b.candidate_cap > 0);
    assert!(b.io_batch_size > 0);
}

#[test]
fn budget_exhaustion_all_variants_have_stable_codes() {
    for variant in BudgetExhaustion::ALL {
        assert!(!variant.as_str().is_empty());
    }
}

#[test]
fn budget_deserialize_validates() {
    let json = r#"{"top_k":0,"candidate_cap":10,"io_batch_size":5}"#;
    let result: Result<RescoringBudget, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

#[test]
fn budget_deserialize_accepts_valid() {
    let json = r#"{"top_k":10,"candidate_cap":100,"io_batch_size":32}"#;
    let budget: RescoringBudget = serde_json::from_str(json).unwrap();
    assert_eq!(budget.top_k, 10);
}

// ===========================================================================
// CANDIDATE RECALL GATING
// ===========================================================================

#[test]
fn recall_rescoring_cannot_recover_missing_neighbor() {
    // True top-3 = [10, 20, 30]
    // Candidate pool (from ANN) = [10, 20, 40, 50] — missing 30
    // After rescoring, final = [10, 20, 40]
    let report =
        CandidateRecallReport::from_sets(&[10, 20, 40, 50], &[10, 20, 40], &[10, 20, 30], 3)
            .unwrap();
    assert_eq!(report.candidate_recall_at_k(), 2);
    assert_eq!(report.final_recall_at_k(), 2);
    // The pool is the bottleneck — rescoring alone can't recover neighbor 30
    assert!(report.candidate_pool_is_recall_bottleneck());
}

#[test]
fn recall_rescoring_improves_order_but_not_pool_recall() {
    // Pool contains all true neighbors but in wrong order
    // True top-3 = [10, 20, 30], pool = [30, 20, 10, 40], final = [10, 20, 30]
    let report =
        CandidateRecallReport::from_sets(&[30, 20, 10, 40], &[10, 20, 30], &[10, 20, 30], 3)
            .unwrap();
    assert_eq!(report.candidate_recall_at_k(), 3);
    assert_eq!(report.final_recall_at_k(), 3);
    assert!((report.candidate_recall_ratio() - 1.0).abs() < 1e-9);
}

#[test]
fn recall_candidate_and_final_reported_separately() {
    // Candidate pool has 3 of 3 true neighbors but final only retains 2
    let report =
        CandidateRecallReport::from_sets(&[10, 20, 30, 99], &[10, 20, 99], &[10, 20, 30], 3)
            .unwrap();
    assert_eq!(report.candidate_recall_at_k(), 3);
    assert_eq!(report.final_recall_at_k(), 2);
    // final < candidate: rescoring improved order but dropped a true neighbor
}

// ===========================================================================
// EXACT GROUND TRUTH
// ===========================================================================

#[test]
fn ground_truth_topk_neighbor_ids_extraction() {
    let metric = ExactMetric::L2;
    let hits = vec![
        GroundTruthHit::new(42, MetricScore::new(metric, 0.5, 0.66).unwrap()).unwrap(),
        GroundTruthHit::new(17, MetricScore::new(metric, 1.0, 0.5).unwrap()).unwrap(),
        GroundTruthHit::new(99, MetricScore::new(metric, 1.5, 0.4).unwrap()).unwrap(),
    ];
    let gt = GroundTruthTopK::new(hits, metric, 3).unwrap();
    assert_eq!(gt.neighbor_ids(), vec![42, 17, 99]);
}

#[test]
fn ground_truth_shares_metric_kernel_with_production() {
    // The ground-truth path uses the same reference_distance kernel
    let a = unit_vector(8);
    let b = unit_vector(8);
    let production = reference_distance(ExactMetric::Cosine, &a, &b).unwrap();
    // Independent cross-check
    let independent = independent_cosine(&a, &b);
    assert!((production.normalized_score() - independent).abs() < 1e-4);
}

// ===========================================================================
// QUALITY POLICY: scoped-exact, never global
// ===========================================================================

#[test]
fn quality_compressed_rescore_is_never_globally_exact() {
    let claim = ExactnessClaim::RescoredApproximate;
    assert!(!claim.is_global_exact());
    assert!(!claim.is_scoped_exact());
}

#[test]
fn quality_scoped_exact_requires_enumerated_scope() {
    let scope = FilterScope::sparse(vec![1, 2, 3]).unwrap();
    let authorized = AuthorizedScope::new(scope, ExactMetric::Cosine).unwrap();
    let claim = ExactnessClaim::ScopedExact(authorized);
    assert!(claim.is_scoped_exact());
    assert!(!claim.is_global_exact());
}

#[test]
fn quality_partial_makes_no_completeness_claim() {
    let claim = ExactnessClaim::Partial;
    assert!(!claim.is_scoped_exact());
    assert!(!claim.is_global_exact());
}

#[test]
fn quality_dimension_constant_is_4096() {
    assert_eq!(EXACT_VECTOR_DIMENSION, 4_096);
}

// ===========================================================================
// CROSSOVER STRATEGY SELECTION (measured thresholds, not hardcoded)
// ===========================================================================

#[test]
fn crossover_uses_measured_threshold_not_hardcoded() {
    let threshold = CrossoverThreshold::new(500, 0.75).unwrap();
    assert_eq!(threshold.measured_ratio(), 0.75);
    assert!(threshold.prefers_exact_for(500));
    assert!(!threshold.prefers_exact_for(501));
}

#[test]
fn crossover_small_scope_selects_exact_scan() {
    let scope = FilterScope::sparse(vec![1, 2, 3]).unwrap();
    let threshold = CrossoverThreshold::new(500, 0.8).unwrap();
    assert_eq!(select_strategy(&scope, &threshold), ScanStrategy::ExactScan);
}

#[test]
fn crossover_large_scope_selects_ann() {
    let large_ids: Vec<VectorOffsetId> = (0..10_000).collect();
    let scope = FilterScope::sparse(large_ids).unwrap();
    let threshold = CrossoverThreshold::new(500, 0.8).unwrap();
    assert_eq!(
        select_strategy(&scope, &threshold),
        ScanStrategy::PredicateAwareAnn
    );
}

// ===========================================================================
// ERROR REDACTION — diagnostic-code-only, no payload leakage
// ===========================================================================

#[test]
fn error_renders_only_diagnostic_code() {
    let err = ExactScanError::contract(ExactScanDiagnosticCode::ZeroVector);
    let display = format!("{err}");
    let debug = format!("{err:?}");
    assert!(display.contains("zero_vector"));
    assert!(debug.contains("zero_vector"));
    // No caller-controlled payload should appear — only the stable code string
    // (the code itself legitimately contains "zero_vector", but no vector data)
    assert!(!display.contains("values"));
    assert!(!display.contains("payload"));
}

#[test]
fn error_all_diagnostic_codes_have_stable_strings() {
    for code in ExactScanDiagnosticCode::ALL {
        let s = code.as_str();
        assert!(!s.is_empty());
        // Every code must round-trip through an error
        let err = ExactScanError::contract(code);
        assert_eq!(err.diagnostic_code(), code);
    }
}

// ===========================================================================
// SCAN COMPLETENESS AND EXACTNESS CLAIM ELIGIBILITY
// ===========================================================================

#[test]
fn full_scope_completeness_is_exact_claim_eligible() {
    assert!(ScanCompleteness::FullScope.is_exact_claim_eligible());
    assert!(!ScanCompleteness::PartialScope.is_exact_claim_eligible());
}

// ===========================================================================
// FILTER SCOPE — contiguous, sorted run, sparse, one-element
// ===========================================================================

#[test]
fn filter_scope_one_element_contiguous() {
    let ext = ContiguousExtent::new(5, 6).unwrap();
    assert_eq!(ext.len(), 1);
    assert!(ext.contains(5));
}

#[test]
fn filter_scope_one_element_sorted_run() {
    let run = SortedIdRun::new(vec![42]).unwrap();
    assert_eq!(run.len(), 1);
}

#[test]
fn filter_scope_one_element_sparse() {
    let scope = FilterScope::sparse(vec![42]).unwrap();
    assert_eq!(scope.len(), 1);
}

// ===========================================================================
// RESCORING REQUEST/RESULT — budget bounds
// ===========================================================================

#[test]
fn rescoring_request_respects_candidate_cap() {
    let budget = RescoringBudget::new(RescoringBudgetFields {
        top_k: 5,
        candidate_cap: 3,
        io_batch_size: 2,
    })
    .unwrap();
    let candidates: Vec<RescoreCandidate> = (0..4)
        .map(|i| RescoreCandidate::new(i, i as f32).unwrap())
        .collect();
    let err = RescoringRequest::new(ExactMetric::L2, candidates, budget);
    assert!(err.is_err());
}

#[test]
fn rescoring_result_tracks_bytes_read() {
    let metric = ExactMetric::L2;
    let result = RescoringResult::new(vec![], 10, None, None, metric).unwrap();
    // 10 vectors * 4096 dims * 4 bytes = 163_840 bytes
    assert_eq!(result.bytes_read(), 10 * 4 * 4096);
}

#[test]
fn rescoring_result_complete_when_no_exhaustion() {
    let metric = ExactMetric::L2;
    let result = RescoringResult::new(vec![], 10, Some(5000), None, metric).unwrap();
    assert!(result.is_complete());
    assert_eq!(result.exact_scoring_nanos(), Some(5000));
}

#[test]
fn rescoring_result_incomplete_on_exhaustion() {
    let metric = ExactMetric::L2;
    let result = RescoringResult::new(
        vec![],
        5,
        None,
        Some(BudgetExhaustion::CandidateCapReached),
        metric,
    )
    .unwrap();
    assert!(!result.is_complete());
}
