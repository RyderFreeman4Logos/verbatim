# Hybrid fusion

`verbatim_core::hybrid_fusion` is the bounded, backend-neutral contract for
combining candidates from dense, lexical, exact/reference, metadata, graph,
and exhaustive retrievers. It is a pure contract layer: it defines the data,
validation, lifecycle order, and audit guarantees without binding a search
backend, store, daemon, CLI, filesystem, or live scorer.

## Contract boundary

A fusion run carries a validated [`FusionProfile`](../../crates/verbatim-core/src/hybrid_fusion/profile.rs),
a [`FusionBudget`](../../crates/verbatim-core/src/hybrid_fusion/budget.rs), and
complete raw `RetrieverResult` records. The durable result is a
`FusionStageOutput`; callers can only encode it after its invariants are
revalidated, and decoding repeats the same validation before the output is
returned.

This contract specifies candidate selection and audit semantics. Retrieval,
scoring, model invocation, hydration I/O, and rendering remain adapter
responsibilities outside the module.

## Bounded candidate lifecycle

The lifecycle is strictly bounded at every transition:

1. **Retriever pool** — each retriever returns a bounded ranked candidate list.
2. **Fusion** — candidates are merged by a profile-selected strategy into a
   bounded fused pool.
3. **Exact/reference precedence** — exact or reference candidates may establish
   explicit precedence according to the profile; they do not silently convert
   approximate results into exhaustive claims.
4. **Diversity** — diversity policy reduces or reorders the bounded pool without
   creating an unbounded side channel.
5. **Rerank** — only the configured rerank-input cap may enter reranking.
6. **Hydration** — only the final-hydration cap may be hydrated or rendered.

`FusionUsage` is recomputed from the retained retriever and fused candidate
counts whenever an output is validated. It must equal the serialized usage
record and satisfy the serialized `FusionBudget`; tampered counters or counts
that exceed a cap fail closed.

## Backend-neutral strategies

`FusionStrategy` deliberately describes policy rather than a backend API:

| Strategy | Contract |
| --- | --- |
| `Rrf` | Reciprocal-rank fusion over the preserved raw ranks. |
| `WeightedScore` | Explicit, positive retriever weights that must sum to one; scores use a compatible normalization policy. |
| `ExactReferencePrecedence` | A reference/exact-first policy that preserves its inclusion reason and provenance rather than hiding the precedence decision. |

The strategy does not make a retrieval implementation exact, exhaustive, or
backend-specific. A backend must opt in explicitly where reduced
explainability is unavoidable.

## Raw provenance and auditability

Every fused candidate retains one `ProvenanceEntry` per contributing retriever:

- retriever identity and kind;
- the raw rank assigned by that retriever; and
- the raw score and direction assigned by that retriever.

A durable output also retains the full `RetrieverResult` for every retriever.
Validation binds each provenance entry to the result with the same retriever
identity and to a candidate with the same hit id. The retriever kind, raw rank,
and raw score must match exactly. A fused candidate therefore cannot invent or
borrow provenance from a different result, rank, score, or retriever kind.

## Completeness semantics

Completeness is explicit and scope-bound:

| State | Meaning |
| --- | --- |
| `ApproximateTopK` | Normal ANN/BM25-style top-k output; it makes no exhaustive claim. |
| `ExactScopeEnumerated` | Exact only for a named, enumerated scope with valid coverage accounting. |
| `CoverageIncomplete` | The scope is known but coverage could not be established; no exhaustive claim is allowed. |

An output may claim `ExactScopeEnumerated` only when a retriever has an
exhaustive result for the **same** `ExhaustiveScopeId` and at least one fused
candidate carries provenance from that retriever. An unrelated exhaustive
result, or an exhaustive result that did not contribute to the fused output,
cannot justify an exact claim.

## Fail-closed diagnostics

Invalid profiles, bounds, provenance, completeness claims, codec payloads, and
state transitions return closed `FusionDiagnosticCode` values. Errors render
only stable diagnostic codes and `FusionError` is intentionally not serializable:
caller-controlled completeness scope identifiers cannot escape through a
structured error payload. The module rejects invalid or unverifiable output
instead of guessing at provenance, usage, or completeness.

## Verification expectations

The focused hybrid-fusion tests cover profile and budget validation, raw
provenance round-tripping, output decode validation, exhaustive-scope rules,
and diagnostic redaction. Adapter integrations should additionally test their
own retrieval-result construction and maintain the same bounded and
fail-closed contract at their boundary.
