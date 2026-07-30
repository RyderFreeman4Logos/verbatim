# ADR-SSD-001: DiskANN3-first SSD-native vector retrieval

**Status:** Accepted for the SSD-native vector program

**Decision scope:** enterprise dense retrieval, lexical ownership, reference backends, and promotion evidence

## Context

Verbatim needs dense retrieval that remains useful when the corpus is larger than
online DRAM, while preserving enterprise filter, ACL, update, and recovery
semantics. The target embedding space is the original 4,096-dimensional `float32`
vector space. A design that makes a smaller derived vector the only retained
representation would change the retrieval contract and cannot establish the
program's full-quality claim.

The decision is constrained by all of the following at once:

- bounded online memory, including Linux cgroup v2 page-cache accounting;
- bounded SSD pages, bytes, queue depth, deadline, and concurrency per request;
- predicate-aware candidate generation for tenant, ACL, lifecycle, and metadata
  constraints;
- exact original-vector rescoring before final dense ranking;
- atomic, generation-bound publication and rollback of derived artifacts; and
- reproducible comparisons against exact/reference backends under cold and warm
  cache states on a declared hardware profile.

The first contract slices intentionally do not yet bind a live DiskANN3 provider
or remote vector service. This ADR specifies the architecture and promotion
criteria that those adapters must satisfy; it does not present an unimplemented
adapter as deployed infrastructure.

## Decision

1. **DiskANN3 is the primary enterprise vector provider.** Its working graph,
   candidate representations, and page-oriented access path are SSD-native.
   Shards are immutable, bounded, generation-bound artifacts rather than one
   index per source.
2. **Tantivy remains the primary lexical/BM25 engine.** Lexical behavior is a
   separate contract from dense-vector storage or ANN candidate generation.
3. **AISAQ-style co-located DiskANN3 pages are the primary low-DRAM experiment.**
   They are a DiskANN3 page-layout option, not a separate final architecture and
   not permission to discard original vectors.
4. **Qdrant and LanceDB remain reference backends.** They are retained to
   falsify the primary design through comparable quality, filtering, update,
   recovery, and resource measurements. Neither is a transitional destination.
5. **Exact, reference, graph, and exhaustive retrieval remain separate
   first-class retrievers.** Fusion records their provenance and does not turn an
   approximate result into an exhaustive claim.

### Architecture and data authority

The catalog/evidence/blob/task stores are authoritative. Tantivy, DiskANN3
shards, reference-backend collections, graph artifacts, and caches are derived
and rebuildable. No vector backend is the source of truth for document,
authorization, lifecycle, or evidence payload state.

```text
 authoritative catalog + evidence/blob + task stores + original 4096d float32 vectors
                                      |
                                      | deterministic build, validate, checksum
                                      v
     +--------------------- generation-bound derived artifacts ---------------------+
     | Tantivy lexical index | DiskANN3 SSD shards | Qdrant/LanceDB references      |
     | ACL/filter structures | exact/reference data | manifests + publication pointer |
     +----------------------------------+---------------------------------------------+
                                        |
                         atomic publication / rollback, one generation per query
                                        |
                                        v
 query + tenant/ACL/source/lifecycle/date/typed-metadata constraints
                                        |
                                        v
 +--------------------- retrieval planner ------------------------+
 | zero authorized scope -> return without vector page I/O        |
 | small authorized scope -> exact full-dimensional scan          |
 | medium authorized scope -> PlannerSelected: exact by default   |
 |   when independent exact budget fits; otherwise fail closed    |
 | broad authorized scope -> predicate-aware DiskANN3 traversal   |
 | unsupported strict predicate -> typed fail-closed result       |
 +-------------------------------+--------------------------------+
                                |
                                v
 bounded candidates -> original 4096d float32 exact rescore -> fusion/rerank
                                |
                                v
 authoritative hydration + ACL/lifecycle/tombstone revalidation -> bounded output
```

Every request is bound to one vector-space identity, encoder/profile identity,
layout/schema version, policy generation, and publication generation. A mixed
generation is a closed failure, not a best-effort merge. A shard's manifest must
bind its vector-space identity, generation, ordinal, vector count, dimension,
layout, file roles, sizes, and checksums.

## Quality and retrieval semantics

### Candidate quantization is not embedding dimension reduction

Candidate generation may use product-quantized, scalar-quantized, spherical, or
other bounded representations to reduce candidate I/O and working-set cost. That
is **candidate quantization**, not permission to change the authoritative
embedding dimension. The original 4,096-dimensional `float32` vectors remain on
SSD, are bound to the same vector-space/profile/generation identity, and are
fetched only for the bounded final rescore set.

A backend cannot claim the program's full-quality mode unless both conditions
hold:

1. complete original vectors are available on SSD; and
2. every final dense candidate is exactly rescored from those originals before
   final dense ranking.

Candidate-code distance, graph traversal order, or a compressed representation
is therefore an approximation aid only. It is never the final quality layer.

### BM25 is decoupled from the vector backend

Qdrant and LanceDB offer hybrid or BM25-related capabilities, but selecting one
for vector storage does not establish Verbatim's lexical semantics. The lexical
contract must preserve and test tokenizer behavior, phrase positions, stemming,
stop words, fields and BM25F-like behavior, IDF scope, identifiers, code, CJK,
query parsing, explain/debug behavior, and stable result semantics. Tantivy is
already the primary lexical engine for those requirements.

Dense, lexical, exact/reference, graph, and exhaustive retrievers enter bounded
fusion as named retrievers with separate provenance. A vector backend must not
silently replace Tantivy, reinterpret BM25 results, or treat a backend-specific
hybrid score as lexical conformance.

### Filter and authorization behavior

Authorization and strict metadata constraints participate before or during
candidate generation. The planner classifies the *authorized* cardinality:

| Class | Authorized-cardinality band | Path |
| --- | --- | --- |
| Zero | `0` | Return without vector page I/O |
| Small | `1..=exact_simd_scan_max_matches` | Exact full-dimensional scan |
| Medium | `exact_simd_scan_max_matches+1 .. predicate_aware_diskann3_min_matches-1` | `PlannerSelected` |
| Broad | `>= predicate_aware_diskann3_min_matches` | Predicate-aware DiskANN3 |

The Medium band is intentional when the calibrated thresholds leave a gap. The
predicate layer reports `PlannerSelected`; the search planner currently selects
exact full-dimensional work for that gap only when the independent exact
candidate budget still fits. If the Medium count exceeds that exact budget, the
request fails closed rather than widening exact work, inventing an uncalibrated
ANN path, or falling back to global ANN plus post-filter. Provenance must record
the planner decision and the selectivity class. Global ANN followed by
post-filtering is never an acceptable fallback. An unsupported strict predicate
is a typed, fail-closed result. Returned candidates are revalidated against
authoritative ACL, lifecycle, and tombstone state during hydration.

## Alternatives considered

| Alternative | Strengths and retained role | Why it is not the SSD-primary decision |
| --- | --- | --- |
| **Qdrant** | Mature vector service with filterable HNSW, payload indexes, on-disk/quantization/hybrid capabilities, replication, and established operational tooling. Retained as a reference backend and service control. | Its dense ANN remains HNSW. Under very small memory budgets, graph and vector traversal can create unfavorable cold random-I/O behavior; it is not selected as the theoretical SSD-native low-memory primary without winning the complete gates. |
| **LanceDB** | Strong embedded columnar/vector option with IVF_RQ/PQ, scalar prefiltering, adaptive probes, refinement, and Rust support. Retained as the strongest embedded falsification backend. | It is not an intermediate destination. It must demonstrate the full hard-gated quality, filtering, update, recovery, and resource result before it can displace the primary. |
| **Milvus AISAQ** | Valuable near-zero-DRAM algorithmic and storage-layout inspiration; an external control where a suitable deployment is available. | The full Milvus service has a substantially larger documented minimum memory footprint than this program's online target. The co-located page-layout hypothesis is tested through a DiskANN3 provider rather than adopting the whole service as the primary. |
| **USearch/HNSW** | Useful local/mmap filtered control and a pragmatic comparison point. | It is not the primary SSD-native graph design under the hard online-memory target. It remains useful for measurements and targeted fallback experiments. |
| **sqlite-vec / SQLite scan (exact)** | Simple, local, exact baseline; ideal for small authorized scopes and ground truth. | Broad dense scan is `O(N * D)` and does not constitute a complete enterprise ANN, filter, update, or service architecture. It remains a deliberate exact/reference path. |
| **instant-distance HNSW** | Lightweight prototype and historical baseline. | Its persistence, filtering, update, and memory model do not meet the target. It is legacy, explicit opt-in only during cutover, and not the final architecture. |

## Consequences

### Positive consequences

- The online memory budget is explicit instead of being implicitly determined by
  corpus size or host page cache.
- The architecture keeps full-quality verification possible because originals and
  exact rescoring are mandatory.
- Reference backends make the DiskANN3 decision falsifiable rather than
  self-certifying.
- Lexical conformance remains stable while dense backends are compared.
- Immutable generation-bound shards make publication, rollback, backup, and
  disaster recovery auditable.

### Costs and obligations

- Operators must size shards and SSD capacity with documented linear formulas,
  measure cold and warm behavior, and run isolated build/compaction work.
- A DiskANN3 adapter must implement real page I/O, budget enforcement, typed
  exhaustion, predicate capability reporting, checksummed manifests, and
  recovery behavior before it can be promoted.
- Each comparison must preserve dataset, qrel, vector-space/profile, hardware,
  cache-state, config, binary revision, and report provenance; otherwise its
  performance claim is not promotion evidence.

## Reconsideration trigger

This decision is deliberately reversible by evidence, not preference. Reconsider
it **only when another backend wins the complete hard-gated benchmark and conformance suite**. "Wins" means the same declared corpus/qrels, vector
space/profile, filter and ACL suite, cold and warm hardware profile, update and
recovery scenarios, exact-rescore semantics, memory/SSD/deadline constraints,
and reproducible artifacts--not a single throughput result or a backend-specific
hybrid score.

A candidate that wins must still preserve Tantivy lexical ownership unless it
also separately passes the lexical conformance decision. A reference backend
becoming competitive does not change its role automatically; the complete
promotion record is required.

## Related architecture and operating documents

- [DiskANN3 SSD-native vector retrieval architecture](diskann3-architecture.md)
- [AISAQ-style page layout](aisaq-page-layout.md)
- [Immutable SSD-native vector shards](immutable-vector-shards.md)
- [Enterprise vector predicates](enterprise-vector-predicates.md)
- [Exact filtered scans](exact-filtered-scans.md)
- [Retrieval resource budgets](retrieval-resource-budgets.md)
- [Durable updates, compaction, and crash recovery](durable-updates.md)
- [Atomic index publication manifests](index-publication-manifests.md)
- [Tantivy lexical engine](tantivy-lexical-engine.md)
- [Hybrid fusion](hybrid-fusion.md)
- [Retrieval telemetry](retrieval-telemetry.md)
- [SSD-vector operator playbook](ssd-vector-operator-playbook.md)
- [Enterprise DiskANN3 profile example](../config/diskann3_enterprise.toml)
- [Benchmark matrix example](../config/benchmark_matrix.toml)
- [Acceptance-gates example](../config/acceptance_gates.toml)
