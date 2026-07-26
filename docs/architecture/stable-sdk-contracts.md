# Stable SDK client trait contracts (SDK-001)

Status: walking skeleton for
[#355](https://github.com/RyderFreeman4Logos/verbatim/issues/355).
Code: `crates/verbatim-core/src/sdk/` (module facade + config / capability /
error / ops / client / cursor_iter units).

## Problem

Applications, agents, skills, and ADK workflows need a **stable typed client**
for the public R/A/G artifact API. Binding to unstable JSON shapes, CLI stdout,
or daemon-internal crates couples third parties to release-to-release churn and
leaks Store/SQL/filesystem types across the process boundary.

## Design direction

Make the Rust SDK trait surface authoritative for native integration. Keep
transport adapters separate from domain helpers. Reuse wire envelopes from
API-002 (`wire_schemas`) and snapshot-bound pagination from API-003
(`pagination`). Capability discovery fails closed on unsupported features.

## Contract summary

| Type | Role |
| --- | --- |
| `SdkConfig` | Endpoint, optional auth token (Debug-redacted), timeout, user-agent, capability cache |
| `SdkCapabilityKind` | Client-facing capability classes (search, retrieve, generate, workflow, …) |
| `SdkCapabilityDescriptor` | Advertised capability set + optional protocol/wire majors |
| `CapabilityCache` | Last successful negotiation stored on `SdkConfig` |
| `CapabilityNegotiation` | Required ∩ advertised; soft `require` preflight |
| `ClientError` | Typed transport / auth / validation / unsupported / compatibility / not_found / timeout / pagination |
| `VerbatimClient` | Async trait: capabilities, source/upload, search, retrieve, resolve, evidence, context, generate, verify, workflow, task, artifact |
| Operation envelopes | Pure request/response DTOs (no Store/SQL types) |
| `CursorIterator` / `CursorPageFetcher` | Typed multi-page walker over `SnapshotPageRequest` |

### Capability negotiation

1. Client calls `discover_capabilities` (or loads a warm `CapabilityCache`).
2. `CapabilityNegotiation::negotiate` intersects required capabilities with the
   advertised set.
3. Missing required capabilities return `ClientError::Unsupported` (not a
   silent no-op).
4. Per-operation `require_capability` maps the same failure class before
   transport.

### Fail-closed rules

- `SdkConfig` rejects non-http(s) endpoints, zero timeouts, empty user-agent /
  tokens, and whitespace hosts.
- Capability schema version must equal `SDK_CAPABILITY_SCHEMA_VERSION` (1).
- Operation envelopes re-validate nested wire envelopes and digests.
- `SearchRequest` requires `query_plan_hash == page.query_plan_hash`.
- `CursorIterator` refuses mode or publication-generation mismatches; exhausted
  pages clear `next_cursor`.

### Layering

| Layer | Module | Notes |
| --- | --- | --- |
| Wire contracts | `wire_schemas` | QueryPlan / EvidencePack / ContextPack / Derived / Workflow |
| Pagination | `pagination` | Snapshot-bound cursors + page envelopes |
| Storage ports | `storage_ports` | Internal backend ports — **not** re-exported as SDK surface |
| Remote clients | `remote_storage_client` | Split-host storage semantics — separate from public SDK |
| This contract | `sdk` | Public client trait + config + errors + iterators |

Adapters should implement `VerbatimClient` in a non-capped module. Do not grow
`store.rs`, daemon `main.rs`, or CLI `client.rs` solely to adopt this contract.

## What this slice wires

- Module export from `verbatim-core` (`pub mod sdk`)
- Pure config, capability negotiation, typed errors, operation envelopes
- `VerbatimClient` trait + `CursorIterator` over pagination types
- Unit tests: construction, JSON round-trip, negotiation fail-closed, R/A/G
  envelope construction, cursor walk + mismatch, stub trait round-trip
- Architecture note (this file)

## What this slice does **not** do (residual)

- Real HTTP/gRPC transport, TLS, retries, SSE/progress streams, cancellation
- OpenAPI/JSON Schema generated clients in other languages
- Live daemon/CLI adoption and example programs (R-only, RA-only, ADK)
- Semver deprecation windows and multi-version compatibility matrix
- Closing epic #355
