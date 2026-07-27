# Overfetch-elimination retrieval orchestration

Status: first walking-skeleton contract (Refs #370).

Code: `crates/verbatim-core/src/overfetch/`.

## Decision

Normal retrieval is a bounded orchestration workflow. It must not turn a
source-, collection-, tenant-, ACL-, or lifecycle-scoped request into a
full-corpus dense/BM25 request followed by per-candidate SQLite reads.

This contract is deliberately pure. It defines types, ports, budgets,
diagnostic-only errors, and test instrumentation, but has no live SQLite,
Tantivy, Qdrant, DiskANN3, daemon, filesystem, or provider wiring. Future
adapters must implement these boundaries before they can participate in normal
retrieval.

An explicit exhaustive workflow is separate. A normal Top-K request may not
allocate, return, or hydrate data proportional to corpus size.

## Required pipeline

`BoundedRetrievalContract::retrieve` uses one ordered pipeline:

```text
plan(budget, filters)
        |
        v
execute_retrievers
        |
        v
fuse_truncate
        |
        v
validate_candidates
        |
        v
hydrate_batch
        |
        v
report
```

Each hand-off is a typed bounded stage. The contract reapplies the plan's cap
at each boundary, so an adapter cannot pass a larger typed candidate collection
to the following stage. Adapters must also issue their backend request with the
plan's cap; truncating a response protects later stages but cannot make an
already materialized corpus-sized backend response acceptable.

`CandidateValidation` is intentionally lightweight and precedes complete
result construction. It carries a candidate identifier and finite retrieval
score, not bodies, parent records, or evidence text. `FullHydration<T>` only
appears at the final batch-hydration boundary.

## Hard search budget

`SearchBudget` has independent positive caps for every bounded normal-query
stage:

| Field | Enforced boundary |
| --- | --- |
| `dense_candidate_k` | Dense retriever request/output |
| `lexical_candidate_k` | Lexical/BM25 retriever request/output |
| `exact_candidate_k` | Exact retriever request/output |
| `graph_candidate_k` | Graph retriever request/output |
| `fused_pool_size` | Candidate fusion output |
| `rerank_input_size` | Lightweight candidate validation/rerank input |
| `final_hydration_list_size` | Complete-result batch hydration input/output |
| `debug_output_size` | Compact or explicit-full diagnostic output |

Validation requires `rerank_input_size <= fused_pool_size` and
`final_hydration_list_size <= rerank_input_size`. An invalid cap, arithmetic
overflow, or malformed serialized budget fails with the closed
`budget_exceeded` diagnostic code.

Debugging has a separate cap. `DiagnosticMode::Disabled` does not invoke the
debug-entry producer; `Compact` and explicit `Full` collect at most
`debug_output_size`. There is no implicit full-debug mode.

## Count and hydration ports

`CountPort::count_indexed` is the only count boundary in this contract. Its
implementation requirement is an indexed `COUNT(*)` or metadata count; it must
not implement a count through `list_all()?.len()` or any equivalent row/text
materialization.

`BatchHydrationPort` separates the five required O(1) batch operations:

1. chunk headers;
2. chunk bodies;
3. parent links;
4. chunk-evidence links; and
5. evidence units.

All methods receive the bounded final candidate slice and a
`StatementCountInstrumentation`. An adapter records one SQL statement for each
batch kind. A repeated batch kind is a deterministic `n_plus_one_detected`
failure, rather than an observational performance regression. The instrumentation
also enforces a total statement cap, making it possible to test the same bounded
statement count with synthetic corpus cardinalities of 10, 10,000, and
1,000,000 without materializing those corpora.

`HydrationBatch::new` rejects rather than silently truncates an oversized
post-hydration result. At that point the complete data was already fetched, so
returning `unbounded_hydration` is the fail-closed behavior.

## Strict filters and adaptive overfetch

Strict filters are typed (`source`, `collection`, `tenant`, `acl`, and
`lifecycle`) and bounded in count. A backend declares one of three strict-filter
modes:

- `Native`: it applies the predicate before ranking;
- `Adaptive`: it uses an `AdaptiveOverfetchPolicy`; or
- `Unsupported`: it returns `unsupported_strict_filter`.

`AdaptiveOverfetchPolicy` has an initial request cap, maximum request cap,
growth factor, and maximum attempt count. Every request is bounded by both the
policy and the relevant `SearchBudget` retriever cap. If a requested Top-K
would equal or exceed the corpus size, it returns
`corpus_size_top_k_forbidden`; it never substitutes the corpus count as Top-K.

This is intentionally conservative. If a strict predicate cannot be satisfied
by native filtering or bounded adaptive overfetch, normal retrieval fails
closed. The caller may choose an explicitly named exhaustive workflow instead;
the normal planner never silently converts to one.

## Primary backend selection

`PrimaryBackendSelection` records one selected primary and an optional declared
fallback. `validate_first_attempt` requires the selected primary to run first.
In particular, selecting Qdrant does not permit an unconditional local-dense
pre-search.

A fallback is available only after `PrimaryBackendOutcome::TypedFailure` or
`DeclaredInsufficientResults`. A satisfied primary result has no fallback, and
a missing or preemptive fallback yields `primary_backend_required`.

## Complexity invariants

For fixed `SearchBudget` values, normal retrieval has these orchestration
bounds:

```text
retriever candidates = O(k_dense + k_lexical + k_exact + k_graph)
fusion              = O(candidate_budget log candidate_budget)
hydration SQL calls = O(1) batches
hydrated text       = O(final_limit + bounded_rerank_candidates)
```

The candidate budget is the sum of the four retriever caps. More ANN effort may
read more SSD pages inside a selected retriever, but no normal-query stage may
expose corpus-proportional candidate vectors, debug output, or hydrated text.

## Diagnostics and serialized inputs

`OverfetchError` has only closed diagnostic variants:

- `budget_exceeded`;
- `corpus_size_top_k_forbidden`;
- `n_plus_one_detected`;
- `unbounded_hydration`;
- `unsupported_strict_filter`; and
- `primary_backend_required`.

Its `Debug` and `Display` implementations retain no caller-controlled strings,
SQL, filters, IDs, secrets, or provider details. Budget and retrieval-plan JSON
helpers serialize only validated values and revalidate after deserialization.

## Adapter checklist

Before connecting a live backend or store to this contract, an adapter must:

1. issue each retriever with the plan's cap rather than a corpus count;
2. apply native strict predicates before ranking when it advertises `Native`;
3. use the typed adaptive policy or return an unsupported-filter diagnostic;
4. run the selected primary before any fallback;
5. validate candidate metadata before complete hydration;
6. invoke every authoritative-store hydration operation as one bounded batch;
7. record batch SQL statements in tests; and
8. keep debug producers lazy and bounded.

Live storage migrations, retrieval integration, benchmarks, and backend-specific
filter capability wiring remain out of scope for this first contract slice.
