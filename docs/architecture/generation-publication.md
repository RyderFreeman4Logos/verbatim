# Atomic DiskANN3 generation publication and migration contract (VECTOR-SSD-008)

Status: walking skeleton for [#379](https://github.com/RyderFreeman4Logos/verbatim/issues/379).
Code: `crates/verbatim-core/src/generation_publication/`.

## Purpose

This is a pure contract module for the atomic publication and migration model
required for DiskANN3-backed enterprise RAG. It defines the publication
manifest, lifecycle, pointer, lease, coordinator-lock, quarantine,
rollback-durability, and dual-generation migration types that guarantee a query
binds to one complete, compatible publication generation. It deliberately
contains no live SSD I/O, no DiskANN3 binding, no migration daemon, no
filesystem, and no provider integration. It verifies the atomic publication
capability under Verbatim's generation, filter, and durability contracts before
a future adapter wires it to real vector backends.

## Relationship to existing modules

- [`index_publication`](index-publication-manifests.md) (#352) owns the generic
  cross-index publication manifest, promotion coordinator, and reconciliation
  report. This module extends that mechanism with **vector-backend-specific**
  fields and the DiskANN3 lifecycle.
- [`durable_updates`](durable-updates.md) (#378) owns the mutation, tombstone,
  compaction, and crash-recovery lifecycle. This module references durable
  update checkpoints via `UpdateCheckpoint`.
- [`vector_shards`](immutable-vector-shards.md) owns immutable shard manifests
  and build checkpoints. This module references shard descriptors via
  `ShardDescriptor`.
- [`diskann3`](diskann3-architecture.md) owns the DiskANN3 shard, retrieval, and
  filter contracts.

## Lifecycle

```text
authoritative snapshot fixed
  → stage lexical/vector/filter/graph artifacts
  → fsync / checksum / validate
  → run conformance and sampled quality gates
  → create publication manifest
  → atomically promote active pointer
  → serve queries bound to that generation
  → retain old generation for leases / cursors / rollback
  → bounded garbage collection
```

`PublicationStage` tracks each generation through eight states:

| Stage | Durable | Search-visible |
| --- | --- | --- |
| `SnapshotFixed` | no | no |
| `Staging` | no | no |
| `Validating` | no | no |
| `Ready` | yes | no |
| `Active` | yes | yes |
| `Retained` | yes | no (readable under lease) |
| `GarbageCollected` | yes | no |
| `Quarantined` | — | no |

`validate_stage_transition` enforces the legal forward transitions. Only `Ready`
generations may be promoted to `Active`; only `Active` generations serve
queries. A generation may be quarantined from `Validating`, `Ready`, or
`Retained`, but never promoted from `Quarantined`.

## Publication manifest

`PublicationManifest` is the comprehensive vector-backend publication document.
It captures:

- **Generation identity**: `PublicationGenerationId`, `vector_space_id`,
  `encoder_profile_id`.
- **Vector geometry**: `dimension`, `metric` (Cosine / InnerProduct / L2),
  `normalization`, `original_vector_encoding` (float32 / SQ / PQ).
- **Backend identity**: `provider` (DiskAnn3Standard / DiskAnn3Aisaq / Qdrant /
  LanceDB), `candidate_quantizer`.
- **Shard list**: each `ShardDescriptor` carries ordinal, vector-id range,
  count, byte size, graph degree, and a validated `sha256:` checksum.
- **Build parameters**: `graph_max_degree`, `build_search_list_size`.
- **Integrity hashes**: `exact_vector_hash`, `id_map_hash` (original float32
  vectors and the external-id ↔ internal-id map).
- **Filter / ACL binding**: `FilterAclBinding` (filter schema version, ACL
  policy generation).
- **Update checkpoint**: `UpdateCheckpoint` (last durable mutation version,
  tombstone generation at seal time).
- **Sampled recall**: `SampledRecallReport` (recall@10 mean and minimum over a
  sampled query set; required before promotion).
- **Build resources**: `BuildResourceReport` (peak memory, build duration, SSD
  bytes written, CPU-seconds).
- **Compatibility**: `CompatibilityContract` (DiskANN3 version, source revision,
  minimum reader version).
- **Seal metadata**: `stage`, `sealed_at`.

`validate()` enforces every structural invariant: nonzero dimension, non-empty
shard list, no duplicate shard ordinals, byte-size lower bound (≥
`vector_count × dimension × 4` for float32), validated hashes, bounded graph
degree, sampled recall range, and resource positivity.

## Pointer and promotion

`PublicationPointer` is the atomic CAS boundary: `(active_generation, epoch,
previous_generation, updated_at)`. Promotion and rollback advance the epoch and
swap the active generation. The previous generation is retained for one-step
rollback.

## Lease tracking and GC gating

`LeaseRegistry` tracks outstanding `GenerationLease` entries per generation. A
generation is reclaimable (eligible for GC) only when it has no outstanding
non-expired leases. `prune_expired` removes expired leases; `has_active_leases`
gates GC.

## Coordinator exclusivity

`CoordinatorLockRegistry` prevents two coordinators from promoting different
generations concurrently. A lock is acquired under a `(coordinator_id, epoch,
target_generation)` triple. A different coordinator or different target
generation is rejected with `CoordinatorLocked`.

## Migration (dual-generation evaluation)

`MigrationProfile` shadows the incumbent and candidate generation under mirrored
sampled queries with independent metrics (`MigrationCandidateMetrics`: recall,
p99 latency, peak memory, read amplification). Supported backend pairs include:

- SQLite scan / HNSW → DiskANN3
- Standard DiskANN3 → AISAQ-style provider
- DiskANN3 version / layout upgrades
- Qdrant / LanceDB comparison or emergency fallback
- Embedding / document-encoder profile changes (#280)

**No default fusion.** `FusionPolicy::None` is the default — old and new backend
results are never mixed in one response. `FusionPolicy::Experiment` is an
explicit opt-in recorded by the experiment profile.

## Quarantine

`QuarantineRegistry` isolates incomplete or corrupt generations after startup
reconciliation. A quarantined generation cannot be promoted under a newer
evidence / ACL generation (`conflicts_with_newer_generation`). Double-quarantine
is rejected with `QuarantineConflict`.

## Rollback durability

`RollbackReceipt` proves a rollback was durable across restart. It carries the
demoted generation, restored generation, epoch, fsync attestation, and
timestamp. A rollback without full fsync (both data and directory) is rejected
with `RollbackNotDurable`.

## Mixed-index read rejection

`reject_mixed_generation_read` enforces that a query binds to exactly one
publication generation. Two different generation ids in one query path is
rejected with `MixedGenerationRead`. DiskANN3 graph, vectors, filters, ID map,
and evidence / lexical generations cannot be mixed.

## Fail-closed rules

1. All validation rejects invalid input. Errors are diagnostic-code-only: no
   variant retains a caller-controlled identifier, vector, content hash,
   tenant, ACL, shard id, embedding profile, or manifest path.
2. Public `Debug` and `Display` emit only the closed code
   (`generation-publication.<code>`). `ContentHash` redacts its value.
3. No `unwrap`, `expect`, or `panic` in production code.
4. A `PublicationManifest` claiming `Ready` requires validated hashes and a
   sampled recall report meeting the promotion threshold.
5. A `RollbackReceipt` requires full fsync or it is rejected.
6. Reclamation before lease expiry is gated by `has_active_leases`.
7. Two coordinators cannot promote different generations concurrently.

## Diagnostic codes

| Code | Meaning |
| --- | --- |
| `invalid_identity` | Zero or malformed generation / epoch / lease id |
| `invalid_bounds` | Out-of-range dimension, shard ordinal, recall, or byte size |
| `invalid_hash` | Malformed `sha256:` content hash |
| `invalid_contract` | Empty required field, unknown schema version, self-migration |
| `invalid_stage_transition` | Illegal lifecycle stage change |
| `missing_component` | Empty shard list or zero total vectors |
| `duplicate_shard` | Two shards with the same ordinal |
| `pointer_conflict` | CAS mismatch on promotion |
| `coordinator_locked` | Another coordinator holds the promotion lock |
| `mixed_generation_read` | Query referenced two different generations |
| `staging_not_durable` | Staged artifacts lack fsync attestation |
| `rollback_not_durable` | Rollback lacks full fsync |
| `quarantine_conflict` | Generation already quarantined |
| `incompatible_backend` | Backend provider mismatch on migration |
| `serialization_failed` | JSON encode / decode failure |

## What this slice wires

- Pure, serializable publication manifest, pointer, lease, coordinator-lock,
  quarantine, rollback, and migration types
- Typed diagnostic-only validation errors with redacted `Debug` / `Display`
- Fail-closed lifecycle transitions, lease GC gating, coordinator exclusivity,
  mixed-read rejection, and rollback durability contracts
- Focused unit coverage for all contract types

## Residual work

- Live SSD I/O, DiskANN3 binding, and manifest persistence
- Multi-process coordinator fencing beyond the in-memory registry
- Startup reconciliation daemon wiring
- Cache / cursor key generation inclusion
- Remote shard / service partial-state handling
- No-downtime cutover orchestration
- Issue-state changes

Refs #379
