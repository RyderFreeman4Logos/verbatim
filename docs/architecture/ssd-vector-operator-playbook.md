# SSD-native vector operator playbook

This playbook operationalizes [ADR-SSD-001](ssd-native-vector-adr.md). It is for
the future DiskANN3 provider and service adapters that implement the current
contract slices. The repository does **not** yet ship a live DiskANN3
`DataProvider`, benchmark CLI, cgroup reader, or remote vector service; do not
represent the examples below as deployed runtime controls until the adapter
implements and validates them.

The checked-in examples are versioned operating contracts:

- [`docs/config/diskann3_enterprise.toml`](../config/diskann3_enterprise.toml)
  -- enterprise profile and bounded defaults;
- [`docs/config/benchmark_matrix.toml`](../config/benchmark_matrix.toml) --
  comparison dimensions and evidence requirements; and
- [`docs/config/acceptance_gates.toml`](../config/acceptance_gates.toml) -- hard
  quality, conformance, resource, publication, and recovery gates.

They contain no credentials. A live deployment must source credentials through
its secret-management boundary, never by copying them into one of these files,
command lines, benchmark artifacts, telemetry, or logs.

## 1. Non-negotiable operating contract

A serving request is bound to one compatible tuple:

```text
(vector-space ID, embedding-profile ID, layout/schema version,
 policy generation, publication generation)
```

Do not route a request across a mixed tuple. A stale cache, mismatched shard, or
unavailable filter structure is a closed failure, not an invitation to merge
results or widen a request.

The following rules apply in every profile:

1. Retain the original 4,096-dimensional `float32` vector on SSD.
2. Candidate product/scalar/spherical quantization is candidate generation only;
   it is **not** embedding-dimension reduction.
3. Fetch only a bounded final candidate set and exactly rescore it from original
   vectors before final dense ranking.
4. Keep Tantivy as the primary lexical/BM25 engine. Dense-backend hybrid support
   does not replace lexical conformance.
5. Push strict tenant, ACL, source, lifecycle, date, and typed-metadata
   predicates into candidate generation. Revalidate returned candidates against
   authoritative ACL, lifecycle, and tombstone state during hydration.
6. Build derived artifacts in a non-active generation, validate them, and publish
   one atomic generation pointer. Retain the prior generation until query leases
   expire.

See [DiskANN3 architecture](diskann3-architecture.md),
[enterprise predicates](enterprise-vector-predicates.md),
[exact filtered scans](exact-filtered-scans.md), and
[atomic publication manifests](index-publication-manifests.md) for the contract
boundaries behind these rules.

## 2. Capacity planning and shard sizing

### 2.1 Use linear disk formulas

Use the measured manifest size for final capacity planning, but begin with a
conservative linear envelope. Let:

- `N` = vectors in the shard;
- `D` = original dimension (`4096`);
- `F` = bytes per original component (`4` for `float32`);
- `Q` = candidate-code bytes per vector;
- `R` = graph degree;
- `I` = neighbor-ID bytes;
- `M` = metadata/filter budget per vector; and
- `H` = all measured per-vector graph/page/header/alignment overhead.

The minimum original-vector floor is:

```text
original_vector_bytes = N * D * F = N * 4096 * 4 = 16,384 * N
```

A planning envelope is:

```text
candidate_code_bytes  = N * Q
graph_page_bytes      ~= N * (R * I + H) + page_padding
attribute_bytes       <= N * M + immutable-filter-index-overhead
id_map_tombstone_bytes = O(N)
manifest_bytes        = O(number_of_files)

shard_physical_bytes ~= original_vector_bytes
                      + candidate_code_bytes
                      + graph_page_bytes
                      + attribute_bytes
                      + id_map_tombstone_bytes
                      + manifest_bytes
```

The `~=` symbol is intentional: graph packing, page headers, alignment,
checksums, and attributes must be measured from the built manifest rather than
assumed away. Capacity is linear only when `D`, `Q`, `R`, `I`, `M`, and the
layout policy are bounded constants.

Choose the shard's vector count with both declared ceilings:

```text
N <= max_vectors_per_shard
N <= floor(max_physical_bytes_per_shard / measured_bytes_per_vector_envelope)
```

Reserve filesystem and rebuild headroom separately. A safe planning allocation
for a shard family is at least active generation + previous leased generation +
staging generation + configured recovery headroom. Do not set capacity from
active bytes alone, and do not compact an old generation before its leases
expire.

Never use one index per source or per tenant as the general sharding scheme.
Shard keys are vector-space/profile, layout version, publication generation, and
ordinal/hash range. Source, tenant, ACL, lifecycle, language, date, and metadata
are filter attributes. This keeps disk growth linear and prevents unbounded
open-file state and query fan-out. See [immutable SSD-native vector
shards](immutable-vector-shards.md).

### 2.2 Preflight the storage device

Use local NVMe storage with declared logical/physical page characteristics and
measured queue behavior. Record the device model/firmware, filesystem, mount
options, kernel, capacity, page size, queue depths, and power/failure policy in
the benchmark hardware profile. Do not compare a local NVMe run to a networked
block device or a different filesystem and call it an equivalent benchmark.

The page layout must honor the configured 4 KiB page and alignment. For direct
I/O, user buffers and offsets must meet the device/filesystem alignment
requirements. For buffered I/O or `mmap`, include kernel page-cache behavior in
the memory budget and benchmark report. A restart is not proof of a cold cache;
define the cold-cache protocol, execute it with the required privileges, and
record exactly how it was achieved.

Use the [AISAQ page-layout contract](aisaq-page-layout.md) for page checksums,
co-location rules, and read-amplification bounds. Original full-precision
vectors remain distinct from candidate codes even when candidate codes are
co-located with graph pages.

## 3. Memory, page-cache, and cgroup limits

### 3.1 Account for total cgroup memory

RSS is insufficient. Under cgroup v2, `memory.current` covers charged anonymous
memory **and file cache**; `memory.high` is the throttle/reclaim threshold, and
`memory.max` is the hard ceiling. The online example uses:

```text
memory.high = 192 MiB (201,326,592 bytes)
memory.max  = 256 MiB (268,435,456 bytes)
```

The isolated build/compaction example uses 384 MiB / 512 MiB. Always keep
`memory.high < memory.max`; headroom is required for reclaim and orderly typed
failure. Read `memory.current`, `memory.high`, `memory.max`, and relevant
`memory.events` from the serving cgroup, not from an unrelated parent cgroup.

Do not size `memory.max` from process RSS. Buffered reads and `mmap` can charge
page cache to the cgroup. Direct I/O avoids ordinary page-cache residency but
still consumes bounded user buffers and does not remove the need to record
actual memory behavior.

### 3.2 Budget the complete online working set

Before enabling a profile, reserve capacity for:

```text
baseline process + bounded caches + active-query working sets + kernel file cache
```

Each request independently enforces caps for read buffers, predicate bitmaps,
graph frontier/visited state, exact-rescore originals, Arrow/serialization
batches, fusion candidates, and hydration text. The example caps sum to a
bounded per-query envelope; concurrency admission must use that envelope and
actual `memory.current`, not assume that all fields can grow freely.

Every corpus-proportional cache--page, mapping, graph, entry-point,
decompression, hydration--requires a fixed byte/entry maximum. A cache miss is
acceptable; an unbounded cache is not. When `memory.high` is exceeded, stop
admitting work as required by the adapter's policy, surface the typed condition,
and protect the `memory.max` headroom. When `memory.max` would be exceeded,
fail the bounded request/work rather than expanding a cache or weakening a gate.

Build, merge, and compaction work run in a separate cgroup with dedicated
workers and lower I/O priority. They must not share an unbounded allocator,
thread-per-shard model, or page-cache allowance with online queries. See
[retrieval resource budgets](retrieval-resource-budgets.md).

## 4. Request budgets, cold/warm behavior, and NVMe behavior

### 4.1 Enforce every limit before issuing more work

The versioned enterprise example has these conservative per-request values:

| Budget | Example hard cap | Operator response at exhaustion |
| --- | ---: | --- |
| Candidate pool | 2,000 | Return the typed bounded/partial result permitted by the contract; do not keep traversing. |
| SSD pages | 4,096 | Stop page traversal and record `page_budget_exceeded`. |
| SSD bytes | 16 MiB | Stop reads and record `byte_budget_exceeded`. |
| IOPS | 1,024 | Stop new reads and record `iops_exceeded`. |
| Queue depth | 8 | Apply backpressure; do not increase outstanding work. |
| Cumulative await | 100 ms | End the request under its typed budget/deadline policy. |
| Read amplification | 5.000 pages/rescored vector | Treat as a layout/graph/compaction investigation, not a reason to raise the cap. |
| Wall deadline | 75 ms | Cancel remaining stages and surface `deadline_exceeded`. |
| Exact rescore set | 200 | Never fetch arbitrary additional originals. |
| Active queries / workers | 32 / 16 | Reject or queue admission; report `concurrency_saturated`. |

Caps are a contract, not tuning hints. The provider may choose less work but may
not widen them. Keep one shared worker maximum and per-category limits for
retrieval, storage, model, and background work; no category may starve the
others.

### 4.2 Measure cold and warm separately

A **cold** run starts from a documented cache state with no undeclared prewarm.
A **warm** run starts only after a declared warm-up workload and records that
workload. Run both states for each required scenario because the primary design
must survive cold random I/O as well as warm page-cache behavior.

For each state, record p50/p95/p99 latency, deadline exhaustion, pages, bytes,
IOPS, queue depth, await time, read amplification, cgroup memory current/high/
max events, and exact-rescore coverage. Do not average cold and warm results
into one latency claim. A warm win cannot mask a cold failure, and a direct-I/O
profile cannot be compared to a buffered/mmap profile without recording the
access mode and cache protocol.

## 5. Filter, ACL, and hydration guarantees

The planner classifies the *authorized* candidate count, not the raw corpus size:

| Class | Band relative to calibrated thresholds | Required path |
| --- | --- | --- |
| Zero | `0` | Return without vector page I/O |
| Small | `1..=exact_simd_scan_max_matches` | Exact full-dimensional scan |
| Medium | `exact_simd_scan_max_matches+1 .. predicate_aware_diskann3_min_matches-1` | `PlannerSelected` |
| Broad | `>= predicate_aware_diskann3_min_matches` | Predicate-aware DiskANN3 |

When the configured thresholds leave a Medium gap (for example 10,001–49,999
with the enterprise example profile), the predicate contract reports
`PlannerSelected`. The search planner currently selects exact full-dimensional
work for that gap only when the independent exact candidate budget still fits.
If the Medium count exceeds that exact budget, planning fails closed. Do not
widen the exact budget silently, invent an uncalibrated ANN path, or issue
global ANN then post-filter a Top-K as a correctness workaround. Stage
telemetry and fusion provenance must record the selectivity class and the
planner choice.

Supported strict predicate categories are source, collection, tenant,
ACL-principal, ACL-deny, lifecycle, date range, and typed metadata equality.
Tenant/ACL constraints are mandatory. Reject malformed, oversized, non-finite,
or unsupported strict predicates with a diagnostic-only typed failure. Do not
turn an unsupported strict predicate into a best-effort result, a broader query,
or an empty success response.

Candidate generation is not the final authorization check. During authoritative
hydration, revalidate ACL, lifecycle, and tombstone state against the request's
bound policy/publication generation. Drop a candidate that is no longer valid.
Telemetry and diagnostics must redact candidate identifiers, predicate values,
tenant identifiers, and payload previews. See [enterprise vector
predicates](enterprise-vector-predicates.md) and [exact filtered
scans](exact-filtered-scans.md).

## 6. Updates, compaction, publication, rollback, backup, and recovery

### 6.1 Update and compaction procedure

1. Commit catalog/evidence/original-vector mutations through the authoritative
   durability path with idempotent, version-ordered identities.
2. Build a **staging** immutable shard generation in the isolated build cgroup.
   Stream bounded batches; do not mutate an active shard in place.
3. Apply deletes/tombstones in a generation- and version-aware form. A tombstone
   excludes matching candidates before hydration and cannot suppress a newer
   reinserted version.
4. Trigger compaction from measured dead-byte ratio, read amplification, mutation
   volume, or p99 latency--not wall-clock time alone. The example thresholds are
   20% dead bytes, 4.0 pages/candidate amplification, 50,000 mutations, or
   50,000 microseconds p99.
5. Fsync staged data **and directory metadata**, write checksummed manifests, and
   validate vector-space/profile, layout, generation, file-role, size, hash,
   predicate, and quality invariants.
6. Only a fully validated `Ready` generation may be promoted. A partially
   fsynced or inconsistent artifact is quarantined and rebuilt from authoritative
   originals.

The live adapter must make each stage durable/observable. If it cannot prove the
stage is durable, recovery chooses the previous committed generation. Refer to
[durable updates, tombstones, compaction, and crash recovery](durable-updates.md).

### 6.2 Publish and roll back atomically

Promotion is an ordered state transition:

```text
stage -> validate -> compare-and-swap active generation pointer -> serve bound queries
```

Validate every component digest and capability before pointer promotion. The
pointer update must compare both expected active generation and epoch. A stale
CAS is a conflict, not a successful publication. Bind existing queries/cursors
to their original generation; do not rebind them mid-stream. Retain old files
until all generation leases expire, then garbage collect in bounded batches.

For rollback, CAS the pointer to the prior validated generation and invalidate
incompatible caches/cursors. Do not "roll back" by mixing old Tantivy, vector,
ACL, or manifest components with new ones. Capture the rollback result,
generation IDs, manifest hashes, and reason as a recovery artifact without
recording user data or secrets.

### 6.3 Backup and disaster recovery

Back up the authoritative catalog/evidence/blob/task stores, original vectors,
publication manifests/pointers, checksums, schema/layout metadata, and the
versioned configuration/evidence record. Treat SSD vector shards as derived
artifacts: they may be backed up for recovery speed, but restore must still
verify manifests and must be able to rebuild from authoritative originals.

A recovery drill must demonstrate:

1. restore or validate authoritative data without accepting a partial artifact;
2. restore a complete checksummed generation or rebuild it deterministically;
3. validate exact-vector availability, ACL/filter structures, and compatible
   vector-space/profile/generation identity;
4. publish only through the atomic pointer transition;
5. demonstrate a rollback to the previous generation; and
6. retain a report with hardware/config/data digests, timings, and pass/fail
   evidence.

A crash after an unfsynced publish claim is an inconsistency: quarantine it and
rebuild rather than serving it. See [generation publication](generation-publication.md)
and [index publication manifests](index-publication-manifests.md).

## 7. Benchmark commands and report interpretation

### 7.1 Current repository commands

The following commands exercise real, currently available contract or Qdrant
spike paths. They are not a substitute for a future DiskANN3 provider benchmark
runner:

```sh
# Parse the checked-in TOML examples with the standard-library TOML parser.
python3 -c 'import tomllib; from pathlib import Path; [tomllib.loads(p.read_text()) for p in Path("docs/config").glob("*.toml")]'

# Exercise focused contract coverage relevant to this decision.
just test-f diskann3
just test-f retrieval_budgets
just test-f enterprise_predicates

# Existing Qdrant spike discovery/control commands.
just bench-qdrant-spike --variant local --dry-run
just bench-qdrant-spike --variant qdrant-primary --dry-run
just bench-qdrant-spike --failure-modes
```

The `just bench-qdrant-spike` recipes are Qdrant spike controls; they do not
claim that Qdrant is the selected primary nor test a missing DiskANN3 binding.
The current telemetry contract also does not export live SSD/page-fault or
benchmark reports. Until an adapter ships a real runner, record the benchmark
as **not run**, not passed. See [Qdrant primary vector spike](../qdrant-primary-vector-spike.md)
and [retrieval telemetry](retrieval-telemetry.md).

A future provider runner must accept a versioned config, retain its exact argv in
the report, and write the matrix-required artifacts. Do not invent a CLI name in
an operational runbook; use the runner actually delivered by the adapter and
capture its `--help`, binary revision, and output schema alongside the report.

### 7.2 Required benchmark report

A report can support a performance claim only when it contains:

- git revision, provider/library version, exact argv, and config digest;
- dataset and qrel revisions/digests plus vector-space/profile/layout identity;
- host CPU, RAM, cgroup values, kernel, filesystem/mount, NVMe model/firmware,
  access mode, page size, and queue-depth profile;
- declared cold/warm protocol and prewarm workload;
- exact ground-truth comparison with quality, filtered quality, and
  exact-rescore coverage;
- predicate/ACL conformance and unsupported-strict-predicate outcomes;
- stage telemetry for planning, candidate generation, SSD reads, rescore, fusion,
  hydration, and publication/recovery where applicable; and
- raw resource counts and explicit gate verdicts, including missing/failed/skipped
  scenarios.

The report interpretation is deliberately strict:

| Report result | Meaning |
| --- | --- |
| **Pass** | Every required scenario, gate, and artifact passed on the declared compatible identity and hardware profile. |
| **Fail** | A quality, ACL/filter, original-vector/rescore, memory, I/O, deadline, publication, or recovery gate failed. Do not promote. |
| **Not run / skipped** | Not evidence of a pass. Include a reason and keep the comparison incomplete. |
| **Incomparable** | Dataset, qrels, identity, cache protocol, hardware, access mode, or configuration changed. Re-run under compatible conditions. |

No single QPS, p99, or warm-cache result can change the ADR. Reconsider the
primary only when another backend wins the **complete hard-gated benchmark and
conformance suite** defined by the matrix and acceptance-gates files.

## 8. In-process and remote service profiles

### In-process profile

Use only for a trusted local deployment where the adapter shares the daemon
process. It must still enforce the same typed `VectorSearch` semantics,
identity/generation binding, per-request budgets, fixed caches, exact rescoring,
filter/ACL rules, and manifest validation. An in-process address space does not
permit bypassing cgroup accounting or using unbounded resident HNSW state.

### Remote service profile

Use for shared-nothing serving when the adapter provides a real service boundary.
The remote service must expose the same request/response semantics and preserve
caller budgets, filter capability reporting, generation binding, typed
exhaustion, redaction, exact-rescore requirements, and publication protocol. It
must not expose a backend-specific raw predicate JSON as the stable public
contract. Transport authentication, authorization, timeouts, and failure policy
belong to the service implementation; none are implemented by the current
walking-skeleton contract.

Both profiles use the same vector-space/profile/generation tuple and the same
operator evidence requirements. A profile difference cannot be used to relax
ACL, quality, memory, or recovery gates.

## 9. Troubleshooting by stage telemetry

Telemetry is a bounded, privacy-safe contract. A future exporter must retain the
stage, counts, timings, resource charges, exhaustion code, backend role, cache
state, and compatible identity while redacting identifiers and predicate values.
Use the following triage order:

| Stage / symptom | Check | Safe response |
| --- | --- | --- |
| **Admission**: saturation or rising queue | Active queries, shared workers, per-category limits, `memory.current` | Apply backpressure; do not add workers or caches beyond the profile. |
| **Planning**: unexpected ANN for a narrow or medium ACL scope | Authorized-cardinality estimate, generation/policy binding, exact/medium thresholds, exact budget | Small scopes must use exact scan; Medium is `PlannerSelected` and currently exact only while the independent exact budget fits, otherwise fail closed. |
| **Predicate**: unsupported or malformed strict filter | Capability declaration and typed diagnostic | Fail closed; implement/validate support before enabling the request. |
| **Candidate traversal**: low filtered recall | Filter pushdown, candidate budget, graph/quantizer configuration, exact reference report | Compare against exact ground truth; do not hide the loss by post-filtering a global Top-K. |
| **SSD read**: high pages/bytes/await/amplification | Access mode, page layout, queue depth, cache state, compaction metrics | Stop at bounds; investigate locality, graph/layout, and compaction rather than widening limits. |
| **Exact rescore**: missing originals or low coverage | Original-vector manifest role, candidate issuance, rescore limit, generation | Quarantine the generation or fail the request; never rank final results only from compressed codes. |
| **Fusion**: unexplained rank/completeness claim | Retriever provenance, raw ranks/scores, bounded fusion output | Preserve provenance; do not label approximate results exhaustive. |
| **Hydration**: ACL/lifecycle/tombstone rejection | Authoritative revalidation outcome and policy generation | Drop rejected candidates; investigate publication/update lag without leaking IDs. |
| **Publication/recovery**: mixed or missing shards | Manifest digests, pointer epoch, fsync attestation, leases | Stop serving the inconsistent generation; restore previous committed generation or rebuild. |

For the telemetry model and redaction rules, see [retrieval telemetry](retrieval-telemetry.md) and [hybrid fusion](hybrid-fusion.md).

## 10. Migration from SQLite scan, HNSW, and Qdrant cache

Migration is a gated cutover, not a silent compatibility bridge.

1. **Inventory and freeze identity.** Record the existing embedding profile,
   dimension, metric, source/evidence mapping, ACL/lifecycle semantics, and
   publication generation. Do not migrate vectors of an incompatible profile
   into a shared shard.
2. **Keep the source of truth authoritative.** SQLite scan/HNSW/Qdrant cache
   contents are derived. Verify that full originals, mappings, and metadata can
   be reconstructed from catalog/evidence/blob data before changing defaults.
3. **Build a staging DiskANN3 generation.** Stream bounded batches, create
   immutable manifests, preserve 4,096d originals, build filters, and validate
   checksums and exact-rescore availability.
4. **Compare before publish.** Run the matrix against exact SQLite scans for
   appropriate scopes and Qdrant/LanceDB references where available. Include
   cold/warm, filters/ACLs, updates, compaction, rollback, and recovery.
5. **Publish atomically after gates pass.** CAS the compatible generation pointer;
   bind queries to it; retain the prior generation and legacy path behind explicit
   opt-in only.
6. **Roll back, do not patch in place.** If any hard gate fails, restore the
   previous pointer/generation, capture the report, and rebuild/fix the staging
   generation. Do not backfill an active generation or silently fall back to
   global ANN post-filtering.
7. **Retire only with evidence.** Remove SQLite scan, resident instant-distance
   HNSW, or Qdrant-cache defaults only after the documented removal criteria,
   rollback window, and recovery evidence are satisfied. Exact SQLite scan may
   remain as a deliberately bounded reference path.

## 11. Operator release checklist

Before enabling a provider or changing a profile, verify all of the following:

- [ ] The vector-space/profile/layout/policy/publication tuple is explicit and
      matches every shard/manifest/query component.
- [ ] Original 4,096d `float32` vectors are on SSD; candidate quantization does
      not reduce the authoritative embedding dimension; exact rescore is bounded
      and required.
- [ ] Shard count and physical-byte ceilings are calculated from the linear
      envelope and measured manifests, with staging/rollback headroom.
- [ ] cgroup `memory.high` and `memory.max` include file cache; all caches and
      per-query allocations have fixed bounds.
- [ ] Candidate, page, byte, IOPS, queue-depth, await, deadline, rescore, and
      concurrency budgets are enforced rather than merely reported.
- [ ] Cold and warm results are separate and the NVMe/access-mode/hardware
      profile is recorded.
- [ ] Strict filter/ACL semantics fail closed and hydration revalidates
      authoritative state.
- [ ] Update, compaction, fsync, manifest validation, pointer CAS, rollback,
      backup, and disaster-recovery drills have evidence.
- [ ] The complete benchmark/conformance report meets every hard acceptance gate.
- [ ] No release note or report implies a backend change merely because a
      partial benchmark, cache state, or one metric looked favorable.
