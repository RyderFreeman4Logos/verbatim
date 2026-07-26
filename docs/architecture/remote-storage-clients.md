# Remote storage and index clients (DIST-005)

Status: walking skeleton for
[#351](https://github.com/RyderFreeman4Logos/verbatim/issues/351).
Code: `crates/verbatim-core/src/remote_storage_client/` (module facade +
identity/bounds/idempotency/outcome/compat/trace/stream/request units).

## Problem

Split-host deployment cannot treat network calls as local function calls.
Coordinator and storage/index services need authenticated remote clients with
deadlines, backpressure, idempotent mutations, generation fences, and explicit
partial-failure behavior — layered on the narrow ports from DIST-004 (#350).

## Contract summary

| Type | Role |
| --- | --- |
| `RemoteClientIdentity` / `RemoteServicePrincipal` / `ServiceRole` | Authenticated service principal (no secrets on the wire type) |
| `RequestBounds` / `RequestDeadline` / `PayloadLimits` / `ConcurrencyBound` / `QueueBound` / `CancellationToken` | Deadlines, payload ceilings, concurrency/queue bounds, cancel correlation |
| `IdempotencyKey` / `MutationKind` / `RetryClass` / `RetryPolicy` | Mutation keys and safe vs unsafe retry classification |
| `RemoteStatus` / `RemoteOutcome` / `PartialResultMeta` | Typed unavailable, timeout, conflict, stale-generation, unauthorized, unsupported, partial-result |
| `map_remote_outcome_to_storage_error` | Project non-success remote statuses onto `storage_ports::StorageError` |
| `CompatibilityOffer` / `CompatibilityWindow` / `ProtocolVersion` / `SchemaVersion` | Protocol/schema negotiation; fail closed on unsupported versions |
| `RemoteTraceCarrier` / `TracePropagationMode` | Trace propagation hooks over `observability_contract::TraceContext` |
| `BoundedPageRequest` / `RangeReadRequest` / `StreamReadRequest` / `StreamChunkHint` | Streaming/range-read and bounded pagination request shapes |
| `RemoteRequestEnvelope` / `RemoteOperation` | Composite pre-flight envelope for every remote call |
| `REMOTE_STORAGE_CLIENT_SCHEMA_VERSION` / `REMOTE_STORAGE_CLIENT_PROTOCOL_VERSION` | Wire identity; unknown versions fail closed |

### Design principles

1. **Layer on narrow ports.** Client semantics sit above `storage_ports`; they do
   not replace Catalog/Evidence/Blob/Search/Task/Publish traits.
2. **No transport in this slice.** HTTP/gRPC, TLS, and reverse-proxy policy are
   residual adapters.
3. **Fail closed on auth.** Unauthenticated identities cannot enumerate or fetch;
   reader roles cannot mutate.
4. **Idempotency before retry.** Upsert/publish/enqueue/finish are safe only with
   an idempotency key; claim is never blindly retryable; delete/read are safe.
5. **Partial is explicit.** `RemoteStatus::Partial` requires `PartialResultMeta`
   and must not be silently mapped to a complete success or generic error.
6. **Negotiate, then refuse.** Protocol/schema windows with no overlap return
   `StorageError::Unsupported` (fail closed).
7. **Bounds reject zero/absurd values** at construction so adapters never inherit
   unbounded payloads or infinite concurrency.

### Mapping to `StorageError`

| Remote status | Storage error |
| --- | --- |
| `Unavailable` | `StorageError::Unavailable` |
| `Timeout` | `StorageError::Timeout` |
| `Conflict` | `StorageError::Conflict` |
| `StaleGeneration` | `StorageError::StaleGeneration` |
| `Unauthorized` | `StorageError::Unauthorized` |
| `Unsupported` | `StorageError::Unsupported` |
| `NotFound` | `StorageError::NotFound` |
| `InvalidRequest` | `StorageError::InvalidRequest` |
| `Ok` / `Partial` | not errors — mapping refuses so partial markers stay visible |

### Pre-flight envelope

Every remote call is described by `RemoteRequestEnvelope`:

1. Validate schema, bounds, retry policy, compatibility offer.
2. `authorize_preflight` — refuse unauthenticated enumerate/fetch and unauthorized
   mutations; honor cancellation tokens.
3. Optionally attach `RemoteTraceCarrier`, expected `StorageGeneration`, bounded
   page, or stream/range shapes.
4. Transport adapter (later slice) serializes the envelope + payload and maps
   wire faults back through `RemoteOutcome`.

## What this slice wires

- Module export from `verbatim-core` (`pub mod remote_storage_client`)
- Pure contract types for identity, bounds, idempotency, outcomes, negotiation,
  trace hooks, stream/page shapes, and request envelopes
- Mapping helpers onto `storage_ports::StorageError`
- Unit tests: unauthorized enumerate/fetch, typed failures, schema fail-closed,
  retry classification, zero/absurd bounds rejection

## What this slice does **not** do (residual)

- Real HTTP/gRPC transport, TLS, mTLS, or reverse-proxy wiring
- Network filesystem access for SQLite
- Daemon/CLI behavior changes beyond the module export
- Growing capped monolith files (`store.rs`, `main.rs`, `client.rs`)
- Rolling multi-version compatibility matrix beyond the current major
- Closing epic #351

## Integration notes

When a later slice owns a transport adapter, construct
`RemoteRequestEnvelope` at the coordinator boundary, negotiate
`CompatibilityOffer` once per connection, require
`RemoteClientIdentity::require_authenticated` before any list/get, attach
idempotency keys for every mutation that is not naturally idempotent, map wire
faults into `RemoteOutcome` (never drop partial markers), then project final
errors with `map_remote_outcome_to_storage_error`. Prefer new non-capped modules
for adapters — do not grow `store.rs`, `main.rs`, or `client.rs` solely to adopt
this contract.
