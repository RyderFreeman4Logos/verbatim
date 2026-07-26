# Snapshot-bound cursors and pagination (API-003)

Status: walking skeleton for
[#354](https://github.com/RyderFreeman4Logos/verbatim/issues/354).
Code: `crates/verbatim-core/src/pagination/` (module facade + cursor / page /
error / idempotency units).

## Problem

Ranked search can change while a client paginates: source updates, reranking
profile changes, ACL/policy changes, approximate index rebuilds, or publication
promotion. Plain `page` / `page_size` offsets silently duplicate, omit, or leak
results across principals and generations. Mutation retries without idempotency
keys also risk double work.

## Design direction

Seal an **opaque cursor** to the exact query identity, principal, publication
generation (`QueryPublicationBinding`), ranking/profile ref, policy version,
pagination mode, last stable sort key, and expiry. Continuations fail closed on
any mismatch. Keep **ranked search** and **exhaustive enumeration** as distinct
modes. Record mutation outcomes under principal-scoped idempotency keys so
retries return one logical operation.

## Contract summary

| Type | Role |
| --- | --- |
| `CURSOR_SCHEMA_VERSION` | Claims schema; unknown versions fail closed |
| `CursorClaims` | Canonical sealed fields + embedded `QueryPublicationBinding` |
| `CursorSealKey` | Server-only key material (never on the wire) |
| `seal_cursor` / `open_cursor` | `v1.<base64url(claims)>.<hex_seal>` with keyed SHA-256 seal |
| `validate_cursor_continuation` | Expiry, principal, generation, profile, policy, query, mode |
| `CursorError` | `expired` / `invalid` / `unauthorized` / `generation_gone` / `generation_mismatch` / `profile_changed` / `policy_changed` / `query_mismatch` / `mode_mismatch` |
| `PaginationMode` | `ranked_search` vs `exhaustive_enumeration` |
| `SnapshotPageRequest` / `SnapshotPageResponse` | Mode-aware page envelopes bound to a generation |
| `MutationIdempotencyKey` | Client mutation identity (≤256 bytes) |
| `InMemoryIdempotencyRegistry` | Pure claim → complete registry (Fresh / InProgress / Replay) |

### Cursor wire form

1. Serialize `CursorClaims` to compact JSON bytes.
2. `seal = hex(SHA-256(key || 0x00 || claims_bytes))` (walking-skeleton MAC).
3. Wire: `v1.<base64url(claims_bytes)>.<seal>`.
4. Open: decode, constant-time seal check, structural validate.
5. Continue: `validate_cursor_continuation` against the live request context.

Clients cannot edit sealed fields. Wrong key, payload flip, or truncated wire
form maps to `CursorError::Invalid`.

### Fail-closed rules

- Pagination **never** silently crosses query, policy, principal, profile, mode,
  or generation boundaries.
- Expired cursors return `expired` (recoverable: restart from page 0).
- Request publication binding ≠ sealed cursor generation returns
  `generation_mismatch` (maps to `StorageError::InvalidRequest`). This is a
  binding error; it must **not** invent `StaleGeneration.actual` from the
  request id.
- Bound generation no longer readable / not retained returns `generation_gone`:
  - observed available known (`available: Some`) → maps to
    `StorageError::StaleGeneration { expected: bound, actual: observed }`
  - observed available missing (`available: None`) → maps to
    `StorageError::Unavailable` (do not invent `StorageGeneration::INITIAL`)
- Principal mismatch returns `unauthorized`.
- Ranked and exhaustive cursors are not interchangeable (`mode_mismatch`).

### Mutation idempotency

| Claim outcome | Meaning |
| --- | --- |
| `Fresh` | First sight of (principal, key); execute then `complete` |
| `InProgress` | Claimed but not completed; do not double-apply |
| `Replay` | Completed; return stored `MutationResultToken` |

Reusing the same key with a different operation fingerprint is a conflict.

### Layering

| Layer | Module | Notes |
| --- | --- | --- |
| Storage ports | `storage_ports::{PageCursor,PageRequest,PageResponse,StorageError,StorageGeneration}` | Generic page types + errors |
| Publication | `index_publication::QueryPublicationBinding` | Generation fence embedded in claims |
| Remote client | `remote_storage_client::IdempotencyKey` / `RetryPolicy` | Transport retry classification |
| This contract | `pagination` | Snapshot-bound seal + mode envelopes + registry |

Adapters should project domain search/list results into
`SnapshotPageResponse` and seal `next_cursor` with a server key. Do not grow
capped monoliths (`store.rs`, `main.rs`, `client.rs`) solely to adopt this.

## What this slice wires

- Module export from `verbatim-core` (`pub mod pagination`)
- Pure cursor seal/open/validate + page envelopes + in-memory idempotency registry
- Unit tests: round-trip, tamper reject, principal mismatch, generation mismatch /
  gone, expiry, profile/policy/query mismatch, ranked vs exhaustive separation,
  idempotent retry, key-reuse conflict
- Architecture note (this file)

## What this slice does **not** do (residual)

- Live store/daemon/HTTP handler wiring
- Real multi-backend keyset stability and generation retention leases
- Production field sets for every search mode / rerank path
- Durable multi-process idempotency store
- Closing epic #354
