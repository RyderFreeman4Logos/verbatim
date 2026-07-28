# Hard-bounded search-budget planner contract

Status: first walking-skeleton contract (Refs #371).

Code: `crates/verbatim-core/src/search_planner/`.

## Decision

Normal retrieval must select a path only after validating a caller-owned,
hard-bounded `SearchBudget`; an authorization-bound cardinality estimate; a
benchmark calibration for the selected immutable generation; and the backend's
declared capabilities. The result is a sealed `RetrievalPlan`, not permission
for an adapter to expand work or reinterpret a filter.

The module is deliberately pure. It defines planning, budget, capability,
generation-binding, completion-reporting, and closed-diagnostic types. It does
not read a catalog, execute SQLite/Tantivy/Qdrant/DiskANN3 work, inspect a query
payload, load an index, or expose daemon or CLI wiring. Live adapters remain
responsible for applying the resulting plan and reporting actual work.

## Planning pipeline

An adapter resolves query, tenant, ACL, source, collection, lifecycle, and other
sensitive inputs before constructing `PlannerRequest`. The planner therefore
receives only authorization-bound facts and selects one path through one atomic
validation gate:

```text
caller SearchBudget + authorized cardinality + selectivity calibration
                     + backend capability + generation binding
                                      |
                                      v
                         SearchPlanner::plan_and_validate
                                      |
                                      v
validate budget, vector dimension/metric, required work reporting,
capability safe limits, and calibration-generation match
                                      |
                                      v
trust disposition for the cardinality estimate
     | trusted                         | stale / low confidence / known wrong
     v                                 v
select exact, DiskANN3, or explicit    fail closed or return a named,
exhaustive path                         hard-bounded degraded profile
     |
     v
seal plan identity + non-widening budget snapshot + visible completeness
     |                                                        |
     v                                                        v
adapter executes only bounded work                       PublicRetrievalRecord
and reports measured usage                                with typed completion
```

`PlannerRequest` intentionally carries no raw query, identifier, tenant value,
ACL material, filter text, or caller-controlled diagnostic text. The raw
matching cardinality is private and redacted from `Debug`; it informs an
internal decision but is not a public corpus-size disclosure.

## Selectivity crossovers and paths

`CrossoverThresholds` are benchmark-derived values bound to a non-zero
calibration generation. They are valid only when all values are positive and
`exact_simd_scan_max_matches <= predicate_aware_diskann3_min_matches`.
`SearchPlanner` rejects a request when that calibration generation differs from
the request's `GenerationBinding`.

The profile records one of the authorized selectivity classes: 100%, 10%, 1%,
0.1%, 0.01%, single-document, or explicit exhaustive. It does not encode a raw
corpus size. For a trusted ordinary Top-K request, the selected path is:

1. **Exact SIMD scan** when the authorized matching count is at or below the
   exact threshold, and conservatively for any gap below the first calibrated
   predicate-aware DiskANN3 threshold. The count must also fit the independent
   `exact_candidate_limit`; otherwise planning fails rather than widening the
   exact work.
2. **Predicate-aware DiskANN3** at or above
   `predicate_aware_diskann3_min_matches`, only when the backend declares
   in-traversal predicate support. This is visibly
   `ApproximatePartial`, has a bounded quantized candidate-generation profile,
   and reserves a separate full-precision rescoring allocation.
3. **Exhaustive enumeration** only when the request intent or selectivity class
   explicitly requires it. Its authorized count must fit both
   `exhaustive_enumeration_max_matches` and `exact_candidate_limit`; exhaustive
   work is never a silent fallback from normal Top-K.
4. **Named degraded profile** only when the caller explicitly permits it for an
   untrusted cardinality estimate or unsupported strict predicate. It remains
   visibly partial and hard-bounded.

A `SingleDocument` profile with more than one matching record is invalid. A
strict predicate must be applied before ranking for exact or exhaustive paths,
and during traversal for the DiskANN3 path. If that cannot happen, the caller's
handling mode determines either a closed `strict_predicate_unsupported` result
or the named degraded profile; post-filtering cannot masquerade as strict
retrieval.

## Independent budget dimensions

`SearchBudget` validates positive, independently measured caps; no single
candidate count stands in for all retrieval cost. Its candidate and result
boundaries are:

| Dimension | Boundary |
| --- | --- |
| `result_limit` | public records returned |
| `dense_candidate_limit` | dense candidate generation |
| `lexical_candidate_limit` | lexical candidate generation |
| `exact_candidate_limit` | exact scan or explicit enumeration |
| `graph_candidate_limit` | graph candidate generation |
| `fused_pool_limit` | post-fusion pool |
| `rerank_candidate_limit` | reranker admission |
| `full_precision_rescore_limit` | original-vector rescoring admission |
| `hydration_limit` | authoritative-record hydration |
| `debug_record_limit` | diagnostic-record output |

Operational limits are independent as well: SSD pages, bytes read, CPU
microseconds, implementation-defined work units, shared wall-time
microseconds, concurrently active stages, and total stage attempts. Validation
requires `result_limit <= hydration_limit <= rerank_candidate_limit <=
fused_pool_limit`, `full_precision_rescore_limit <= rerank_candidate_limit`,
and a checked sum of dense, lexical, exact, and graph candidate limits.

A plan may narrow a caller's budget but cannot widen any dimension.
`SearchBudgetUsage` repeats every dimension for measured execution; a
`PublicRetrievalRecord` is emitted only after that usage fits the sealed plan.
Fallback work derives a fresh budget from the remaining shared caps, so it
cannot reset pages, bytes, CPU, work units, deadline, or attempt accounting.

## Safety rules

The contract fails closed instead of converting a bad input or unsafe backend
claim into unbounded work:

- Every cap and backend safe limit must be positive; malformed or overflowing
  budgets are rejected.
- The backend must support the requested vector dimension and metric, bind
  results to a generation, report pages/bytes/CPU/work units, and accept every
  requested candidate, I/O, CPU, work, and concurrency cap.
- The calibration generation and plan generation must match before path
  selection; results preserve the same immutable generation binding.
- Stale, low-confidence, or known-wrong cardinality estimates are rejected
  unless the caller expressly asks for a named bounded degraded profile.
- Exact, exhaustive, and approximate paths have distinct typed completeness.
  An approximate or degraded plan cannot report `Complete`.
- Actual execution exceeding any sealed cap is rejected before a public record
  is created.
- `SearchPlannerError` contains only a stable diagnostic code. Its `Debug` and
  `Display` text contain no raw cardinality, query, identifier, filter, tenant,
  backend response, or secret.

## Relation to overfetch elimination and DiskANN3

This module is the path-selection boundary that precedes the bounded retrieval
orchestration in [overfetch elimination](overfetch-elimination.md) (Refs #370).
The two modules intentionally have separate `SearchBudget` types and are not
wired together in this walking skeleton. The #371 planner decides whether a
trusted authorized subset may use exact scan, predicate-aware DiskANN3, explicit
exhaustive enumeration, or a visible degraded profile. The #370 contract owns
backend request caps, fusion/truncation, candidate validation, batch hydration,
and final reporting. A future adapter must translate only a non-widening,
validated plan into #370's bounded stages; it must not use planning as a reason
to materialize a corpus-sized response.

[DiskANN3](diskann3-architecture.md) (Refs #369) defines the SSD-native vector
retrieval architecture, its immutable generation model, predicate-aware
traversal requirement, full-precision rescoring, and its own resource/stage
contracts. #371 selects `PredicateAwareDiskAnn3` only after a generation-bound
crossover and explicit in-traversal predicate capability. It does not implement
`diskann3::VectorSearchContract`, open SSD files, construct graph queries, or
share a budget type with #369. Those integrations remain future adapter work.

## What this slice wires

- A public, validated `SearchBudget` and measured `SearchBudgetUsage` contract.
- Generation-bound cardinality, selectivity, capability, and path-selection
  types.
- Sealed `RetrievalPlan` snapshots with exact, approximate, exhaustive, and
  degraded completeness semantics.
- Closed diagnostics and public execution records that validate actual work.
- Module-local test wiring for budget, selection, strict-filter, generation,
  exhaustive-boundary, and degraded-profile behavior.

## Out of scope

- Live catalog counting, filter parsing, authorization evaluation, or query
  payload processing.
- Live exact SIMD, DiskANN3, Qdrant, Tantivy, SQLite, filesystem, daemon, or
  CLI integration.
- Benchmark execution or choosing production crossover numbers.
- Converting this planner into an executor, a fallback mechanism that resets
  budgets, or an implicit exhaustive-query route.
