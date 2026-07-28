# Exact filtered scans and full-precision rescoring

Status: first walking-skeleton contract (Refs #376).

Code: `crates/verbatim-core/src/exact_scan/`.

## Decision

When filtering leaves a small or contiguous candidate set, sequential
full-precision scanning can be faster and more predictable than graph
traversal with random SSD reads. This contract defines the typed boundaries
for exact filtered scans, full-precision ANN rescoring, candidate recall
gating, exact ground truth, and the quality policy that governs exactness
claims.

This contract is deliberately pure. It defines types, budgets, metric kernels
(portable scalar reference), diagnostic-only errors, and crossover-selection
logic, but has no live SSD I/O, no SIMD dispatch, no DiskANN3 binding, no
daemon wiring, and no vector math beyond the scalar reference kernel. Future
adapters must implement these boundaries before they can participate in
production exact scanning.

Original vector dimension remains 4,096 for the target profile. No dimension
reduction is introduced by this module.

## Contract surface

The module exports the following typed boundaries:

```text
exact_scan::ExactScanRequest      — query + scope + budget, validated
exact_scan::ExactScanOutcome      — bounded top-K hits + completeness
exact_scan::FilterScope           — contiguous extent / sorted ID run / sparse
exact_scan::RescoringRequest      — candidate pool + budget, validated
exact_scan::RescoringResult       — rescored candidates + I/O accounting
exact_scan::CandidateRecallReport — candidate recall@K vs final recall@K
exact_scan::GroundTruthTopK       — offline exhaustive trusted Top-K
exact_scan::RescoringBudget       — top-K, candidate cap, I/O batch bound
exact_scan::ExactMetric           — cosine / dot / L2 with normalization rule
exact_scan::reference_distance    — portable scalar reference kernel
exact_scan::ExactnessClaim        — scoped-exact / rescored-approximate / partial
exact_scan::CrossoverThreshold    — measured cardinality limit for exact vs ANN
exact_scan::select_strategy       — pick exact scan or predicate-aware ANN
exact_scan::ExactScanError        — fail-closed, diagnostic-code-only, redacted
```

## Production exact scan

When filtering leaves a small or contiguous candidate set, the exact scan path
scores every vector in the declared scope using full-precision 4,096-dimensional
distance computation.

- **Contiguous/aligned `float32` vector storage** is described by
  [`FilterScope::Contiguous`] (`ContiguousExtent`).
- **Compact numeric-ID to vector-offset mapping** uses [`VectorOffsetId`] (`u32`).
- **Sorted ID runs** are described by [`FilterScope::SortedRun`] (`SortedIdRun`),
  validated for monotonic increase and no duplicates.
- **Sparse sets** are described by [`FilterScope::Sparse`].
- **Cosine/dot/L2 kernels** are provided by [`reference_distance`], consistent
  with the metric normalization rule.
- **AVX2/AVX-512/NEON dispatch** is out of scope for this walking skeleton;
  the portable scalar fallback is the reference kernel. Architecture-specific
  kernels will be added in a future adapter and must produce identical results
  to this reference.
- **Fixed-size Top-K heap**: [`ExactScanOutcome`] is bounded by the configured
  `top_k`; cardinality exceeding top-K is rejected.
- **Deterministic tie handling and finite-value checks**: all vectors are
  validated for finiteness, non-zero, correct dimension, and metric-specific
  normalization before entering the scan.
- **Filter ID runs/bitmaps** minimize random reads by expressing the scope as
  contiguous extents or sorted runs.

### Crossover selection

Crossover between exact scan and predicate-aware ANN is selected by **measured**
thresholds, not hardcoded constants. [`CrossoverThreshold`] carries the
cardinality limit and the measured latency ratio that derived it.
[`select_strategy`] returns [`ScanStrategy::ExactScan`] when the scope
cardinality is at or below the limit, and [`ScanStrategy::PredicateAwareAnn`]
otherwise.

## ANN rescoring

DiskANN3 or another backend may use PQ/scalar/spherical/binary-like
representations to generate a bounded candidate pool.
[`RescoringRequest`] binds a candidate pool to a budget. The adapter reads the
original 4,096-dimensional vectors for those candidates and recomputes exact
distance before final dense ranking.

[`RescoringResult`] reports:

- the rescored candidates with exact distances;
- number and bytes of original vectors read (`vectors_read`, `bytes_read`);
- exact-scoring CPU time (`exact_scoring_nanos`);
- whether a budget prevented complete rescoring (`exhaustion`).

Rescoring improves order among retrieved candidates but **cannot** recover a
true neighbor never included in the candidate pool. Candidate Recall@K must
therefore be gated separately (see below).

## Candidate recall gating

[`CandidateRecallReport`] separates **candidate recall@K** (how many true
neighbors are in the candidate pool) from **final recall@K** (how many are in
the final rescored top-K). The `candidate_pool_is_recall_bottleneck` method
identifies when the gap between candidate and final recall cannot be closed by
rescoring alone — the candidate generation must change (e.g. increase
oversampling).

This cross-references issue #266 for candidate recall gating.

## Exact ground truth

[`GroundTruthTopK`] is the offline/diagnostic exhaustive path producing trusted
full-dimensional Top-K for benchmark samples. It shares the metric kernel
([`reference_distance`]) with production exact scan while retaining independent
validation: the test module cross-checks every distance with an *independent*
reference calculation (built from first principles, not reusing the production
kernel) to catch common bugs.

## Quality policy

- Original vector dimension remains 4,096 for the target profile
  ([`EXACT_VECTOR_DIMENSION`]).
- Candidate compression is allowed only if exact-ground-truth gates pass.
- Output-vector storage quantization, model-weight quantization, and embedding
  dimension are distinct concepts in config and reports (this module deals only
  with original full-precision vectors).
- **Never** label compressed-candidate plus rescore as globally exact
  ([`ExactnessClaim::RescoredApproximate`] — `is_global_exact()` always returns
  `false`).
- Exact/completeness claims require enumeration of the declared authorized
  scope ([`ExactnessClaim::ScopedExact`] carries an [`AuthorizedScope`]).

## Budget bounds

[`RescoringBudget`] bounds top-K memory (`top_k`), candidate fetch (`candidate_cap`),
and I/O batch size (`io_batch_size`). All three must be positively bounded; zero
is rejected. Budget exhaustion is reported via the typed [`BudgetExhaustion`]
enum, never by panicking or by an unbounded fallback.

## Fail-closed validation

All validation rejects invalid input:

- **Wrong dimension**: `VectorDimensionMismatch`
- **Non-finite (NaN/Inf)**: `NonFiniteVector`
- **Zero vector**: `ZeroVector`
- **Normalization mismatch** (e.g. cosine requires unit L2 norm):
  `MetricNormalizationMismatch`
- **Empty/unsorted/duplicate filter scope**: `InvalidFilterScope`
- **Budget exceeded**: `BudgetExceeded`, `CandidateCountExceedsCap`
- **Zero top-K / I/O batch**: `InvalidTopK`, `InvalidIoBatchSize`
- **Duplicate IDs**: `DuplicateCandidateId`, `DuplicateResultId`
- **Cardinality over top-K**: `ResultExceedsTopK`

No `unwrap`/`expect`/`panic` appears in production code. Errors are
diagnostic-code-only with a redacted `Debug` implementation that renders no
caller-controlled payload.

## Raw distance vs normalized score

[`MetricScore`] separates the raw ranking distance (smaller = closer) from the
metric-native normalized score (higher = closer). They are deliberately
distinct fields so that a reporting score can never be mistaken for a ranking
distance.

| Metric | Raw distance | Normalized score |
|--------|-------------|-----------------|
| Cosine | `1 - cos_sim` | `cos_sim` |
| Dot    | `-dot`       | `dot`           |
| L2     | `sqrt(sum_sq)` | `1 / (1 + dist)` |

## Test coverage

- Golden cosine/dot/L2 fixtures, normalized and non-normalized.
- Independent cross-check of every metric against a first-principles reference.
- Zero, NaN, Inf, negative-infinity, and wrong-length vector rejection.
- Cosine normalization enforcement (rejects non-unit norm).
- Exact scan over contiguous, sparse, and one-element filters.
- Candidate pools that include and omit the true neighbor.
- Budget bounds: zero top-K, zero candidate cap, zero I/O batch, cap exceeded.
- Candidate recall gating: candidate@K vs final@K reported separately.
- Quality policy: scoped-exact vs rescored-approximate vs partial.
- Crossover selection by measured thresholds.
- Error redaction: diagnostic-code-only rendering.
