# Atomic index publication manifests (DIST-006)

Status: walking skeleton for
[#352](https://github.com/RyderFreeman4Logos/verbatim/issues/352).
Code: `crates/verbatim-core/src/index_publication/` (module facade +
manifest/pointer/validate/promotion/reconcile/query_binding units).

## Problem

A source replacement may update SQLite evidence, Tantivy, local HNSW, Qdrant,
graph artifacts, and caches at different times. Remote services make partial
publication more likely. Querying mixed generations can return missing chunks,
stale ACLs, or vectors for the wrong embedding profile.

## Design direction

Build derived artifacts in a **staging generation**, validate a hash-bound
**IndexPublicationManifest**, atomically promote an **ActiveGenerationPointer**
(CAS on generation + epoch), bind every query/run/cursor to exactly one
publication generation, and emit **typed reconciliation findings** when
components diverge.

## Contract summary

| Type | Role |
| --- | --- |
| `INDEX_PUBLICATION_SCHEMA_VERSION` | Wire schema; unknown versions fail closed |
| `IndexPublicationManifest` | Full publication record: sources, generations, profiles, digests, ACL/lifecycle policy versions, build status |
| `SourceSnapshotRef` / `ComponentDigest` / `ComponentKind` | Opaque source ids + content digests per component |
| `DeclaredCapabilities` | Which of evidence/catalog/lexical/vector/graph this generation publishes |
| `BuildStatus` | `Building` / `Validating` / `Ready` / `Active` / `RolledBack` / `Quarantined` / `Failed` — only `Ready` may promote |
| `ActiveGenerationPointer` / `PointerEpoch` | Durable single-active pointer with CAS epoch |
| `validate_*` / `validate_for_promotion` | Pure completeness, profile, referential integrity, hash integrity, status gates |
| `PublicationCoordinator` / `InMemoryPublicationCoordinator` | stage → validate → promote (CAS) → rollback |
| `PromotionOutcome` / `PromotionConflict` | Typed success / reject / concurrent stale conflict |
| `ReconciliationFinding` / `ReconciliationReport` | Divergent component, missing chunk, hash mismatch, stale ACL, orphan generation |
| `QueryPublicationBinding` | Binds query/run/cursor to one `publication_generation` |

### Layering vs storage ports

| Layer | Type | Notes |
| --- | --- | --- |
| DIST-004 ports | `storage_ports::PublicationManifest` | Thin wire record for `IndexPublisher` (checksum + generation) |
| DIST-006 contract | `index_publication::IndexPublicationManifest` | Full multi-component generation identity and promotion SM |
| Shared | `StorageGeneration`, `StorageError` | Generation fence + fail-closed errors |

Adapters and coordinators should project the rich manifest into the thin port
shape when publishing through `IndexPublisher`, not invent a third generation id
space.

### Promotion state machine

```
stage(manifest)  -- no active mutation; rejects Active payloads and restage of
   │                 the live generation (pointer id or registry Active row)
   ▼
validate(generation)  -- completeness, profile, referential integrity, digests;
   │                    Ready only (Building/Failed cannot promote); advisory
   │                    check — promote re-validates live (no recorded fence)
   ▼
promote(gen, expected_current, expected_epoch)  -- live re-validate + CAS pointer;
   │                                               conflict → typed stale
   ▼
rollback(expected_current, expected_epoch)  -- restore previous_generation if present
```

### Reconciliation findings

| Kind | Meaning |
| --- | --- |
| `DivergentComponent` | Observed component generation ≠ active manifest |
| `MissingChunk` | Required chunk absent under bound generation |
| `HashMismatch` | Observed digest ≠ declared component digest |
| `StaleAclPolicy` | Component ACL/lifecycle policy older than manifest |
| `OrphanGeneration` | Generation artifact without publication manifest |
| `IncompleteGeneration` | Non-ready generation past lease (placeholder) |

### Query binding

Every query, run, or cursor carries `QueryPublicationBinding` with exactly one
`publication_generation` (and optional pointer epoch). Mid-stream rebinding is
forbidden; incompatible caches/cursors are invalidated when the active pointer
moves (adapter residual).

## What this slice wires

- Module export from `verbatim-core` (`pub mod index_publication`)
- Pure contract types and in-memory coordinator
- Unit tests: happy promote, reject incomplete, hash integrity, concurrent CAS
  conflict, rollback, restage-of-active reject, unknown schema fail-closed,
  reconciliation construction, query binding round-trip

## What this slice does **not** do (residual)

- Real multi-backend promotion across SQLite / Tantivy / HNSW / Qdrant
- Network/coordinator crash recovery beyond types and this design note
- GC lease enforcement beyond type placeholders
- Daemon/router wiring or growing capped monolith files
- Closing epic #352

## Integration notes

Later adapters should:

1. Stage component builds under a non-active generation id.
2. Assemble `IndexPublicationManifest` with digests for every declared capability.
3. Call pure validators before attempting promotion.
4. CAS-update `ActiveGenerationPointer` (generation + epoch); map conflicts to
   `StorageError::StaleGeneration`.
5. Attach `QueryPublicationBinding` to every query/run/cursor.
6. Run startup/periodic reconciliation and quarantine incomplete/orphan
   generations until leases expire.

Prefer new non-capped modules for durable coordinators — do not grow
`store.rs`, `main.rs`, or `client.rs` solely to adopt this contract.
