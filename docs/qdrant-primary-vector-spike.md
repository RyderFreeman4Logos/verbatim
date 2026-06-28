# Qdrant Primary Vector Sink Spike

Issue: #162

This spike compares three explicit benchmark variants:

- `local`: SQLite/local vector rows remain the primary vector sink.
- `qdrant-cache`: local vector rows remain primary, then bounded Qdrant payloads are synced for remote search.
- `qdrant-primary`: experimental harness-only prototype where metadata/chunk text stay in SQLite and vectors are written only to Qdrant.

Production ingest and retrieval defaults are unchanged. A production storage migration, if any, is a separate follow-up issue.

## Harness

Run commands:

```sh
just bench-qdrant-spike --variant local --dry-run
just bench-qdrant-spike --variant qdrant-primary --dry-run
just bench-qdrant-spike --failure-modes
just bench-qdrant-spike --variant local
just bench-qdrant-spike --variant qdrant-cache
just bench-qdrant-spike --variant qdrant-primary
```

The harness writes only under `target/qdrant-spike/<variant>/` by default. It emits a machine-readable `RUN_MANIFEST_JSON=...` line on dry-run and writes `results.json` for measured runs.

Required result fields are under these JSON paths:

- `metrics.source_per_sec`
- `metrics.chunks_per_sec`
- `metrics.vectors_per_sec`
- `metrics.cpu_core_sec_per_source`
- `metrics.physical_write_mib_per_source`
- `metrics.retrieve_latency.p50_ms`
- `metrics.retrieve_latency.p95_ms`

`metrics.cpu_core_sec_per_source` and `metrics.physical_write_mib_per_source` are local harness/client measurements only. Qdrant service CPU and physical writes are reported separately as `metrics.qdrant_service_cpu_core_sec_per_source`, `metrics.qdrant_service_physical_write_mib_per_source`, and `metrics.external_service_unmeasured`. When the Qdrant service fields are null, the local-client numbers must not be treated as total system CPU or total system write amplification.

## Latest Local Run

Raw result artifacts are intentionally uncommitted.

Run environment:

- Qdrant was available through an isolated Docker container on `127.0.0.1:6333`.
- Image used: `qdrant/qdrant:latest`, pulled with digest `sha256:75eab8c4ba42096724fdcfde8b4de0b5713d529dde32f285a1f86fdcb2c9e50c`.
- Fixture: 4 deterministic sources, 6 chunks/vectors, `fixture_hash=86c0895847cd967902266dd0bd34a5f9df88e099e309aa039f2c16d0eb6435f8`.

| Variant | Artifact | source/sec | chunks/sec | vectors/sec | local client CPU core-sec/source | local client/profile write MiB/source | retrieve p50/p95 ms | run seconds | Qdrant ops | Failure verdict |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `local` | `target/qdrant-spike/local/results.json` | 44.650088 | 66.975132 | 66.975132 | 0.001540 | 0.237305 | 0.034 / 0.133 | 0.089585 | `{}` | `pass` |
| `qdrant-cache` | `target/qdrant-spike/qdrant-cache/results.json` | 6.257754 | 9.386631 | 9.386631 | 0.003795 | 0.233398 | 1.648 / 3.224 | 0.639207 | `availability_check=1, collection_create=1, collection_delete=1, search_requests=9, upsert_requests=1, upsert_points=6` | `pass` |
| `qdrant-primary` | `target/qdrant-spike/qdrant-primary/results.json` | 5.472629 | 8.208943 | 8.208943 | 0.005881 | 0.229492 | 4.709 / 5.952 | 0.730910 | `availability_check=1, collection_create=1, collection_delete=1, search_requests=9, upsert_requests=1, upsert_points=6` | `pass` |

Every numeric table value must trace directly to the matching `results.json` field path listed above. If Qdrant is unavailable, the table must stay explicit about skipped remote timings rather than substituting simulated numbers. Qdrant service CPU/write totals are not inferred from local client process counters.

The micro-fixture is deliberately small, so source/chunk/vector throughput is dominated by SQLite fsync, process scheduling, and Qdrant collection setup. Treat the table as a repeatability and correctness smoke result, not as production throughput evidence.

Additional result paths:

- `qdrant.operation_timing_ms.search_requests`: 14.534 ms for cache, 33.684 ms for primary.
- `qdrant.operation_timing_ms.upsert_requests`: 18.229 ms for cache, 17.602 ms for primary.
- `qdrant.local_vector_rows`: 6 for local/cache, 0 for primary.
- `metrics.external_service_unmeasured`: `false` for local, `true` for cache/primary.
- `correctness.hydrated_remote_hits`: 45 for cache and primary.
- `privacy.verdict`: `pass` for all variants.
- `correctness.verdict`: `pass` for all variants.

## Adversarial Self-Review

- Measurement bias: valid finding. The committed fixture is too small and setup/fsync dominates throughput. The report documents this and does not claim production throughput.
- Stale correctness: no unresolved finding. Remote hits hydrate through SQLite and stale/missing/capability-mismatch hits are rejected in `--failure-modes`.
- Privacy payload: no unresolved finding. The harness rejects forbidden payload fields and enforces a 240-character preview bound.
- Default behavior drift: no unresolved finding. Production Rust ingest/retrieve code is unchanged; qdrant-primary exists only behind `--variant qdrant-primary`.
- False performance wins: valid risk. The recommendation does not treat lower primary local vector rows as enough to promote Qdrant because retrieve latency and total run time did not improve.

## Correctness And Freshness

The harness checks collection freshness before retrieve measurement:

- missing sources;
- stale source/profile generation;
- unembedded source members.

Remote Qdrant hits are never final evidence by themselves. They must hydrate through the local SQLite chunk table and match the current profile generation, fixture collection, and expected capability before entering final evidence. Stale generation hits, missing chunk hits, and collection/capability mismatches are rejected and counted in `correctness`.

`just bench-qdrant-spike --failure-modes` reports full `pass` only when every required case is actually covered. If Qdrant is unavailable, the unavailable failover case can pass, but the collection-reset case is reported as `not_covered` and the overall failure-mode verdict is not `pass`.

The latest local Docker run covered:

- Qdrant unavailable: cache mode falls back to local; primary mode fails explicitly.
- Collection reset: remote search after reset must not return stale final evidence.
- Stale remote hits: stale generation, missing chunk, and capability-mismatch hits are rejected before final evidence.

## Privacy

Qdrant payload inspection runs for cache and primary records. Allowed payload fields are:

- `profile_id`
- `profile_generation`
- `chunk_id`
- `source_id`
- `heading_path`
- `text_preview`

`text_preview` is bounded at 240 characters. The payload must not contain raw chunk text, full document text, private source paths, or absolute paths.

## Recommendation

Recommendation: **defer Qdrant**.

Current evidence does not justify promoting Qdrant to the primary vector sink. The primary prototype proved the shape is testable (`qdrant.local_vector_rows=0`) and privacy/correctness gates can pass, but on this small fixture Qdrant adds remote reset/upsert/search work and retrieve latency is higher than local. The fixture is too small to make a production throughput claim, and Qdrant service CPU/write resources were not included in the local-client counters, so the right decision is to defer Qdrant and first run the harness against a representative ingest collection with explicit service-side measurement. The existing architecture should remain unchanged: local vectors stay authoritative and Qdrant remains optional.

Tradeoffs:

- Throughput: local was faster on this fixture; Qdrant variants were dominated by remote setup and SQLite metadata work.
- CPU: Qdrant variants consumed slightly more local harness/client CPU per source in this run; Qdrant service CPU was unmeasured.
- Write amplification: qdrant-primary skipped local vector rows, but the reported write metric covers only local harness/profile writes; Qdrant service physical writes were unmeasured.
- Retrieve latency: local p50/p95 was lower than both Qdrant variants.
- Correctness: all variants passed hydration, generation freshness, and failure-mode checks.
- Failure modes: the local Docker run covered cache fallback, primary fail-closed behavior, collection reset, and stale-hit rejection in `failure-modes.json`; unavailable-only runs report reset as `not_covered`.
- Privacy: payloads contain only stable IDs, bounded headings/previews, and profile generation; no raw text or private paths.

Follow-up issues after merge:

- Run this harness against the live representative ingest collection and attach summarized results.
- If primary wins materially, design a production migration for Qdrant as primary vector sink with explicit recovery semantics.
- If cache wins only retrieve latency, keep Qdrant optional and optimize local ingest drain/write bursts first.
