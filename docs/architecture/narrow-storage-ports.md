# Narrow storage ports (DIST-004)

Status: walking skeleton for
[#350](https://github.com/RyderFreeman4Logos/verbatim/issues/350).
Code: `crates/verbatim-core/src/storage_ports/` (module facade +
common/catalog/evidence/blob/task_queue/search/publisher units).

## Problem

The daemon and retrieval pipeline currently assume co-location through concrete
`Store` access and rebuild methods that accept `&Store`. A single giant remote
store abstraction would leak transactions, paths, and backend-specific behavior
across the network. Narrow capability-oriented ports make local behavior
testable before introducing transport failure.

## Contract summary

| Type | Role |
| --- | --- |
| `CatalogStore` | Authoritative catalog/collection/source metadata CRUD |
| `EvidenceStore` | Authoritative evidence/chunk storage with pagination and filtering |
| `BlobStore` | Authoritative binary blob storage (no path leakage) |
| `TaskQueue` | Durable async task enqueue / claim / finish / list |
| `LexicalSearch` | Derived BM25/lexical query with pagination |
| `VectorSearch` | Derived embedding similarity search with pagination |
| `GraphSearch` | Derived graph/entity query (stub-ready interface) |
| `IndexPublisher` | Atomic index publication / generation manifests |
| `StorageError` | Typed timeout / conflict / stale-generation / unsupported / not-found / unauthorized / invalid-request / unavailable |
| `StorageCapability` | Capability discovery so unsupported ops fail closed |
| `StorageAuthContext` | Wire-safe principal + optional ACL scope / request id |
| `PageRequest` / `PageResponse` / `PageCursor` | Backend-neutral pagination |
| `StorageGeneration` | Monotonic publication / index generation fence |
| `PublicationManifest` | Checksummed generation publish record |
| `STORAGE_PORTS_SCHEMA_VERSION` | Wire schema; unknown versions fail closed |

### Authoritative vs derived lifecycle

| Lifecycle | Ports | Responsibility |
| --- | --- | --- |
| **Authoritative** (source of truth) | `CatalogStore`, `EvidenceStore`, `BlobStore`, `TaskQueue` | Durable writes; transaction boundaries stay inside the adapter |
| **Derived / rebuildable** | `LexicalSearch`, `VectorSearch`, `GraphSearch`, `IndexPublisher` | Rebuilt from authoritative data; publication fenced by generation |
| **Cached** (out of scope here) | — | Adopt `cache_identity` separately |
| **Ephemeral** | queue leases, in-flight claims | Adapter-local; not part of publication manifests |

### Design principles

1. **Narrow ports, not a remote `Store`.** Each trait is a capability with typed
   request/response objects.
2. **No SQL, rusqlite types, or local paths on the wire.** Adapters own those.
3. **Server-side transaction boundaries.** Callers issue operations, not TX
   handles.
4. **Generation fencing.** Search/publish paths carry `StorageGeneration`;
   stale reads/writes return `StorageError::StaleGeneration`.
5. **Capability discovery.** `StorageCapability::require` fails as
   `Unsupported` instead of panicking or silently no-opping.
6. **Fail closed on unknown schema.** Decode helpers reject foreign
   `schema_version` values.
7. **Authorization context without secrets.** `StorageAuthContext` carries a
   resolved principal kind and optional ACL scope only.

### Error classes

Adapters map every backend failure into:

- `Timeout` — deadline exceeded
- `Conflict` — optimistic concurrency / CAS failure that is not generation-stale
- `StaleGeneration` — publication/index generation fence mismatch
- `Unsupported` — capability or operation missing on this backend
- `NotFound` — missing entity
- `Unauthorized` — ACL/role rejection
- `InvalidRequest` — structural validation (including unknown schema)
- `Unavailable` — temporary backend outage

### Pagination and filtering

List/search operations take `PageRequest { limit, cursor }` and return
`PageResponse { items, next_cursor, total_hint }`. Cursors are opaque strings —
never a SQL `OFFSET` contract. Evidence filters are typed field bags
(`source_id` / `evidence_id` / `chunk_id`), not query strings.

## What this slice wires

- Module export from `verbatim-core` (`pub mod storage_ports`)
- Typed trait definitions for all eight ports plus shared errors/capability
  discovery
- Fault-injection / compliance stubs in unit tests
- Unit tests: error class coverage, capability discovery, schema fail-closed,
  CAS publication / stale generation, stub trait compliance

## What this slice does **not** do (residual)

- Implement in-process SQLite / Tantivy / HNSW / Qdrant adapters
- Wire daemon, retriever, or ingest orchestration onto the ports
- Remove concrete `Store` parameters from `LexicalIndex` / `VectorIndex`
- Touch capped files (`main.rs`, `client.rs`, `store.rs`)
- Introduce a remote transport
- Close epic #350

## Integration notes

When a later slice owns an adapter, implement the relevant port traits on a
non-capped type, advertise capabilities through `StorageCapability`, and share
contract tests with other backends. Prefer adapters in new modules — do not grow
`store.rs`, `main.rs`, or `client.rs` solely to adopt these ports. Keep
authoritative writes and derived rebuilds separate so remote publication can
ship generations without shipping SQLite transactions.
