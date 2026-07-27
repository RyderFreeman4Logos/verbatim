# Bounded grounded-answer workflow (WORKFLOW-005)

Status: walking skeleton for
[#356](https://github.com/RyderFreeman4Logos/verbatim/issues/356).
Code: `crates/verbatim-core/src/grounded_answer/` (module facade + stage /
claim / citation / answer / policy / run / workflow / error units).

## Problem

Users who configure a text-model endpoint need a **safe convenience path** that
retrieves evidence, builds context, generates a draft, verifies claims, and
either publishes a grounded answer or abstains. Without an explicit stage
pipeline and fail-closed contracts, model timeouts, malformed schemas, invalid
citations, and policy denials can surface as apparently verified answers —
violating source-bounded publication rules and R/A/G separation.

## Design direction

Define a pure typed stage machine and persistence envelope. Reuse public
wire/SDK/pagination contracts from API-002 / API-003 / SDK-001. Never access
Store or daemon internals. Model failure degrades to **typed abstention**, never
to an unconstrained or partially verified answer.

Pipeline:

```text
QueryPlan
  → EvidencePack
  → ContextPack
  → AnswerPlan / draft
  → claim verification
  → deterministic citation rendering
  → GroundedAnswer  |  abstention
```

## Contract summary

| Type | Role |
| --- | --- |
| `WorkflowStage` | `planned` → `retrieving` → `assembling` → `generating` → `verifying` → `rendering` → `published` / `abstained` |
| `WorkflowTransition` / `advance_stage` | Legal state-machine edges (incl. one bounded `revise_once`) |
| `WorkflowError` | `validation` / `illegal_transition` / `policy_denied` / `verification_failed` / `model_failure` / `missing_evidence` / `budget_exhausted` / `disabled` |
| `ClaimId` / `DraftClaim` / `ClaimVerdict` | Claim-level draft + verification |
| `ClaimSupportClass` | `supported` / `partial` / `conflict` / `unsupported` / `non_factual` |
| `QuotationCheck` / `QuotationCheckStatus` | ID + quotation support checks |
| `ClaimVerificationReport` | Aggregate verdicts; `all_publishable` gates render/publish |
| `CitationStyle` / `render_citations` | Deterministic citation labels (bracketed sequential or evidence-unit id) |
| `AnswerPlan` / `AnswerDraft` | Generation plan + unverified draft |
| `GroundedClaim` / `GroundedAnswer` | Publishable claims only + citation bijection |
| `PolicyGate` / `PolicyDecision` / `WorkflowPolicyContext` | Trait + types only (engine residual) |
| `WorkflowRun` | Persistence envelope: stages, hashes, fingerprints, costs, warnings, final status |
| `GroundedAnswerWorkflow` | Async trait surface for adapters (no live impl in this slice) |
| `WorkflowOutcome` | `Published` / `Abstained` / `Disabled` |

### Fail-closed rules

1. Only `ClaimSupportClass::Supported` with `QuotationCheckStatus::Match` is
   publishable.
2. `GroundedAnswer` requires non-empty claims and a claim↔citation bijection;
   text must equal `citations.rendered_text`.
3. `try_publish` requires the run to be in `rendering` (or already published).
4. `fail_closed` maps model/verify/policy failures to `Abstained` (or
   `Disabled` when no model endpoint is configured).
5. Unknown `WorkflowRun.schema_version` fails validation/decode.
6. Empty / whitespace digests, ids, and required strings are rejected.
7. Disabling the workflow must not affect R/RA paths (`WorkflowError::Disabled`).

### Layering

| Layer | Module | Notes |
| --- | --- | --- |
| Wire contracts | `wire_schemas` | QueryPlan / EvidencePack / ContextPack / WorkflowEnvelope |
| Pagination | `pagination` | Snapshot-bound search pages (retrieve path residual) |
| Public SDK | `sdk` | `VerbatimClient` ops for R/A/G + workflow request envelopes |
| This contract | `grounded_answer` | Stage machine, claims, citations, run envelope, trait |
| Live generate | `generate` | Existing daemon path — **not** rewritten in this slice |

Adapters should implement `GroundedAnswerWorkflow` in a non-capped module and
call only public SDK/wire types. Do not grow `store.rs`, daemon `main.rs`, or
CLI `client.rs` solely to adopt this contract.

## What this slice wires

- Module export from `verbatim-core` (`pub mod grounded_answer`)
- Pure stages, claim verification, citation rendering, answer artifacts
- `WorkflowRun` persistence envelope + JSON encode/decode helpers
- `GroundedAnswerWorkflow` trait + pure `advance_stage` / `fail_closed` helpers
- Unit tests: transitions, fail-closed abstention, round-trips, bijection
- Architecture note (this file)

## What this slice does **not** do (residual)

- Live model integration, SSE/streaming, daemon/CLI/ADK wiring
- Policy engine implementation beyond types/trait hooks
- Hybrid retrieval/rerank profiles and bounded retry orchestration
- Full in-body citation marker substitution (footer rendering only)
- Benchmark execution (Qwen / unanswerable / injection cases)
- Closing epic #356
