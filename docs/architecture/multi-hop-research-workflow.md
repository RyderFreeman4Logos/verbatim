# Bounded multi-hop research workflow (WORKFLOW-006)

Status: walking skeleton for
[#357](https://github.com/RyderFreeman4Logos/verbatim/issues/357).
Code: `crates/verbatim-core/src/multi_hop_research/` (module facade +
decomposition / subquery / coverage / budget / evidence / merge / run /
workflow / error units).

## Problem

Cross-document questions need multiple complementary searches and coverage
checks. A single top-k pass may miss bridge evidence, but an unbounded agent
can spend uncontrollably, follow injected instructions, or fabricate completion.

## Design direction

Represent the workflow as a finite state graph with typed subquestions, declared
dependencies, parallel retrieval, evidence coverage, bounded corrective rounds,
and one final merged ContextPack. Fail closed on budget exhaustion and
incomplete coverage — never open-ended autonomous looping.

Pipeline:

```text
ResearchQuestion
  → DecompositionPlan (SubQuestion DAG)
  → ParallelRetrievalBatch / SubqueryResult (+ provenance)
  → CoverageReport (covered / partial / missing / conflicts)
  → [optional CorrectiveRound within budget]
  → MergedContextPack  |  Incomplete
```

## Contract summary

| Type | Role |
| --- | --- |
| `ResearchQuestion` / `SubQuestion` / `DecompositionPlan` | Typed decomposition with declared dependencies |
| `RetrieverKind` | `lexical` / `dense` / `graph_local` / `graph_global` / `exact` |
| `SubqueryRequest` / `SubqueryResult` / `ParallelRetrievalBatch` | Parallel independent subqueries + per-retriever provenance |
| `CoverageReport` / `FactCoverage` / `RelationCoverage` | Coverage, conflicts, unresolved requirements |
| `ResearchBudget` / `ResearchBudgetUsage` / `BudgetDimension` | Max rounds, subqueries, candidates, tokens, endpoint calls, cost, wall time |
| `ResearchRound` | `decomposing` → `retrieving` → `evaluating_coverage` → `corrective_round` → `complete` / `incomplete` |
| `EvidenceOrigin` / `EvidenceOriginKind` | Injection guard: evidence text cannot alter workflow control |
| `MergedContextPack` / `AttributedEvidenceUnit` | Deduped units with subquestion attribution + direct flag |
| `WorkflowRun` | Persistence envelope: rounds, hashes, budget, usage, final status |
| `MultiHopResearchWorkflow` | Async trait: `decompose`, `retrieve_round`, `evaluate_coverage`, `merge`, `execute` |
| `ResearchOutcome` | `Complete` / `Incomplete` / `Disabled` |
| `ResearchError` | `validation` / `illegal_transition` / `budget_exhausted` / `incomplete_coverage` / `injection_rejected` / `missing_evidence` / `model_failure` / `disabled` |

### Fail-closed rules

1. Every budget dimension is a hard cap; exhaustion yields `BudgetExhausted`
   and terminal `Incomplete` (never silent complete).
2. `CoverageReport.is_complete` requires all required facts/relations
   `Covered`, non-empty requirements, and zero conflicts; flag is recomputed
   and validated.
3. Graph retriever kinds declare `requires_edge_evidence`; adapters must not
   accept graph paths without backing evidence units (enforcement residual).
4. `EvidenceOrigin` of `evidence_text` / `document_body` /
   `model_intermediate` cannot alter workflow instructions or tool permissions.
5. Subquery result origins must not include control channels
   (`WorkflowInstruction` / `PolicyConfig`).
6. `WorkflowRun.complete` requires `evaluating_coverage` or `corrective_round`
   and binds pack digests to the run.
7. Unknown `WorkflowRun.schema_version` fails validation/decode.
8. Empty / whitespace digests, ids, and required strings are rejected.
9. Decomposition plans reject duplicate ids, self-deps, unknown deps, and
   dependency cycles.

### Layering

| Layer | Module | Notes |
| --- | --- | --- |
| Wire contracts | `wire_schemas` | QueryPlan / EvidencePack / ContextPack (adapters materialise) |
| Pagination | `pagination` | Snapshot-bound search pages (retrieve residual) |
| Public SDK | `sdk` | Client ops for R/A/G + workflow request envelopes |
| Grounded answer | `grounded_answer` | Downstream: ContextPack → verified answer |
| This contract | `multi_hop_research` | Decomposition, coverage, budget, merge, trait |

Adapters should implement `MultiHopResearchWorkflow` in a non-capped module and
call only public SDK/wire types. Do not grow `store.rs`, daemon `main.rs`, or
CLI `client.rs` solely to adopt this contract.

## What this slice wires

- Module export from `verbatim-core` (`pub mod multi_hop_research`)
- Pure decomposition, subquery, coverage, budget, injection markers, merge
- `WorkflowRun` persistence envelope + JSON encode/decode helpers
- `MultiHopResearchWorkflow` trait + pure `advance_round` / `fail_closed` helpers
- Unit tests: transitions, budget exhaustion, injection, round-trips, merge
- Architecture note (this file)

## What this slice does **not** do (residual)

- Live retrieval / model integration, SSE/streaming, daemon/CLI/ADK wiring
- Graph edge evidence enforcement beyond type flags
- Full hybrid retrieval/rerank profiles
- Benchmark cases (two/three-hop, false bridge, injection, budget)
- Closing epic #357
