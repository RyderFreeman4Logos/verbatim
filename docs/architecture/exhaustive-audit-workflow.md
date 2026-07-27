# Completeness-aware exhaustive audit workflow (WORKFLOW-008)

Status: walking skeleton for [#359](https://github.com/RyderFreeman4Logos/verbatim/issues/359).
Code: `crates/verbatim-core/src/exhaustive_audit/`.

## Purpose

An **exhaustive** result is meaningful only relative to a declared scope. This
contract records that scope, its freshness/index prerequisites, deterministic
candidate enumeration, and a coverage manifest before it permits the wording
`exhaustive over declared scope`. It never represents an ANN, graph, or top-k
result as a global or exhaustive result.

```text
DeclaredAuditScope
  → primary candidate enumeration (exact / metadata / lexical)
  → CoverageManifest (per declared collection/source/snapshot)
  → reconciliation / deduplication
  → completeness report
```

## Contract summary

| Type | Role |
| --- | --- |
| `DeclaredAuditScope` / `AuditScopeMember` | Explicit collection + source + snapshot identity, freshness, and index coverage prerequisites |
| `CompletenessTarget` | User intent: `all`, `only`, `none`, or `every` |
| `EnumerationMethod` / `CandidateEnumeration` | Enumeration provenance; exact, metadata, and lexical are primary, while dense ANN, graph, and top-k are supplementary |
| `CoverageManifest` / `CoverageEntry` | Every scope member is `searched`, `unsearched`, `blocked`, `stale`, or `unsupported` |
| `DeduplicatedCandidate` / `CandidateOccurrence` | Type-only version/near-duplicate grouping retaining occurrence count and locators |
| `CompletenessStatus` | `exhaustive_over_declared_scope`, `incomplete`, `unable_to_establish`, or `blocked` |
| `ExhaustiveAuditBudget` / `ExhaustiveAuditUsage` | Hard caps for scope members, enumerations, candidates, cost units, and elapsed time |
| `AuditWorkflowRun` | Persistable envelope for stages, hashes, fingerprints, budget/usage, warnings, and status |
| `ExhaustiveAuditWorkflow` | Adapter trait: `declare_scope → enumerate → cover → reconcile → report` |

## Fail-closed rules

1. The scope must declare non-duplicate collection/source/snapshot members.
2. `ExhaustiveOverDeclaredScope` requires every member to be fresh, fully
   indexed, and marked `searched` in a manifest bound to the exact scope hash.
3. A deterministic primary exact, metadata, or lexical enumeration bound to the
   same scope is also required. ANN, graph, and top-k passes are supplementary
   even when they find no results.
4. Any blocked member yields `Blocked`; unsearched, stale, or unsupported
   members yield `Incomplete`. A missing deterministic primary pass yields
   `UnableToEstablish`.
5. The state machine only permits `declared → enumerating → covering →
   reconciling → reporting → terminal`; reporting selects the terminal stage
   that matches the computed completeness status.
6. Budget increments are checked before mutating run usage.
7. The contract contains no Store, SQL, filesystem, live retriever, model,
   daemon, or CLI wiring. Adapters must preserve these conditions.

`Only` and `None` are not special exemptions: they require the same exhaustive
coverage conditions as `All` and `Every`, rather than relying on empty or
limited approximate retrieval output.

## What this slice wires

- `verbatim-core` public module export and pure serializable contract types
- scope, coverage, enumeration/deduplication, budget/error, stage/run, and
  adapter-trait modules
- focused tests for stale/partial/unsupported scope prerequisites, target
  coverage, ANN/top-k refusal, blocked and incomplete manifest states,
  occurrence retention, legal stages, and budget caps

## Residual work

- authoritative Store/SDK/pagination adapters and persisted report storage
- lexical synonym/alias expansion rule implementations and human-review UI
- parser, ACL, and freshness probes that populate real coverage manifests
- benchmark corpora and end-to-end audit presentation
