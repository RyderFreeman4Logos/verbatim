# Upstream-first policy (UPSTREAM-001)

Status: adopted walking skeleton for [#364](https://github.com/RyderFreeman4Logos/verbatim/issues/364).
Machine-readable inventory: [`upstream-substitution-matrix.toml`](./upstream-substitution-matrix.toml).
New-work checklist: [`../templates/upstream-reuse-check.md`](../templates/upstream-reuse-check.md).

## Burden of proof

Generic infrastructure is **upstream-owned by default**. A Verbatim-local
implementation of a generic subsystem requires evidence that available, actively
maintained upstream modules cannot satisfy the required semantics through public
APIs or a thin adapter.

A green compile is not evidence of semantic compatibility. Prefer deep modules
with public API surface over shallow wrappers or abandoned crates.

## Permanent Verbatim ownership

Verbatim permanently owns the versioned meaning and invariants of:

```text
LogicalSource / SourceSnapshot / SourceLocation
EvidenceUnit / EvidenceId / source selectors
QueryPlan / RetrievalProfile / RetrievalRun
EvidencePack / ContextPack
index and graph generations
completeness and abstention statuses
authorization decisions over sources and chunks
DerivedGraphArtifact
draft / verified / approved / published states
GroundedAnswer and source-bounded publication rules
```

Owning these contracts does **not** require owning every runtime, database
client, workflow engine, tokenizer, parser, retry loop, transport auth
implementation, telemetry exporter, or evaluation harness.

## Disposition taxonomy

For each generic subsystem or substantial custom module, record exactly one
disposition in the substitution matrix:

| Disposition | Meaning |
| --- | --- |
| `ADOPT` | Use an upstream public API directly. |
| `WRAP` | Use upstream behind a small Verbatim anti-corruption adapter. |
| `UPSTREAM` | Implement the missing generic capability as an upstream issue/PR, with a temporary local shim. |
| `KEEP` | Retain a Verbatim implementation because it encodes product-specific semantics or measured upstream incompatibility. |
| `DELETE` | Remove redundant local code after migration and conformance. |

### KEEP requirements

Every `KEEP` row must include all of:

1. **Dated decision** — `reviewed_on` (ISO-8601 date).
2. **Evidence** — non-empty `rationale` that cites product contracts or measured
   incompatibility (not “we already wrote it”).
3. **Owner** — `owner` accountable for maintenance and future reconsideration.
4. **Reconsideration trigger** — non-empty `reconsider_when` describing when the
   KEEP must be reopened (upstream release, conformance suite land, ADK-001
   milestone, major release audit, etc.).

`KEEP` may be recommended by an implementation agent, but an independent
architecture/reuse reviewer must approve it before merge.

## How to choose a disposition

1. Inventory the subsystem’s public contracts and product invariants.
2. Search maintained Rust crates and primary upstream projects.
3. Score candidates on maintenance activity, contributor diversity,
   release/stability policy, public API depth, security process, tests/fuzzing,
   performance evidence, license, MSRV, dependency weight, and exit strategy.
4. Prefer individual crates and minimal feature sets over umbrella dependencies.
5. Define conformance suites around Verbatim semantics before replacement.
6. Contribute generally useful missing capabilities upstream when practical.
7. Keep every local compatibility shim small, isolated, documented, and paired
   with a deletion issue or removal condition.
8. Never copy private upstream modules or bind Verbatim persistence/wire formats
   to unstable implementation types.

## Autonomous-development rules

- Implementation agents may recommend `KEEP`; independent architecture/reuse
  review must approve it.
- A green compile is not semantic compatibility evidence.
- Before deleting local code, tests must compare old/local and upstream-backed
  behavior on the same fixtures.
- Agents must not weaken an invariant or benchmark solely to make an upstream
  migration pass.
- Dependency updates and semantic migrations must remain separable and
  bisectable.

## Supply-chain recording (SUPPLY-001)

This policy points at supply-chain expectations; it does **not** implement the
full `SUPPLY-001` program.

When adopting or wrapping upstream crates, record (or ensure existing tooling
records) artifact hashes, enabled features, licenses, advisories, MSRV, and
upgrade evidence under the supply-chain process. Prefer `cargo deny` and lockfile
commits already required by local gates; expand SBOM/signature coverage under
`SUPPLY-001` rather than inventing a second ledger here.

## Re-audit cadence

Re-run the substitution audit:

- before large refactors that touch generic infrastructure;
- at each major release;
- when a KEEP row’s `reconsider_when` condition becomes true.

Validate the matrix with:

```sh
bash scripts/tests/upstream-matrix-tests.sh
```

## Non-goals

- Do not outsource Verbatim’s evidence, provenance, completeness, authorization,
  or publication truth.
- Do not adopt abandoned or shallow wrappers merely because they are external.
- Do not fork an upstream project unless contribution and adapter paths have been
  exhausted and the fork has an explicit maintenance budget.
- This document does not migrate the agent/workflow stack to ADK (see deferred
  work below).

## Deferred acceptance criteria (follow-ups)

This walking skeleton closes the policy, matrix, KEEP-field enforcement, and
upstream-reuse checklist slices of #364 / `UPSTREAM-001`. The following remain
explicitly deferred:

- Full workflow/agent stack migration under `ADK-001` before built-in workflows
  ship.
- Mass `DELETE` of redundant local modules after conformance, security, and
  resource regression gates.
- Complete inventory of every source module (matrix covers identified generic
  subsystems; row-level depth continues to grow with audits).
- R-only and RA-only minimal dependency footprint programs.
- Full `SUPPLY-001` enforcement beyond the existing deny/lockfile baseline.

## Related work

- Parent/coordination: `ROADMAP-004`
- Supply-chain enforcement: `SUPPLY-001`
- Autonomous merge governance: `AUTODEV-001`
- First mandatory adoption program: `ADK-001`
