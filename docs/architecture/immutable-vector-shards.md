# Immutable SSD-native vector shards

Status: first walking-skeleton contract (Refs #373).

Code: `crates/verbatim-core/src/vector_shards/`.

## Decision

Verbatim's SSD-native vector-retrieval target requires immutable, bounded-size
shards so that **disk growth is linear within documented constant factors** while
**online memory and open-file state stay bounded**. This contract defines the
physical shard, identifier, file-layout, manifest, routing, and compaction model.
It is deliberately pure: no live SSD I/O, no DiskANN3 dependency, no upstream
binding, no filesystem, daemon, or provider integration.

A shard contains many sources and tenants subject to authorization policy.
**One-index-per-source is prohibited** as the general design because it causes
file/handle proliferation and unbounded query fan-out as source count grows.

## Shard identity

Shards are keyed by stable operational dimensions, not by individual source:

```text
vector-space identity
+ document encoder profile
+ index schema/layout version
+ publication generation
+ shard ordinal/hash range
```

`ShardId` binds a `ShardVectorSpace`, a nonzero `ShardGeneration`, and a bounded
`ShardOrdinal`. Source, collection, tenant, ACL, lifecycle, language, date,
document type, and generation are indexed filter attributes (`Attributes` role),
not shard keys.

## Bounded shard policy

`StorageGrowthBound` configures a maximum vector count and maximum physical bytes
per shard. A build planner creates another shard before either limit is exceeded.
Limits are chosen through benchmarked SSD queue depth, page locality,
build/recovery time, and memory behavior — not arbitrary source boundaries.

## Manifest as the stable contract

`ShardManifest` is the stable contract. It lists every file in the shard with its
size, stable `ShardFileRole`, and `FileHash` (SHA-256). **Exact file names are
implementation details; the role-tagged, hash-verified file set is the stable
surface.** The manifest revalidates:

- shard identity and generation match,
- vector count is nonzero and within the growth bound,
- at least one file per required role (`Vectors`, `GraphPages`, `CandidateCodes`,
  `IdMap`, `Tombstones`, `Attributes`),
- no duplicate file names,
- every file passes its own size and hash validation.

## Complexity requirements

Documented and tested upper bounds in terms of `N` vectors, dimension `D`, fixed
graph degree `R`, candidate-code bytes `Q`, and metadata `M`:

```text
original vectors:  O(N * D)
graph/pages:       O(N * R)
candidate codes:   O(N * Q)
metadata/filter:   O(N + M)
manifests/maps:    O(N)
```

`StorageGrowthBound` exposes these growth classes and validates that the byte
ceiling covers the vectors floor (`N * D * 4`). No component may contain all
source pairs, tenant pairs, vector pairs, or per-source copies of a global graph.

## Routing and identifiers

`ShardRouter` holds a small, bounded set of `GenerationDescriptor`s — never
corpus-scale metadata. Online memory is `O(generations)`, not `O(shards)` or
`O(sources)`. `select()` chooses shards whose manifest generation is compatible
with the query, capped at a hard `max_fan_out`, sharing one `deadline_micros`
deadline. Exact small-scope scans receive shards sorted by ordinal so they can
use contiguous vector extents or sorted ID runs.

- Compact stable numeric IDs inside shards (`ShardOrdinal`).
- Versioned, checksummed mapping to chunk identity (`IdMap` file role).
- Large routing/filter state belongs on SSD or compressed immutable structures.
- A query selects a compatible generation/shards before vector search.

## Build and recovery

`ShardBuildCheckpoint` records a bounded, resumable build. Builds run in a
separate process/cgroup from online serving and use streaming batches. Before a
shard is marked `Complete`, `FsyncAttestation` must confirm that both data files
and directory metadata were `fsync`ed. The checkpoint contract enforces:

- `Complete` requires full fsync durability (`data_fsynced && dir_fsynced`).
- Any stage at or past `Fsyncing` requires full durability.
- Progress (`vectors_streamed`) must be positive once past `StreamingData`.
- Generation must match the shard's generation.

On recovery, the builder resumes from the last durable checkpoint or quarantines a
partially-written shard. Old generations are retained while queries/cursors hold
leases, then garbage-collected in bounded batches. Publication flows only through
[#379](https://github.com/RyderFreeman4Logos/verbatim/issues/379).

## Fail-closed contract surface

All validation rejects invalid input. `VectorShardError` is diagnostic-code-only:
no variant retains a caller-controlled identifier, file name, checksum, tenant,
ACL, or source. Public `Debug` and `Display` emit only the closed code, so
failures are safe to surface in operational diagnostics. Nonzero generations and
ordinals are enforced through constructors even when deserialized (no zero bypass
via serde).

Decode helpers (`decode_shard_manifest_json`) revalidate decoded manifests before
returning them, so untrusted JSON cannot bypass constructor invariants.

## Out of scope for this slice

- Live DiskANN3 bindings, `DataProvider`, SSD files, or graph build.
- Live Qdrant, LanceDB, SQLite, or HNSW adapter wiring.
- Catalog migrations, daemon/API/CLI routes, distributed serving, metrics export.
- Actual build workers, publication (#379), or garbage collection execution
  (the contract only guarantees their required crash-safe boundary).
