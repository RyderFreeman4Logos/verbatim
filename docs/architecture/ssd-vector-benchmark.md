# SSD vector benchmark contract

Status: first walking-skeleton contract for [#382](https://github.com/RyderFreeman4Logos/verbatim/issues/382) / EVAL-SSD-001.

Code: `crates/verbatim-core/src/ssd_vector_benchmark/`.

Focused tests: `just test-f ssd_vector_benchmark`.

## Purpose

This module makes the full-dimensional SSD vector benchmark **decision-falsifying**, not ceremonial. It encodes closed types, fail-closed validation, deterministic local-subset plan/run, gate evaluation, and machine-readable plus Markdown report emission so that:

1. Dimension reduction cannot sneak into acceptance runs.
2. Exact full-dimensional ground truth and original f32 final scoring are mandatory.
3. Cold/warm cache states and cgroup memory accounting are first-class fields.
4. Every backend comparison binds identical vectors, filters, budgets, qrels digests, and final scoring policy.
5. Storage-growth verification requires N and source-count series.
6. A required **reference** backend complete-gate win forces `ArchitectureDecisionMustBeReconsidered` rather than silent ignore.
7. Regression-only backends (SQLite scan, instant-distance HNSW) cannot promote alone.

Live provider runners (Qdrant/LanceDB/Milvus processes, 1M/10M corpus generation, real cgroup orchestration) are **intentionally not live yet**. Harness inputs may inject recorded measurements for tests and future adapters.

## Relation to SSD-VECTOR-ADR-001

See `docs/architecture/ssd-native-vector-adr.md` for the DiskANN3-first architecture decision. This benchmark contract is the evidence surface that can **falsify** that decision under the complete quality/latency/memory/SSD/update gate suite.

Versioned example matrix and gates:

- `docs/config/benchmark_matrix.toml`
- `docs/config/acceptance_gates.toml`

Those TOML files remain example/config documentation. The Rust module is the mechanically enforceable contract.

## Systems under test

| Backend | Role | Required | Notes |
| --- | --- | --- | --- |
| DiskANN3 standard | primary_candidate | yes | Primary SSD-native design |
| DiskANN3 AISAQ colocated-performance | primary_candidate | yes | Low-DRAM experiment layout |
| DiskANN3 AISAQ colocated-scale | primary_candidate | yes | Scale-oriented colocated layout |
| Exact full-dimensional flat scan | exact_baseline | yes | Ground-truth / baseline |
| Qdrant reference | reference | yes | Can force architecture reconsideration |
| LanceDB IVF_RQ | reference | yes | Can force architecture reconsideration |
| LanceDB IVF_PQ | reference | yes | Can force architecture reconsideration |
| SQLite scan | regression_only | yes | Cannot promote alone |
| instant-distance HNSW | regression_only | yes | Cannot promote alone |
| USearch HNSW | external_control | no | Outside Verbatim process budget |
| Milvus AISAQ | external_control | no | Outside Verbatim process budget |

## Corpus / query / metrics

**Corpus scales** include bible/canonical fixture, enterprise collections, synthetic 1M/10M, opaque enterprise-style (#271), and a small deterministic local-subset synthetic corpus. All acceptance runs require dimension **4096** and exact full-dimensional f32 ground truth for final scoring.

**Query matrix** dimensions: query class, filter selectivity, concurrency, cache state (cold/warm required), update state. Candidate quality gates are separate from final rescore quality gates.

**Quality metrics**: candidate and final Recall@K, nDCG, MRR, filtered authorized-subset recall, rank correlation, top-k overlap, locator accuracy, update-stream drift.

**Resource metrics**: latency percentiles, throughput, **cgroup memory.current / high / max** (unknown = fail), page faults, SSD bytes/ops, index bytes.

## Gate policy and promotion coupling

`HardGatePolicy::program_default()` mirrors the essential invariants in `docs/config/acceptance_gates.toml`:

- no dimension reduction;
- exact full-dimensional ground truth;
- candidate vs final quality gated separately;
- hard cgroup memory cap;
- cold and warm required;
- storage-growth series required;
- missing measurement = fail.

Suite verdicts:

| Verdict | Meaning |
| --- | --- |
| `pass` | Primary candidates satisfy complete gates; no reference complete-gate win |
| `fail` | Hard gate failure or incomplete evidence |
| `architecture_decision_must_be_reconsidered` | A reference backend won the complete gate suite |

Promotion under [#379](https://github.com/RyderFreeman4Logos/verbatim/issues/379) is expected to consume these reports as regression/blocking evidence. This module does not implement the promotion workflow itself.

## Local subset vs full suite

`LocalSubsetPlan::deterministic_default` selects:

- bible/canonical + small synthetic local corpus identity;
- required backends catalog (optional controls omitted);
- cold + warm semantic scenarios;
- program-default hard gates;
- minimal storage-growth series.

`LocalSubsetPlan::run_with_injected` requires every backend × scenario measurement cell, evaluates gates fail-closed, and emits `BenchmarkReport` (JSON-serializable) plus `to_markdown()`.

The full 1M/10M multi-backend live suite remains future work; the contract types are ready to bind those runners without weakening invariants.

## Contract surface

| Module | Purpose |
| --- | --- |
| `error` | Closed diagnostic codes; Display/Debug code-only |
| `system` | Backend catalog, roles, required flags |
| `identity` | Closed labels, digests, comparison identity |
| `corpus` | Corpus scales, ground truth, storage growth |
| `query_matrix` | Query classes, selectivity, concurrency, cache/update |
| `metrics` | Quality stages/metrics + cgroup resource metrics |
| `gate` | Hard-gate evaluation and suite verdicts |
| `report` | Report + hardware profile + Markdown emission |
| `run` | Local-subset plan and injected measurement runner |

`SSD_VECTOR_BENCHMARK_CONTRACT_SCHEMA_VERSION` is currently `1`.

## What is intentionally not live yet

- Network clients for Qdrant, LanceDB, Milvus, or USearch
- Real cgroup create/attach and page-cache drop orchestration
- 1M/10M corpus generation and enterprise opaque corpora
- Daemon/CLI command wiring for a one-shot operator binary
- Automatic promotion decisions (#379)

Adapters must inject measurements through `InjectedScenarioMeasurement` (or future equivalent ports) without bypassing constructor validation or serde revalidation.

## Serialization and privacy

Public types re-run constructor validation on `Deserialize`. Error `Display` is `ssd-vector-benchmark.<code>` only. Labels reject path-like fragments. Digests are opaque hex-like tokens. No raw query text, evidence, or user paths appear in report Markdown or diagnostics.

## Focused verification

```bash
just test-f ssd_vector_benchmark
just clippy-p verbatim-core
```
