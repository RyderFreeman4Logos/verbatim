# Compare-sources / version-differences workflow (WORKFLOW-007)

Status: walking skeleton for
[#358](https://github.com/RyderFreeman4Logos/verbatim/issues/358).
Code: `crates/verbatim-core/src/compare_sources/` (pure scope / dimension /
budget / result / run / stage / workflow / error units).

## Problem

A version-difference answer is only trustworthy when each side is an explicitly
selected source/version and every displayed conclusion retains auditable
per-side evidence. A generic summarizer can silently compare the wrong
version, elide an ACL denial, confuse a model interpretation with source text,
or keep spending after a budget cap. Those are unsafe failure modes for an
R/A/G system that must expose source provenance.

## Design direction

This contract models **exactly two** source/version identities (not arbitrary
multi-source aggregation), bounded comparison dimensions, and an ordered
workflow. It records hashes, fingerprints, costs, warnings, and terminal status
without invoking a retriever or model.

```text
ComparisonScope (left version + right version + constraints)
  → decompose dimensions
  → resolve lifecycle / availability / ACL
  → extract provenance-bound quotations and separate interpretations
  → align cells (agreement / difference / conflict / missing / incomparable)
  → render ComparisonContextPack
  → Complete | Incomplete | Disabled
```

## Contract summary

| Type | Role |
| --- | --- |
| `ComparisonScope` / `SourceVersion` | Two source/version identities, lifecycle, effective-date, jurisdiction, product, and resolution status constraints |
| `SourceAvailability` | `authorized` / `acl_denied` / `version_gone` / `unresolved`; resolution fails closed |
| `ComparisonDimension` | Bounded, human-meaningful comparison axis |
| `DimensionValue` | One side's normalized value plus verbatim `quotations`, separate `interpretation`, and provenance |
| `EvidenceProvenance` / `QuotedEvidence` | Evidence-unit identity, source/version binding, locator, content hash, and source quote |
| `DimensionAlignment` | `agreement` / `difference` / `conflict` / `missing` / `incomparable` |
| `ComparisonCell` / `ComparisonResult` | Structured pairwise cells and result-level summary; conflict/missing remain visible rather than being summarized away |
| `ComparisonBudget` / `ComparisonBudgetUsage` | Hard dimensions, sources, candidates, tokens, cost-units, and wall-time caps |
| `CompareSourcesWorkflowRun` | Versioned persistence envelope: stages, artifact hashes, fingerprints, costs, warnings, and status |
| `CompareSourcesWorkflow` | Async adapter trait: `decompose` → `resolve` → `extract` → `align` → `render` |
| `ComparisonContextPack` | Reusable cells for downstream workflows, optionally paired with a public wire `ContextPackEnvelope` |
| `ComparisonError` | Typed `scope_unresolved`, `version_gone`, `acl_denied`, `budget_exhausted`, and `missing_evidence` failures (plus validation/state/disabled support) |

### Fail-closed rules

1. `ComparisonScope::require_comparable` requires both selected identities to
   be distinct, resolved, version-available, and ACL-authorized. `acl_denied`,
   `version_gone`, and `unresolved` yield typed errors; no side is silently
   skipped.
2. This first slice supports exactly two sides. `ComparisonBudget.max_sources`
   cannot exceed two; multi-source execution is residual.
3. Every `DimensionValue` needs at least one quotation and provenance record;
   every quotation must name one of its provenance evidence units, which must
   bind the value's source and version.
4. Source quotation and analytical interpretation are separate fields.
   `ComparisonCell.interpretation` is never a substitute for quotation text.
5. `Agreement`, `Difference`, `Conflict`, and `Incomparable` require evidence
   from both sides. `Missing` preserves any available side and rejects a false
   missing classification when both sides are present.
6. Each budget dimension is a hard cap. `record_stage` checks candidate usage
   before mutating the run; excess produces `BudgetExhausted`, not clamping or
   a silent partial result.
7. A completed run requires rendering plus hashes for both `ComparisonResult`
   and `ComparisonContextPack`. Unknown run schema versions fail decoding.
8. `fail_closed` maps ACL, lifecycle, evidence, and budget errors to
   `Incomplete`; only an explicitly disabled capability maps to `Disabled`.

### Layering

| Layer | Module | Notes |
| --- | --- | --- |
| Wire contract | `wire_schemas` | Canonical JSON/hash conventions; optional validated `ContextPackEnvelope` in the reusable comparison pack |
| Pagination | `pagination` | Snapshot-bound retrieval page contract for live adapters (residual) |
| Public SDK | `sdk` | Public operation envelopes for future adapters (residual) |
| Grounded output | `grounded_answer` | Downstream consumer may receive materialized context evidence |
| This contract | `compare_sources` | Scope, dimensions, alignment, budgets, results, run, and trait only |

Adapters should implement `CompareSourcesWorkflow` outside capped core paths,
use public SDK/wire contracts, and avoid growing `store.rs`, daemon entrypoints,
or CLI clients merely to adopt this contract.

## What this slice wires

- `verbatim-core` public module export: `compare_sources`
- Pure two-sided scope, source lifecycle/ACL-resolution, dimension, evidence,
  alignment, result, context-pack, budget, state-machine, and run contracts
- Versioned JSON run encode/decode and deterministic SHA-256 artifact hashing
- Async adapter trait and pure fail-closed state helpers
- Unit tests for every enum/error class, ACL/version resolution, evidence
  binding, quotation/interpretation separation, all hard budget caps, legal
  transitions, terminal outcomes, and JSON round-trips

## Residual (not in this slice)

- Live retriever/model/daemon/CLI wiring
- Multi-source (>2) comparison execution
- SSE/streaming
- Retrieval snapshots, ranking, and model prompts
- Rendering templates/UI beyond structured `ComparisonContextPack`
- Issue-state changes
