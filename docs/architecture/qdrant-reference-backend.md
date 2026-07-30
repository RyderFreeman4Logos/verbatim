# Qdrant enterprise reference backend contract

Status: VECTOR-REF-001 walking-skeleton contract (Refs [#383](https://github.com/RyderFreeman4Logos/verbatim/issues/383)).

Code: `crates/verbatim-core/src/qdrant_backend/`.

## Decision

Qdrant is a **reference** enterprise vector backend, not Verbatim's primary low-DRAM
SSD algorithm. DiskANN3 remains the primary experimental/production path for the
SSD-native program described by [ADR-SSD-001](ssd-native-vector-adr.md). Qdrant is
retained because its service operation, replication, payload filtering, collection
management, and mature operational controls make it an important fair comparison and
operator-selectable enterprise deployment profile.

This module is a typed adapter **contract walking skeleton**. It deliberately has no
Qdrant dependency and opens no live network connection. The transitional hand-written
REST adapter remains in `crates/verbatim-core/src/index/qdrant.rs`; it uses legacy
`/points/search`, an unnamed cosine vector, and limited profile/source filtering. That
path is not represented as a completed gRPC/Query API replacement.

## Contract guarantees

The typed boundary requires:

- collection, named-vector-space, profile, generation, and configuration-digest
  identity; names and digests are bounded and revalidated after serde decoding;
- a 4,096-dimensional named-vector schema with compatible metric/normalization,
  quantization, HNSW/on-disk-vector flags, and required payload schema;
- payload-index plans for keyword, ACL, lifecycle, and range predicate support;
- native multi-source, collection, tenant, ACL, and lifecycle filters. Unsupported
  strict filters fail with a typed diagnostic: the adapter may not execute global ANN
  and silently post-filter it;
- Qdrant-primary selection for Qdrant profiles. Unconditional local dense pre-search
  is explicitly forbidden and non-representable as a compliant policy. A local fallback
  requires a typed Qdrant failure and a remaining, narrower `SearchBudget`;
- authoritative-store hydration only after the point's profile and exact publication
  generation match; stale or wrong-generation points cannot hydrate;
- bounded deadlines, retries, and backpressure markers; capability discovery for Query
  API, named vectors, multivectors, quantization, on-disk vectors/HNSW, sparse control,
  payload indexes, and gRPC; and
- types-only requirements for the intended official Rust Qdrant client + gRPC Query
  API cutover, without claiming that the cutover is already implemented.

## Sparse BM25 and hybrid controls

Qdrant sparse vectors, server BM25, hybrid prefetch/fusion, quantization,
oversampling, and original-vector rescoring are useful comparison/control surfaces.
They are **not** automatic substitutes for Verbatim lexical semantics. Tantivy remains
the canonical lexical/BM25 backend unless Qdrant passes the complete [#380](https://github.com/RyderFreeman4Logos/verbatim/issues/380)
conformance suite. Future evaluation must explicitly validate multilingual tokenizers,
phrases, stop words, stemming, identifiers/code/path/URL/version/CJK semantics, field
weighting, tenant IDF scope, and retrieval score/rank provenance.

## Non-goals

This issue does not implement:

- live cluster provisioning, collection migration, replication, or operational runbooks;
- a complete official-client/gRPC integration or replacement of the transitional REST
  code path;
- an assertion that Qdrant is the primary low-DRAM SSD ANN algorithm;
- an assertion that Qdrant BM25/hybrid replaces Tantivy; or
- the variable-cardinality late-interaction implementation tracked by [#385](https://github.com/RyderFreeman4Logos/verbatim/issues/385), though capability discovery preserves the native multivector gate.

The contract is intended to make Qdrant measurable as a complete reference backend in
the benchmark work tracked by [#382](https://github.com/RyderFreeman4Logos/verbatim/issues/382), while retaining the lexical/conformance gates of [#380](https://github.com/RyderFreeman4Logos/verbatim/issues/380) and the architecture authority of [ADR-SSD-001](ssd-native-vector-adr.md).
