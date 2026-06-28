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

## Latest Local Run

Raw result artifacts are intentionally uncommitted.

Run environment:

- Qdrant was available through an isolated Docker container on `127.0.0.1:6333`.
- Image used: `qdrant/qdrant:latest`, pulled with digest `sha256:75eab8c4ba42096724fdcfde8b4de0b5713d529dde32f285a1f86fdcb2c9e50c`.
- Fixture: 4 deterministic sources, 6 chunks/vectors, `fixture_hash=86c0895847cd967902266dd0bd34a5f9df88e099e309aa039f2c16d0eb6435f8`.

| Variant | Artifact | source/sec | chunks/sec | vectors/sec | CPU core-sec/source | write MiB/source | retrieve p50/p95 ms | run seconds | Qdrant ops | Failure verdict |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `local` | `target/qdrant-spike/local/results.json` | 1.216852 | 1.825278 | 1.825278 | 0.003497 | 0.241211 | 0.069 / 0.188 | 3.287170 | `{}` | `pass` |
| `qdrant-cache` | `target/qdrant-spike/qdrant-cache/results.json` | 0.606758 | 0.910138 | 0.910138 | 0.004734 | 0.127930 | 1.113 / 1.608 | 6.592410 | `availability_check=1, collection_create=1, collection_delete=1, search_requests=9, upsert_requests=1, upsert_points=6` | `pass` |
| `qdrant-primary` | `target/qdrant-spike/qdrant-primary/results.json` | 0.633657 | 0.950486 | 0.950486 | 0.004569 | 0.194336 | 0.964 / 1.773 | 6.312560 | `availability_check=1, collection_create=1, collection_delete=1, search_requests=9, upsert_requests=1, upsert_points=6` | `pass` |

Every numeric table value must trace directly to the matching `results.json` field path listed above. If Qdrant is unavailable, the table must stay explicit about skipped remote timings rather than substituting simulated numbers.

The micro-fixture is deliberately small, so source/chunk/vector throughput is dominated by SQLite fsync, process scheduling, and Qdrant collection setup. Treat the table as a repeatability and correctness smoke result, not as production throughput evidence.

Additional result paths:

- `qdrant.operation_timing_ms.search_requests`: 9.467 ms for cache, 8.530 ms for primary.
- `qdrant.operation_timing_ms.upsert_requests`: 4.392 ms for cache, 5.017 ms for primary.
- `qdrant.local_vector_rows`: 6 for local/cache, 0 for primary.
- `privacy.verdict`: `pass` for all variants.
- `correctness.verdict`: `pass` for all variants.

## Adversarial Self-Review

- Measurement bias: valid finding. The committed fixture is too small and setup/fsync dominates throughput. The report documents this and does not claim production throughput.
- Stale correctness: no unresolved finding. Remote hits hydrate through SQLite and stale/missing hits are rejected in `--failure-modes`.
- Privacy payload: no unresolved finding. The harness rejects forbidden payload fields and enforces a 240-character preview bound.
- Default behavior drift: no unresolved finding. Production Rust ingest/retrieve code is unchanged; qdrant-primary exists only behind `--variant qdrant-primary`.
- False performance wins: valid risk. The recommendation does not treat lower primary local vector rows as enough to promote Qdrant because retrieve latency and total run time did not improve.

## Correctness And Freshness

The harness checks collection freshness before retrieve measurement:

- missing sources;
- stale source/profile generation;
- unembedded source members.

Remote Qdrant hits are never final evidence by themselves. They must hydrate through the local SQLite chunk table and match the current profile generation and fixture collection before entering final evidence. Stale generation hits, missing chunk hits, and collection/capability mismatches are rejected and counted in `correctness`.

`just bench-qdrant-spike --failure-modes` covers:

- Qdrant unavailable: cache mode falls back to local; primary mode fails explicitly.
- Collection reset: remote search after reset must not return stale final evidence.
- Stale remote hits: stale generation and missing chunk hits are rejected before final evidence.

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

Current evidence does not justify promoting Qdrant to the primary vector sink. The primary prototype proved the shape is testable (`qdrant.local_vector_rows=0`) and privacy/correctness gates can pass, but on this small fixture Qdrant adds remote reset/upsert/search work and retrieve latency is higher than local. The fixture is too small to make a production throughput claim, so the right decision is to defer Qdrant and first run the harness against a representative ingest collection. The existing architecture should remain unchanged: local vectors stay authoritative and Qdrant remains optional.

Tradeoffs:

- Throughput: local was faster on this fixture; Qdrant variants were dominated by remote setup and SQLite metadata work.
- CPU: Qdrant variants consumed slightly more local CPU per source in this run.
- Write amplification: qdrant-primary skipped local vector rows, but total measured write MiB did not show a decisive production-scale win.
- Retrieve latency: local p50/p95 was lower than both Qdrant variants.
- Correctness: all variants passed hydration, generation freshness, and failure-mode checks.
- Failure modes: cache fallback and primary fail-closed behavior are covered by `failure-modes.json`.
- Privacy: payloads contain only stable IDs, bounded headings/previews, and profile generation; no raw text or private paths.

Follow-up issues after merge:

- Run this harness against the live representative ingest collection and attach summarized results.
- If primary wins materially, design a production migration for Qdrant as primary vector sink with explicit recovery semantics.
- If cache wins only retrieve latency, keep Qdrant optional and optimize local ingest drain/write bursts first.
