# LanceDB IVF_RQ / IVF_PQ reference backend contract

Status: VECTOR-REF-002 types-only walking-skeleton contract (Refs [#384](https://github.com/RyderFreeman4Logos/verbatim/issues/384)).

Code: `crates/verbatim-core/src/lancedb_backend/`.

## Purpose and decision boundary

LanceDB is a strong embedded columnar **reference** backend for filtered SSD vector
retrieval. It exists to test whether an embedded IVF implementation can falsify the
DiskANN3-first decision in [ADR-SSD-001](ssd-native-vector-adr.md), not to declare a
live replacement. LanceDB may require reconsideration of that decision only after it
wins the complete quality, latency, memory, SSD, update, and filter matrix gates in
[#382](https://github.com/RyderFreeman4Logos/verbatim/issues/382). Ease of integration
or a partial benchmark is not a promotion signal.

This module is deliberately a typed contract harness. It has no `lancedb` dependency,
live client, table handle, filesystem action, or network I/O. A future crate-owned
adapter must implement the sealed `LanceDbVectorSearch` boundary and preserve every
constructor validation here.

## Full-dimensional table and profiles

Every table identity binds a table name, embedding profile, publication generation, and
configuration digest. The schema accepts only **4,096** dimensions, retains original
`f32` vectors, and requires full-dimensional final scoring. No dimension reduction is
representable.

`LanceDbIndexProfile` is a closed comparison surface:

- `IvfRq`: required compressed candidate-generation reference;
- `IvfPq { num_sub_vectors }`: required second quantized baseline, bounded and required
to divide 4,096;
- `IvfHnswFlat` and `IvfHnswSq`: high-recall controls, not assumed defaults; and
- `BypassExactScan`: sampled ground truth and small filtered-subset exact scan.

The hypothesis that IVF_RQ or IVF_PQ is preferable for frequently filtered workloads
remains a benchmark hypothesis, rather than an asserted production conclusion.

Every `LanceDbSearchRequest` carries one validated `LanceDbIndexProfile`; adapters must
use `request.profile()` for candidate generation rather than a detached schema-validation
choice. Durable adaptive-probe, quality, candidate-loss, and profile values deserialize
through their validated constructors, so malformed persisted contracts fail closed.

## Scalar prefilters and adaptive probes

`LanceDbFilterContract` covers source, collection, tenant, ACL, lifecycle, and
microsecond timestamp predicates. Each clause has a closed mandatory scalar-index
binding: BTree for source/collection/tenant/time, LabelList for ACL, and Bitmap for
lifecycle. Strict filters with any missing or wrong binding return
`strict_filter_unbound`; they may not silently run global ANN and post-filter.

`AdaptiveProbePlan` requires distinct `minimum_nprobes` and `maximum_nprobes`, with a
hard maximum. It deterministically chooses a bounded probe count from filter
selectivity, so narrow filters are not forced into a fixed global probe mode.

## Quality and publication lifecycle

Quantized IVF produces candidates only. `LanceDbQualityPlan` requires a bounded
`refine_factor`, retained original `f32` vectors, and full-precision rescoring.
`CandidateLossReport` separately records omitted ground-truth neighbors because a
refinement pass cannot recover neighbors excluded during candidate generation.

Every schema, policy, hit, request, and lifecycle transition is bound to the exact
Verbatim publication generation. Hydration rejects stale and mismatched profile or
generation hits. The lifecycle contract follows the repository's
`generation_publication` / `IndexPublisher` language:

`staged -> optimized/reindexed -> validated -> promoted`

Rollback is allowed only from validation. Delete, compaction, and crash-recovery hooks
remain generation-bound; they cannot expose a mixed-generation serving state. These
are declarations of required future live behavior, not an implementation of a live
publisher.

## Lexical scope

LanceDB FTS is optional comparison instrumentation only. Tantivy remains the canonical
BM25/lexical engine unless LanceDB passes the complete [#380](https://github.com/RyderFreeman4Logos/verbatim/issues/380)
conformance and qrel gates, including tokenizer, phrase, stop-word, stemming, CJK,
identifier, field-weighting, tenant-IDF, and provenance semantics. This mirrors the
[Qdrant reference backend](qdrant-reference-backend.md) caveat.

## Related architecture

- [Qdrant reference backend](qdrant-reference-backend.md)
- [DiskANN3 architecture contract](diskann3-architecture.md)
- [SSD vector benchmark contract](ssd-vector-benchmark.md)
- [SSD-native vector ADR](ssd-native-vector-adr.md)

Focused verification: `just test-f lancedb_backend`.
