# Retrieval telemetry contract

Status: first walking-skeleton contract for [#387](https://github.com/RyderFreeman4Logos/verbatim/issues/387).

Code: `crates/verbatim-core/src/retrieval_telemetry/`.

## Decision

Retrieval must make the time and resource use of each bounded pipeline stage observable without placing queries, evidence, paths, identifiers, ACLs, credentials, or high-cardinality labels into default telemetry. This contract provides typed data shapes only. It does not open live spans, read cgroups or page faults, bind DiskANN/LanceDB/Qdrant, emit OpenTelemetry, or change retrieval behavior.

The module complements the cross-service observability contract and the retrieval resource-budget contract:

- `observability_contract` owns generic cross-service trace/log/metric shapes.
- `retrieval_telemetry` owns retrieval-specific stage names, work counters, storage/resource observations, backend knobs, and the strict default privacy boundary.
- `retrieval_budgets` owns enforcing hard resource limits. Telemetry records what an adapter observed; it does not itself admit or reject live work.

## Contract surface

| Type | Purpose |
| --- | --- |
| `StageSpan` / `SpanKind` | Bounded timing record for one named retrieval stage. |
| `CandidateCounters` | Per-stage requested/returned K plus visited, evaluated, filtered, rejected, fused, reranked, and hydrated work. |
| `StorageCounters` | SQL, page/byte, cache, I/O-mode, and page-fault observations. |
| `ResourceCounters` | SSD operations/IOPS/queue depth/wait time and CPU-time observations. |
| `MemorySnapshot` | Path-free cgroup current/peak/event plus anonymous/file/kernel observation. |
| `BackendAttribute` | Closed, bounded backend-knob attribute for a span or controlled run artifact. |
| `PrivacyPolicy` / `RedactedTelemetryId` | Default emission policy and opaque trace/run correlation token. |
| `TelemetryError` / `TelemetryDiagnosticCode` | Closed, payload-free validation and overflow failures. |

`RETRIEVAL_TELEMETRY_CONTRACT_SCHEMA_VERSION` is currently `1`.

## Stage timing

`StageSpan` accepts monotonic microsecond start/end values and rejects end-before-start or a duration over `MAX_STAGE_DURATION_MICROS` (five minutes). Extending a span uses checked arithmetic, so a clock or counter overflow cannot silently wrap.

The closed `SpanKind` vocabulary makes every non-model latency source attributable without allowing caller-selected labels:

1. request setup;
2. selectivity estimation;
3. planner choice;
4. query embedding;
5. dense, lexical, exact, and graph retrieval;
6. filter compilation;
7. fusion/diversity;
8. original-vector read and exact rescoring;
9. reranking and evidence hydration;
10. graph expansion;
11. remote queue/network work; and
12. fallback handling.

Adapters should record query embedding separately from search/storage spans and remote queue/network separately from local backend work. This permits a latency report to answer whether time was spent in embedding, retrieval, filtering, vector I/O, fusion, rescoring, hydration, graph expansion, or queueing.

## Counters and snapshots

All counter additions and counter-ledger merges use checked addition. An overflow returns `counter_overflow`; no counter saturates, wraps, or reports an implicit partial value.

### Candidate work

`CandidateCounters` reserves a fixed slot for every `SpanKind`, so requested and returned K are per-stage but cannot create an unbounded map of dynamic names. Its aggregate dimensions record:

- visited and evaluated candidates;
- filtered and rejected candidates; and
- fused, reranked, and hydrated candidates.

A later adapter can bind planner/profile/generation/plan identities through its existing safe correlation context; it must not turn those IDs into metric labels.

### Storage and resources

`StorageCounters` records SQL statement count, rows and bytes read; graph/vector/filter pages and bytes; cache hits/misses/evictions; direct/buffered/mmap operation counts; and major/minor page faults. `ResourceCounters` records SSD operations, sampled IOPS, queue depth, SSD wait microseconds, and CPU microseconds. The fixed `StorageAccessMode` enum avoids free-form access-mode labels.

`MemorySnapshot` mirrors cgroup v2 `memory.current`, `memory.peak`, and `memory.events` without retaining a cgroup path. It also carries anonymous, file/page-cache, and kernel memory figures. Every byte figure is capped at one EiB, `current <= peak`, and each breakdown category must fit the snapshot current value. The contract intentionally treats page cache as memory: RSS alone is not a sufficient retrieval-memory signal.

## Backend attributes and cardinality

Backend-specific knobs are bounded `BackendAttribute` values, not metric labels. The key vocabulary is closed:

- DiskANN search effort and provider/page layout;
- LanceDB probes, refinement, and index type;
- Qdrant HNSW `ef`, quantization, oversampling, and rescore flag; and
- exact-scan cardinality.

Numeric knobs have a fixed upper bound. Layout, index type, quantization, and rescore values use closed enums or booleans. A key/value mismatch, an unknown serde key, zero where a positive knob is required, or an out-of-range number fails closed. `BackendAttribute::is_metric_label()` is always false: an adapter may attach one to a bounded span or access-controlled run artifact, but never to a metric-label set.

## Privacy policy

`PrivacyPolicy::strict_default()` blocks these data classes at every default telemetry destination:

- raw query text and evidence text;
- filesystem paths and raw IDs;
- ACL values and tokens; and
- unbounded source or tenant labels.

The policy also prohibits backend attributes and correlation IDs from default metric labels, even though their values are bounded/opaque, because labels must remain low cardinality.

`RedactedTelemetryId::new` accepts a bounded source ID, derives a domain-separated SHA-256-based opaque token, and drops the source text. Debug, Display, and serialization expose only the stable `rtid_…` token, never the supplied identifier. The token is valid for span/run correlation; it is not a general identifier or metric label.

`TelemetryError` carries only a closed diagnostic code. Its `Debug` form is `TelemetryError(<code>)` and its display form is `retrieval-telemetry.<code>`; neither can include caller-controlled values.

## Serialization and adapter obligations

Invariant-bearing types revalidate during deserialization:

- `StageSpan` rechecks ordering and duration;
- `MemorySnapshot` rechecks cgroup and breakdown bounds;
- `BackendAttribute` rechecks its closed key/value schema; and
- `RedactedTelemetryId` accepts only the opaque token format.

A future live adapter must:

1. capture stage timing from a monotonic clock and construct one `StageSpan` per applicable closed stage;
2. charge candidate/storage/resource counters along the actual path, including page faults and cache outcomes where available;
3. take cgroup-aware snapshots that include file cache rather than RSS only;
4. attach only validated backend attributes to spans or controlled run artifacts;
5. route every emission decision through `PrivacyPolicy`; and
6. treat telemetry failures separately from retrieval failures unless a future explicit audit-delivery policy says otherwise.

## Out of scope

This slice does **not** add live retrieval instrumentation, an OTLP exporter, metric backend, diagnostic pack storage, cgroup or `/proc` readers, SSD/page-fault probes, benchmark command/report, backend binding, or a telemetry-overhead measurement. Those integrations must preserve the bounded and privacy-safe contract defined here.

## References

- [Issue #387](https://github.com/RyderFreeman4Logos/verbatim/issues/387)
- `docs/architecture/cross-service-observability.md`
- `docs/architecture/retrieval-resource-budgets.md`
- `docs/architecture/search-budget-planner.md`
- `docs/architecture/hybrid-fusion.md`
- `docs/architecture/exact-filtered-scans.md`
