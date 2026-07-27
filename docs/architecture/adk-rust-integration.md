# ADK-Rust integration boundary

Status: first walking-skeleton contract for [#365](https://github.com/RyderFreeman4Logos/verbatim/issues/365).
Code: `crates/verbatim-core/src/adk_integration/`.

## Purpose

This module defines the policy boundary for Verbatim's **optional** ADK-Rust
Agent and workflow adapters. It records the selected public crate decisions,
prevents ADK implementation types from entering Verbatim's durable/public
domains, and accepts only exact stable 1.x versions. It is deliberately pure:
this slice adds no ADK-Rust dependency, runtime, provider call, storage access,
or daemon wiring.

```text
verbatim-domain / verbatim-core / retrieval / storage / public artifacts
                              |
                     stable Verbatim SDK/API
                              |
                     future verbatim-adk adapter
                              |
                future optional ADK-Rust workflows
```

## Catalog policy

`AdkCrateCatalog::standard()` contains all selected crates and validates exact
coverage, the approved disposition, and its required constraints:

| Crate | Disposition | Boundary constraint |
| --- | --- | --- |
| `adk-core` | Adopt | Agent/Llm/Tool/Event/context abstractions only |
| `adk-agent` | Adopt | Agent implementations only |
| `adk-model` | Wrap | Provider clients behind Verbatim endpoint profiles |
| `adk-tool` | Adopt | Function tools, schemas, and scopes only |
| `adk-runner` | Adopt | Execution/event runtime only |
| `adk-graph` | Adopt | Bounded workflow DAGs only |
| `adk-session` | Wrap | Workflow session only; never a Verbatim session |
| `adk-artifact` | Wrap | Workflow artifacts only; never `EvidenceStore` |
| `adk-auth` | Wrap | OIDC/JWT/RBAC plumbing only; never data-plane ACL |
| `adk-telemetry` | Wrap | Use Verbatim spans |
| `adk-guardrail` | Adopt | Supplemental guardrails only |
| `adk-eval` | Adopt | Workflow evaluation only |
| `adk-rag` | Wrap | Generic providers only; never source truth |
| `adk-memory` | Keep | Agent-memory adapter only; never `EvidenceStore` |
| `adk-server` | Keep | Optional sidecar only; never canonical daemon |
| `adk-action` | Wrap | Capability-whitelisted only |
| `adk-sandbox` | Upstream | Requires platform-security conformance before adoption |
| `adk-mistralrs` | Keep | Optional adapter only |

`adk-sandbox` may transition from `Upstream` to `Adopt` only when its catalog
entry records `PlatformSecurityConformance::Verified`; pending evidence is a
closed failure.

## Fail-closed boundaries

`AdkIntegrationContract::check_boundary` rejects these crossings:

1. Persisted Verbatim artifacts using ADK-only schemas.
2. Public Verbatim wire APIs exposing ADK-internal structs.
3. Built-in ADK workflows accessing SQLite, Tantivy, HNSW, or Qdrant directly.
4. Agent/tool scopes replacing source or chunk ACL enforcement.
5. An `adk-graph` workflow replacing the GraphRAG knowledge graph.

The only recognized allowed crossing in this contract is a stable Verbatim
adapter. It must translate into Verbatim domain/API types rather than export
ADK implementation types.

## Version and serialization policy

`AdkIntegrationContract::pin_version` accepts only canonical exact stable
`1.x.y` releases, for example `1.0.0`. It rejects ranges, pre-release/build
suffixes, git URLs, `main`, and non-1.x releases. `VersionPolicy` stores the
parsed numeric version instead of retaining an untrusted source specification.

The catalog and version policy have encode/decode helpers. Decode revalidates
all invariants; JSON parse, policy, and serialization failures expose only a
closed `AdkIntegrationDiagnosticCode`, never caller-controlled input.

## What this slice wires

- Serializable crate catalog, disposition enum, constraints, and sandbox
  conformance guard
- Explicit `DomainBoundaryRule` and `AdkBoundaryUse` values
- `AdkIntegrationContract` with disposition validation, boundary checks, and
  exact version pinning
- Diagnostic-code-only errors and validated JSON round trips

## Residual work

- Conformance evaluation and supply-chain review for exact ADK-Rust releases
- `verbatim-adk` adapters that translate to stable Verbatim domain/API types
- Optional workflow runtime wiring, provider adapters, telemetry spans, and
  capability enforcement
- Any live storage, daemon, public API, or CLI integration
