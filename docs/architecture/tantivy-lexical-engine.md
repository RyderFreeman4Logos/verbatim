# Tantivy Lexical Engine

Establishes Tantivy as the dedicated enterprise lexical engine behind
`LexicalSearch`, with a stable BM25 semantics contract that keeps lexical
relevance independently evaluable from the vector backend. Refs #380.

## Status

**Contract only.** This module (`crates/verbatim-core/src/lexical_engine/`)
defines the pure contract types for the Tantivy lexical engine. There is no
live Tantivy index, no actual BM25 computation, no tokenizer binding, and no
query execution in this module. The contract is the stabilization surface that
a future live implementation must satisfy, and that any non-canonical backend
migration must be evaluated against.

## Why Tantivy is canonical

A single vector database should not dictate Verbatim's lexical semantics.
Enterprise RAG needs more than a generic BM25 text field:

- field-aware body/title/heading boosts;
- exact phrase and proximity behavior;
- Chinese and mixed CJK/Latin analysis;
- filenames, paths, URLs, hashes, error codes, Rust/code symbols, versions,
  dates, and raw identifiers;
- Boolean/range/typed metadata compilation from a stable Query AST;
- match explanations and highlighted spans;
- generation, schema, analyzer, and corpus-statistics identity.

Tantivy provides a Rust-native inverted index, BM25 scoring, phrases, fields,
tokenizers, mmap, incremental indexing, and query primitives suitable for this
role. It is the canonical backend for the `LexicalSearch` enterprise profile.

## Contract surface

### Lexical fields and schema

`LexicalFieldSpec` defines a single indexed field: a closed, validated name;
a `LexicalFieldType` (`text`, `cjk_text`, `keyword`, `identifier`, `facet`,
`i64`, `u64`, `bool`, `f64`, `date`); `FieldIndexingFlags` (indexed / stored /
fast); and an optional positive finite BM25 boost (tokenized text fields only).

`LexicalSchema` is the complete, versioned field set bound to an
`AnalyzerIdentity`. Field names are validated to a closed character set
(ASCII alphanumeric + underscore, identifier-like, ≤ 64 chars) so a diagnostic
never echoes arbitrary caller input. The schema is redacted in `Debug`: only
the field count and version are emitted.

The schema covers the full enterprise field contract from #330:

- natural-language body by language/profile;
- CJK and mixed-language fields;
- title, heading, section, caption, notes;
- exact text/keyword;
- filename/path/extension/origin;
- identifiers, code symbols, errors, hashes, URLs, versions, dates;
- tenant/source/collection/ACL/lifecycle/generation fast fields;
- content kind, author, language, jurisdiction, tags, typed metadata.

### Analyzer identity

`AnalyzerIdentity` binds a closed `AnalyzerFamily` (`english`,
`simplified_chinese`, `mixed_cjk_latin`, `code`, `raw`), an `AnalyzerVariant`
(`standard`, `lowercase`, `ngram`, `cjk_segmentation`, `keyword`), and a
nonzero version. The analyzer identity is part of the schema and the
`LexicalGeneration`. A change requires a staged rebuild and atomic cutover —
see `AnalyzerChangeDisclosure`.

### BM25 and field scoring

`Bm25ScoringConfig` pins the exact BM25 semantics:

- `k1` (term-frequency saturation, [0.0, 10.0]);
- `b` (length normalization, [0.0, 1.0]);
- `FieldCombinationStrategy`: `bm25f`, `weighted_sum`, or `weighted_rrf`;
- `LengthNormalizationStrategy`: `uniform` or `per_field`;
- per-field boosts (positive finite, bounded, tokenized-text-only).

**Key contract:** weighted RRF over separate per-field indexes is **not**
mathematically identical to BM25F. If BM25F-like semantics are desired, the
`FieldCombinationStrategy::Bm25F` variant must be declared explicitly so the
field combination happens *inside* the BM25 saturation function, not as a
post-scoring fusion. `WeightedSum` and `WeightedRrf` are post-scoring fusions
and are explicitly flagged as differing from true BM25F via
`is_post_scoring_fusion()`.

### Corpus/IDF scope

`IdfScope` records whether corpus/IDF statistics are:

- `Global` — aggregated over the entire corpus (all tenants);
- `PerTenant` — scoped to a single tenant (strict tenant ranking);
- `PerCollection` — scoped to a single collection within a tenant;
- `Segmented` — per shard/segment, merged consistently before ranking.

In multi-tenant deployments, `validate_tenant_strictness(true)` rejects a
`Global` or `Segmented` scope for a strict tenant-specific ranking contract,
because other tenants' statistics would silently alter ranking. Only
`PerTenant` and `PerCollection` are tenant-isolated.

### Lexical generation

`LexicalGeneration` is the immutable identity of a built lexical index: a
`LexicalGenerationId`, the `LexicalSchema`, the `AnalyzerIdentity` (taken from
the schema), the `Bm25ScoringConfig`, and a `CorpusStatsSnapshot`
(document count + `sha256:` statistics hash + IDF scope). Construction
validates that the scoring IDF scope matches the corpus-stats scope.

The generation is part of the `RetrievalProfile`. Any change to schema,
analyzer, scoring, or corpus requires a new generation with staged rebuild and
atomic cutover.

### Retriever type classification

`LexicalRetrieverType` classifies the distinct retrieval paths:

- `Bm25TopK` — approximate relevance ranking (never justifies completeness);
- `ExactPhrase` — exact phrase/proximity match;
- `Identifier` — exact identifier match;
- `Reference` — exact reference/citation match;
- `Metadata` — structured metadata/filter match;
- `ExhaustiveEnumeration` — exhaustive enumeration over a declared authorized
  scope (the **only** retriever that may justify `all`/`only`/`none`/`every`
  claims).

`CompletenessClaim` (`top_k`, `approximate`, `all`, `only`, `none`, `every`)
is validated against the retriever type: BM25 Top-K and other
non-exhaustive retrievers may not claim completeness. This enforces the issue's
requirement that BM25 Top-K cannot justify `all`, `only`, `none`, or `every`
claims over a scope.

### Conformance/qrel gate

`LexicalConformanceGate` is the pass/fail contract that a backend BM25 change
must satisfy before publication or migration cutover. It binds a
`ConformanceSuiteId` (closed name + version), a set of `ConformanceThreshold`s
(`ndcg_at_k`, `recall_at_k`, `precision_at_k`, `mrr`, `map`), and a qrel case
count. `evaluate()` checks that every declared threshold has a matching
`ConformanceObservations` value that meets or exceeds the minimum.

Backend BM25 changes — a Tantivy upgrade, tokenizer swap, field-scoring change,
or migration to Qdrant/LanceDB FTS — must pass the same conformance/qrel suite.

## Why Qdrant/LanceDB BM25 are not canonical

Qdrant supports server-side BM25 sparse vectors and hybrid fusion, and LanceDB
supports BM25 FTS. They remain useful comparison or co-located deployment
options, but migration is **not transparent**:

- tokenizers, stemming, stop words, positions, field scoring, and IDF scope can
  differ;
- phrase behavior requires the appropriate position/stop-word configuration;
- Qdrant does not make Verbatim's typed query language or exact identifier
  routing automatic;
- server-side fusion may hide per-retriever rank/score details required for
  audit;
- backend BM25 changes must pass the same lexical conformance/qrel suite.

`SemanticDifferenceDisclosure` requires that at least one semantic difference
(tokenizer, stemming, stop words, positions, field scoring, IDF scope) be
explicitly declared for any non-canonical backend evaluation.
`NonCanonicalMigrationContract` binds a candidate `BackendClass`
(`qdrant_sparse`, `lancedb_fts`, `sqlite_fts5`), the disclosure, and the
conformance gate. It explicitly records `not_transparent: true`. A canonical
(Tantivy) candidate is rejected — the canonical backend does not migrate to
itself.

## Fail-closed guarantees

- All validation rejects invalid input (empty fields, out-of-range parameters,
  duplicate names, mismatched scopes, missing components).
- Errors are diagnostic-code-only: no variant retains a caller-controlled field
  name, tenant, ACL principal, source id, collection id, analyzer identity,
  tokenizer name, qrel label, or content hash.
- `Debug` and `Display` on `LexicalEngineError` emit only the closed code.
- Types carrying sensitive data (`FieldName`, `LexicalFieldSpec`, `LexicalSchema`,
  `Bm25ScoringConfig`, `CorpusStatsHash`, `LexicalGeneration`) override `Debug`
  to render only closed/redacted summaries.
- No `unwrap`/`expect`/`panic` in production code.

## Mixed-generation read rejection

`reject_mixed_generation_read(active, requested)` blocks a single query path
from reading two lexical generations simultaneously, mirroring the
generation-publication contract's mixed-read rejection for the lexical engine.

## Acceptance criteria mapping

This contract module addresses the issue's acceptance criteria as follows
(full live-index satisfaction is out of scope for this contract-only step):

- Tantivy as production `LexicalSearch` — canonical backend declared
  (`CANONICAL_BACKEND = BackendClass::Tantivy`).
- Schema/analyzer/IDF-scope identity persisted and generation-bound —
  `LexicalGeneration`, `AnalyzerIdentity`, `IdfScope`.
- BM25 field scoring transparency — `Bm25ScoringConfig`,
  `FieldCombinationStrategy` (RRF ≠ BM25F).
- Exact and exhaustive paths separate — `LexicalRetrieverType`,
  `CompletenessClaim`.
- Backend changes pass conformance — `LexicalConformanceGate`.
- Qdrant/LanceDB migration non-transparent —
  `NonCanonicalMigrationContract`, `SemanticDifferenceDisclosure`.

The English, Simplified Chinese, mixed CJK/Latin, phrase, path, code,
identifier, version, URL, and hash fixtures (live-index acceptance) will be
satisfied by the follow-on live Tantivy implementation that binds to this
contract.

## References

- #330 — primary lexical profile specification.
- #296 — structured query constraints.
- #305, #381 — multi-retriever fusion.
- [Tantivy](https://github.com/quickwit-oss/tantivy)
- [Qdrant text search](https://qdrant.tech/documentation/search/text-search/)
- [LanceDB FTS](https://docs.lancedb.com/indexing/fts-index)
