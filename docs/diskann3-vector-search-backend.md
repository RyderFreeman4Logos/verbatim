# DiskANN3 VectorSearch backend adapter contract

`verbatim_core::diskann3_backend` is a **crate-owned walking-skeleton adapter
contract**, not a DiskANN3 implementation or public provider-extension API. It
defines the validated inputs, operation surface, observable capabilities, and
fail-closed diagnostic vocabulary that a future crate-owned DiskANN3 integration
must honor before it can be used as a `storage_ports::VectorSearch`.

## Boundary ownership

| Layer | Owns | Does not own |
| --- | --- | --- |
| `diskann3` | Publication generations, shard identities, predicate types, and architecture-specific invariants | An upstream ANN dependency, native lifecycle, or SSD I/O |
| `diskann3_backend` | The crate-owned anti-corruption adapter contract, validated adapter operations, mapping/version bindings, capabilities, and diagnostics | A concrete `DataProvider`, daemon, page-cache implementation, upstream types in public APIs, or an out-of-crate provider extension boundary |
| `storage_ports::VectorSearch` | The repository-standard dense search capability used by callers | DiskANN3-specific lifecycle, recovery, and exact-rescore details |

`DiskAnnVectorSearch` extends `storage_ports::VectorSearch`, but is deliberately
sealed and crate-owned: only `verbatim_core` may implement it. The trait records
the walking skeleton's internal adapter boundary; it is not a downstream provider
plugin contract. A concrete crate-owned backend therefore remains reachable
through the standard repository port while its DiskANN3-specific operations stay
behind one explicit contract boundary.

### Page issuance and future extensions

`SearchCandidate` and `SearchPage` are opaque public result types, but their
issuance constructors are crate-private. Public callers can consume an issued
page but cannot forge a candidate or page, and an out-of-crate provider cannot
implement `DiskAnnVectorSearch` to issue one. A future out-of-crate provider
needs a separately designed issuer boundary whose crate-owned side validates raw
provider output and materializes pages. That boundary is not part of this slice;
this contract intentionally exposes no public `SearchCandidate` or `SearchPage`
factory.

## Invariants at the boundary

Every externally constructible operation request is validated before an adapter
can receive it:

- Vectors are exactly `4096` `f32` components, all finite.
- Vectors carry an `EmbeddingProfileId` and `PublicationGeneration`; both must
  match the `GenerationContext` and `VectorSpaceSpec` exactly.
- Cosine vectors must have nonzero norm and be unit-normalized within the
  declared tolerance. Dot-product and L2 vectors preserve magnitude.
- A `SearchBudgetBinding` validates both budgets and rejects an operation budget
  that is wider than caller authority.
- `PredicatePlan` is bounded and requires predicates to constrain candidate
  generation, not merely post-filter an already generated result set.
- `ChunkIdMapping` binds a versioned `StableVectorId -> ChunkId` mapping to the
  selected vector space, embedding profile, and publication generation.
- Mutation batches and exact-rescore candidate sets are bounded, generation
  consistent, and idempotent where state is changed.

The contract accepts no implicit default vector space, profile, generation,
metric, score interpretation, or resource budget.

## Required crate-owned adapter operations

A crate-owned `DiskAnnVectorSearch` implementation must provide contract operations for:

1. staging, building, loading, and validating a shard generation;
2. idempotent batch upserts and stable-ID tombstones;
3. predicate-aware Top-K and raw-distance range search;
4. exact original-vector fetch for final rescoring;
5. generation status and capability discovery;
6. bounded page-cache diagnostics;
7. snapshot/restore or reproducible rebuild from authoritative vectors; and
8. deterministic shutdown with a resource-release receipt.

The current slice intentionally provides only request, response, capability,
and error types. It does **not** create a provider, ANN index, SSD page layout,
background daemon, or hidden local-state cache.

## Full-quality result rule

Compression may be used only to generate candidates. `FullQualityGuarantee`
requires all of the following before a provider can claim full quality:

- the original 4,096-dimensional `f32` vectors remain accessible on SSD;
- every final candidate is eligible for an exact original-vector fetch; and
- final scoring occurs from that original representation.

`CandidateScore` keeps `raw_distance` separate from `normalized_score` and
labels both with `VectorMetric`. A raw L2 distance, a cosine distance, and a
dot-product-derived score are not interchangeable. Normalized scores are only
meaningful under the adapter's declared metric policy; they are not a license
to compare unrelated metric domains.

## Capability and diagnostic rules

`DiskAnnCapabilities` explicitly advertises supported metrics, predicate-aware
search, Top-K/range behavior, exact fetch, mutation/tombstone support,
snapshot/rebuild support, deterministic shutdown, page/cache/byte limits, and
the full-quality proof. Operation budgets may not exceed that envelope.

`PageCacheDiagnostics` publishes bounded aggregate counters only. It must not
expose page keys, filesystem paths, raw vector values, internal handles, or
unbounded diagnostic records.

All `DiskAnnBackendError` rendering is code-only, such as
`diskann3-backend.profile_mismatch`. Errors deliberately omit user text,
chunk IDs, vector values, filter values, idempotency keys, provider paths, and
upstream messages. The opaque `IdempotencyKey` type also redacts its `Debug`
representation.

## Future upstream adoption and pinning policy

This contract intentionally adds **no** upstream DiskANN3 dependency or pin.
When a concrete integration is proposed, the owner must first record an audited,
immutable upstream release/tag and source revision, package name and enabled
features, license, checksum or source provenance, MSRV compatibility, unsafe
and FFI surface, security/advisory review, benchmark evidence, and rollback
plan. Floating branches, unbounded `latest` selection, private provider
formats in public contract types, and direct leakage of upstream errors are not
acceptable.

The concrete integration should translate upstream inputs and failures at the
adapter boundary, retain Verbatim's authoritative original-vector and
stable-ID mapping rules, and introduce its dependency/pin in a dedicated
follow-up change.

## Non-goals

This slice does not select an ANN algorithm, quantify recall, define a graph
layout, cache policy, SIMD/FFI implementation, SSD format, deployment model,
or automatic migration path. Those choices belong to the future concrete
provider and must satisfy this contract rather than expand it implicitly.

Refs #372
