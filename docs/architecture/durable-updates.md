# Durable DiskANN3 updates, deletes, tombstones, compaction, and crash recovery (VECTOR-SSD-007)

Status: walking skeleton for [#378](https://github.com/RyderFreeman4Logos/verbatim/issues/378).
Code: `crates/verbatim-core/src/durable_updates/`.

## Purpose

This is a pure contract module for the durable update lifecycle required for
enterprise RAG on DiskANN3: idempotent vector inserts, replacements, source
deletion, tombstones, long update streams, bounded compaction, and crash
recovery. It defines typed mutation, tombstone, compaction, lease, and recovery
boundaries. It deliberately contains no live SSD I/O, no DiskANN3 binding, no
compaction daemon, no filesystem, and no provider integration. It verifies the
update capability under Verbatim's generation, filter, and durability contracts
before a future adapter wires it to a real index.

```text
MutationOperation (Upsert | Delete | Tombstone | SourceReplace)
  → MutationBatch (bounded, idempotent, version-ordered)
  → MutationStage lifecycle:
      OperationLogged → VectorUpserted → Tombstoned
      → GraphEdgeUpdated → FilterIndexUpdated → Checkpointed
      → Compacted → Validated → Published
  → CrashRecoveryResult (PreviousCommitted | NewCommitted | InconsistentRejected)
```

## Mutation lifecycle

Every mutation operation carries a content-aware identity (`DurableVectorId`,
`MutationVersion`, optional `ContentHash`) and an opaque `MutationIdempotencyKey`
whose `Debug` form is redacted. A `MutationBatch` is bounded (≤ 10 000
operations), idempotent (no two operations in one batch target the same vector
id), and version-ordered: `validate_against_committed` rejects any operation
whose version regresses relative to the last committed version for that vector.

The `MutationStage` enum tracks the lifecycle through nine stages, mirroring the
issue's crash model. A crash may occur at operation-log append, vector/page
write, adjacency update, filter index update, checkpoint, compaction, validation,
or publication. A stage is durable only from `Checkpointed` onward, and search-
visible only at `Published`.

## Tombstone policy

A `TombstoneSet` excludes tombstoned ids **before hydration**: a candidate whose
`vector_id` is tombstoned in the search's generation at a version ≥ the
candidate's index version is removed without fetching its payload. Tombstones are
generation- and version-aware:

- A tombstone recorded in generation *G* is invisible to searches bound to *G-1*
  (which may still be served under a live query lease).
- A tombstone for version *V* does not suppress a vector re-inserted at *V+1*.
- Recording the same `(generation, version)` is idempotent; an older version is
  rejected.

Tombstone/delta memory is capped (`TombstoneSet::DEFAULT_CAP = 100 000`).
Reaching the cap triggers compaction rather than unbounded growth.

## Compaction policy

Compaction is triggered by measured signals — not wall-clock time alone. A
`CompactionTrigger` compares four measured values against `CompactionThresholds`:

| Signal | Default threshold |
| --- | --- |
| dead-byte ratio | 0.20 |
| read amplification (SSD pages / candidate) | 4.0 |
| update volume (mutations since last compaction) | 50 000 |
| p99 query latency | 50 000 µs |

`should_compact()` returns `true` if **any** signal exceeds its threshold. A
`CompactionPlan` is resumable and restart-safe: it records the source and target
generations, the current `CompactionStage`, and whether the staged immutable
artifact has been fsynced. A plan claiming `Staged` without fsync is rejected
(`CheckpointNotDurable`). Old pages are reclaimed only after their
`MutationLease` expires; `can_reclaim_generation` blocks reclamation while any
live lease protects the generation.

## Crash model

`CrashRecoveryResult::decide` inspects the last observed stage and fsync
attestation:

- `Published` + fully durable fsync → `NewCommitted` (the new generation is live).
- `Published` without full fsync → `InconsistentRejected` (the manifest lies
  about durability; the shard is quarantined and rebuilt from authoritative
  vectors).
- Any durable-but-unpublished stage (`Checkpointed`, `Compacted`, `Validated`) →
  `PreviousCommitted` (the previous committed generation survives).
- Any pre-checkpoint stage → `PreviousCommitted`.

Recovery may yield the previous committed state or the complete new committed
state. It may **never** yield a search-visible mixture whose manifest claims
success but whose data is partially written.

## Source replacement

`validate_source_replace_atomicity` enforces that old and new chunks are never
exposed together under one active generation. The replacement is atomic: old
vector ids must be retired (tombstoned) in the same generation that publishes the
new ids, respecting ACL, lifecycle, and generation visibility.

## Fail-closed rules

1. All validation rejects invalid input. Errors are diagnostic-code-only: no
   variant retains a caller-controlled identifier, vector, content hash, tenant,
   ACL, or source.
2. Public `Debug` and `Display` emit only the closed code (`durable-updates.<code>`).
   `MutationIdempotencyKey` and `MutationBatch` redact the key value.
3. No `unwrap`, `expect`, or `panic` in production code.
4. A `CompactionPlan` claiming `Staged` requires fsync; a `CrashRecoveryResult`
   claiming `Published` requires full fsync or it is rejected.
5. Reclamation before lease expiry is rejected (`LeaseActive`).

## What this slice wires

- Pure, serializable mutation, tombstone, compaction, lease, and recovery types
- Typed diagnostic-only validation errors with redacted `Debug`/`Display`
- Fail-closed idempotency, version-ordering, tombstone cap, compaction-trigger,
  lease, and crash-recovery contracts
- Focused unit coverage for mutation idempotency, version ordering, tombstone
  generation awareness, compaction triggers, crash recovery states, and lease
  expiry

## Residual work

- Live SSD I/O, DiskANN3 binding, and operation-log persistence
- Compaction daemon scheduling under a separate resource budget
- Injection of process termination and I/O errors during each lifecycle stage
- Recall drift, graph connectivity, filter correctness, latency, SSD
  amplification, peak memory, and storage-growth benchmarking for long streams
- Referential validation against authoritative catalog/evidence state
- Rebuild-from-authoritative-vectors repair path
- Issue-state changes
