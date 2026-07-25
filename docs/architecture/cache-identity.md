# Cache identity contract (CACHE-001)

Status: walking skeleton for
[#339](https://github.com/RyderFreeman4Logos/verbatim/issues/339).
Code: `crates/verbatim-core/src/cache_identity.rs`.

## Problem

Caching can leak unauthorized or stale data when keys omit principal scope,
authorization scope, query/plan identity, source/index generation, model
fingerprints, trust domain, policy version, or ContextPack hash. Edits,
deletion, ACL changes, lifecycle transitions, model drift, profile changes,
graph rebuilds, and retention updates must invalidate every affected layer.

## Contract summary

| Type | Role |
| --- | --- |
| `CacheIdentity` | Canonical inputs for any cache entry |
| `CacheIdentityFields` | Field bundle for constructing `CacheIdentity` |
| `CacheKey` | Content-addressed key derived from identity; retains match fields |
| `CacheDependencyGraph` | Artifact/generation lineage for fan-out invalidation |
| `InvalidationEvent` | Edit/delete/snapshot/ACL/lifecycle/model/profile/graph/retention |
| `cache_key_matches_invalidation` | Narrow principal-safe match predicate |
| `CACHE_IDENTITY_SCHEMA_VERSION` | Wire schema; unknown versions fail closed |

### Required identity fields

1. `principal` — authenticated caller/tenant identity
2. `acl_scope` — collection/classification authorization scope
3. `query_plan_hash` — deterministic QueryPlan + profile hash
4. `source_generation` — source/index generation fence
5. `model_fingerprint` — served model version hash
6. `trust_domain` — trust classification
7. `policy_version` — cache/lifecycle/retention policy version
8. `context_pack_hash` — hash of the ContextPack / grounded context payload
   (answer cache boundary: hits must not cross distinct packs)
9. `schema_version` — forward-compatible wire version (currently `1`)

Shared reuse is opt-in only when **all** fields are equivalent. Query text
equality alone is never a valid key.

### Invalidation propagation design

Invalidation is field-explicit and principal-safe:

- Principal-scoped events (`Edit`, `Delete`, `Snapshot`, `Lifecycle`, `Acl`)
  match only when both principal and the relevant generation/scope match.
- Global semantic events (`Model`, `Profile`, `Retention`) match on the
  corresponding identity field only.
- `Graph` currently matches through `source_generation` until dedicated graph
  fields are wired into adapters.
- `CacheDependencyGraph` records multi-artifact lineage so future adapters can
  fan out one event to many entries without widening cross-principal reuse.

Decode helpers (`decode_cache_identity_json`, `decode_cache_key_json`,
`decode_cache_dependency_graph_json`) reject unknown schema versions instead of
silently accepting them as current schema.

## Cache classes that will eventually adopt this contract

| Class | Existing touchpoints (not rewired in this slice) |
| --- | --- |
| Query embedding cache | `store_cache.rs` / `embedding_cache` table |
| Retrieval candidates / RRF | retrieve path + task controls (`bypass_cache`) |
| Rerank results | rerank controls / provider adapters |
| Evidence hydration | store evidence span loads |
| ContextPack / answer cache | ask/generate task path |
| Workflow / graph reports | `graphrag` / graph extraction |
| Provider capability cache | OpenAI-compatible provider fingerprinting |

This slice **defines** the contract and tests; it does **not** change live cache
lookup or write paths.

## What this slice wires

- Module export from `verbatim-core` (`pub mod cache_identity`)
- Deterministic key derivation and invalidation match helpers
- Dedicated `context_pack_hash` field fencing answer-cache ContextPack reuse
- Unit tests for principal/ACL/ContextPack isolation, per-field digest isolation,
  invalidation match/non-match (including Edit/Snapshot/Lifecycle/Profile/Graph),
  serde roundtrip, and unknown-schema rejection for identity/key/dependency graph

## What this slice does **not** do (residual)

- Wire existing cache tables or in-memory maps to `CacheKey`
- Remote/local tombstone propagation end-to-end
- Storage TTL, encryption, or hit/miss telemetry surfaces
- Poisoning/stale-generation integration tests against live stores
- Dedicated `InvalidationEvent::TrustDomain` variant (trust isolation is still
  enforced via `content_digest` / key fields; event-driven trust-domain
  invalidation remains residual)
- Closing epic #339

## Integration notes

When a later slice touches a cache class, construct a `CacheIdentity` at the
authorization boundary, derive `CacheKey` once, and store
`CacheDependencyGraph` beside the value. Prefer matching invalidation through
`cache_key_matches_invalidation` rather than ad-hoc string compares. Answer and
ContextPack caches must populate `context_pack_hash` so distinct packs never
share a key under the same principal/query/ACL. Do not grow `store.rs`,
`main.rs`, or `client.rs` solely to adopt this contract; keep adapters in
non-capped modules.
