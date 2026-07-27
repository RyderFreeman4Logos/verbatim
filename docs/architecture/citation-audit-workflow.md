# Citation-audit / claim-support workflow (WORKFLOW-009)

Status: walking skeleton for [#360](https://github.com/RyderFreeman4Logos/verbatim/issues/360).
Code: `crates/verbatim-core/src/citation_audit/`.

## Purpose

This contract audits externally authored prose without treating model output,
existing citation markup, retrieval candidates, or generated locators as source
truth. It exposes claim-level support findings that are useful even when
Verbatim generation is disabled.

```text
AuditDocument (prose + untrusted existing citations)
  → ClaimSegmentation (stable IDs + exact source offsets)
  → retrieve opaque candidates (exact / lexical / dense / graph / metadata)
  → constrained classification
  → server-resolve IDs + exact quotation validation
  → per-claim result artifacts + aggregate coverage
```

## Contract summary

| Type | Role |
| --- | --- |
| `AuditDocument` / `UntrustedExistingCitation` | Externally supplied prose and optional markup. The markup is retained but never becomes evidence. |
| `ClaimRecord` / `ClaimSegmentation` | Stable claim IDs, exact UTF-8 source offsets, and a document-hash binding. |
| `EvidenceCandidate` / `RetrievalStrategy` | Opaque candidate identity and recorded strategy; candidates are not support. |
| `ResolvedEvidence` / `EvidenceRegistry` | Server-resolved source text, source hash, and locator; duplicate IDs are rejected. |
| `EvidenceReference` | Model/adaptor proposal with only an ID and a verbatim quote — no client/model-supplied locator. |
| `EvidenceClassification` | Fixed `supported`, `partially_supported`, `contradicted`, `unrelated`, or `insufficient` result domain. |
| `ClaimAuditResult` | Per-claim evidence, missing requirements, conflicts, source applicability, and calibration status. |
| `ClaimCoverageEnvelope` | Hash-bound aggregate counts and complete audited-claim coverage. |
| `CitationAuditBudget` / `CitationAuditUsage` | Checked caps for claims, candidates, classifications, cost units, and elapsed time. |
| `CitationAuditRun` | Versioned JSON envelope retaining only artifact hashes for text-bearing values. |
| `CitationAuditWorkflow` | Async adapter trait: `decompose → retrieve_candidates → classify → validate → aggregate`. |

## Fail-closed rules

1. A `ClaimRecord` must exactly equal its audited document byte slice and use
   UTF-8 character boundaries. Segmentation binds to the SHA-256 document hash.
2. Existing citations are **untrusted input**. They are never consulted by
   `EvidenceRegistry` or `ClaimAuditResult::validate_for_claim`.
3. Every evidence-bearing result resolves each opaque ID in the registry and
   requires each proposed quote to occur exactly in that server-resolved text.
   Unknown IDs or altered quotations produce `EvidenceRejected`.
4. Output never accepts or creates source text or locators. The only locator is
   owned by `ResolvedEvidence`, produced by a future server adapter.
5. `supported`, `partially_supported`, `contradicted`, and `unrelated` require
   validated evidence with classification-specific applicability/gap/conflict
   constraints. `insufficient` has no evidence reference and must state a gap.
6. A coverage envelope requires one validated result for every segmented claim,
   with document/segmentation/results hashes recomputed deterministically.
7. `AuditTextOrigin::{DocumentBody, EvidenceText, ModelIntermediate}` cannot
   alter workflow control. This preserves the injection boundary at an adapter
   edge; this pure contract invokes no tools.
8. Unknown run schema versions and budget overflow fail closed. A revision is
   residual and, if later requested, must pass the separate WORKFLOW-005
   `grounded_answer` publication gates.

## What this slice wires

- Pure serializable schemas and validation only; no Store/SQL/filesystem types
- Typed stage machine, budget/error taxonomy, run JSON round-trip/hash binding
- Server-resolution boundary for evidence IDs, quotes, and locators
- Focused unit coverage for all five classifications, source offsets,
  fabricated/altered citations, prompt-injection origins, coverage, run JSON,
  legal transitions, and checked budgets

## Residual work

- Live retrieval/model/daemon/CLI wiring and execution policy
- Multi-strategy retrieval orchestration, rankings, and candidate collection
- Minimum-model, independent judge, and human-evaluation benchmarks
- Persistence adapters and answer-revision publication implementation
- Issue-state changes
