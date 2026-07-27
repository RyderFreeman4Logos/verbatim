# DiskANN3 SSD-native vector retrieval architecture

Status: first walking-skeleton contract for [#369](https://github.com/RyderFreeman4Logos/verbatim/issues/369).
Code: `crates/verbatim-core/src/diskann3/`.

## Decision

Verbatim's vector-retrieval target is **DiskANN3-first**: original 4,096-dimensional
`float32` embeddings are retained, while SSD-resident graph and candidate-vector
structures provide the working set. Online retrieval has explicit per-request and
global memory, I/O, concurrency, deadline, and stage-output limits. The current
contract is deliberately pure: it adds no DiskANN3, Qdrant, LanceDB, SQLite, HNSW,
filesystem, service, or provider integration.

`DiskAnn3` is the sole `Primary` backend. `Qdrant` and `LanceDb` are `Reference`
backends used to **falsify** the primary design through comparable correctness,
recall, filtering, update, and resource measurements; they are not stepping stones.
`SQLite` full-vector scan and `HnswLegacy` (`instant-distance`) are `Legacy`: both
require explicit opt-in and remain deprecated during the cutover.

## Authoritative data and derived index flow

SQLite/PostgreSQL-compatible catalog and evidence stores remain authoritative.
Every lexical or vector index is derived, versioned, checksummed, and reproducibly
rebuildable.

```text
Authoritative catalog + evidence + original float32 embeddings
                         |
                         v
       deterministic build / validate / checksum / publish generation
                         |
                         v
  immutable SSD shard manifests (space, generation, shard ordinal, layout)
                         |
              atomic generation publication / rollback
                         |
                         v
query + ACL/tenant/source/lifecycle/language/date/metadata filters
                         |
                         v
 retrieval planner: exact SIMD scan when strict filtered set <= threshold,
 otherwise bounded DiskANN3 candidate traversal with predicate pushdown
                         |
                         v
 full-precision rescore -> filter application -> bounded fusion/RRF -> rerank
                         |
                hard truncate before hydration
                         |
                         v
 bounded hydration from authoritative evidence/catalog stores
```

A reader receives one `PublicationGeneration`; generation mismatch is a closed
failure, so it cannot combine old and new shards during publication or rollback.
Shard manifests bind every bounded shard to its named `VectorSpaceId`, generation,
vector count, 4,096 dimension, byte size, graph degree, quantizer, page layout, and
checksum. Future DiskANN3 `DataProvider` work owns real SSD I/O and may use the
AISAQ-style co-located page layout recorded by this contract.

## Implementation requirements

The following ten implementation requirements from the architecture decision govern
all child implementations:

1. Retain full original 4,096-dimensional `float32` vectors on SSD.
2. Keep the graph SSD-resident with only a bounded in-memory directory/cache state.
3. Permit compressed candidate-generation representations only without dimension
   reduction of the original vector.
4. Perform full-precision original-vector rescoring before final ranking.
5. Use predicate-aware traversal for enterprise source, collection, tenant, ACL,
   lifecycle, language, date, and typed metadata filters.
6. Select exact SIMD scan when a strict filter produces a candidate set at or below
   `ExactScanThreshold`; otherwise use bounded ANN traversal.
7. Use immutable bounded-size shards keyed by vector-space and publication
   generation—never one index per source.
8. Implement staged build, validation, atomic promotion, rollback, and garbage
   collection for derived index generations.
9. Provide a local in-process adapter and a shared-nothing enterprise service with
   the same `VectorSearch` semantics.
10. Enforce hard cgroup and per-request memory/I/O/deadline gates and measure them
    under both cold- and warm-cache states.

## Fail-closed contract surface

`VectorSearchContract` records bounded `search`, `rescore`, and `hydrate` adapter
operations. `VectorSearchPolicy` validates the 4,096 finite-float query, hard
budgets, and bounded filter predicates. `BoundedCandidates` rejects tombstones and
mixed publication generations, enforces a cap at every `RetrievalStage`, and makes
fusion truncation explicit. The diagnostic-only `VectorSearchError` has no
caller-controlled detail in `Debug` or `Display`.

The serializable contract covers `VectorDimension`, `VectorSpaceId`,
`PublicationGeneration`, `ShardId`, `SsdShardManifest`, backend roles, budgets,
filters, exact-scan planning, staged candidates, and closed diagnostics. Decode
helpers revalidate decoded manifests, budgets, and candidates before returning them.

## Migration and cutover

```text
SQLite full-vector scan  ->  resident instant-distance HNSW  ->  DiskANN3 SSD-native
       Legacy/deprecated          Legacy/deprecated                 Primary
       explicit opt-in            explicit opt-in                   default target
```

Migration is a gated cutover, not a silent compatibility bridge. Build and compare
new generations against the reference backends; publish only a validated generation;
retain a previous immutable generation for atomic rollback. Legacy backends remain
available only behind explicit operator opt-in until their removal criteria are met.

## Out of scope for this slice

- Live DiskANN3 bindings, `DataProvider`, AISAQ page reader, SSD files, or graph build
- Live Qdrant, LanceDB, SQLite, or HNSW adapter wiring
- Catalog migrations, daemon/API/CLI routes, distributed serving, metrics export, or
  benchmark execution
- Actual update/delete workers (the contract only guarantees their required boundary)
