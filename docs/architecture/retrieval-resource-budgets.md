# Retrieval resource budgets

Status: first walking-skeleton contract (Refs #377).

Code: `crates/verbatim-core/src/retrieval_budgets/`.

## Decision

Retrieval resource use must be bounded independently of corpus size. RSS
alone is insufficient: Linux cgroup v2 accounts page cache and anonymous
memory together via `memory.current`, `memory.high`, and `memory.max`. A
serving process must gate on the **total** cgroup usage, including file
cache, and every corpus-proportional structure must carry a fixed configured
maximum.

This contract is deliberately pure. It defines validated budget types,
typed exhaustion states, diagnostic-only errors, and process-isolation
specs, but has no live cgroup reader, no resource monitor, no DiskANN3
binding, no runtime spawn, and no semaphore. Future adapters must implement
these boundaries before they can participate in retrieval.

Parent program: https://github.com/RyderFreeman4Logos/verbatim/issues/369.

## Contract surface

### Memory budgets

`MemoryBudgetProfile` — cgroup-aware process memory caps.

| Field              | cgroup v2 file    | Meaning                                      |
|--------------------|-------------------|----------------------------------------------|
| `current`          | `memory.current`  | Observed total usage including file cache     |
| `high`             | `memory.high`     | Throttle threshold; reclaim pressure begins  |
| `max`              | `memory.max`      | Hard ceiling; OOM / hard failure begins      |

Invariants:

- `high < max` (high must leave reclaim headroom).
- `max` is at or above the role-specific floor
  (`ONLINE_MEMORY_MAX_FLOOR = 64 MiB`, `BUILD_MEMORY_MAX_FLOOR = 128 MiB`).
- `check_current` returns a typed code: `MemoryHighExceeded` between high
  and max, `MemoryMaxExceeded` over max.

Walking-skeleton profiles match the issue target:

```text
online serving memory.high = 192 MiB
online serving memory.max  = 256 MiB
isolated build memory.high = 384 MiB
isolated build memory.max  = 512 MiB
```

`PerQueryMemoryCaps` — hard caps for every per-query working set:

- read buffers (decompression, vector read, page read);
- predicate bitmap working sets;
- per-query graph frontier / visited state;
- exact-rescore candidate pool (full-precision vectors);
- Arrow / serialization batch buffers;
- lexical / graph / fusion candidate pools (pre-rerank);
- hydration text / evidence buffers.

Every allocation proportional to a request parameter must validate against
the effective `PerQueryMemoryCaps` before allocation.

### Corpus-proportional caches

`CacheCapacity` — fixed configured maximum for every cache:

- page cache;
- mapping cache;
- graph cache;
- DiskANN entry-point cache;
- decompression cache;
- hydration cache.

No unbounded corpus-proportional structure is permitted. `max_bytes` must
admit at least one full entry (`max_bytes >= entry_bytes`).

### I/O budgets

`IoBudget` — hard SSD I/O caps extending `SearchBudget`:

| Field                     | Meaning                                          |
|---------------------------|--------------------------------------------------|
| `max_pages`               | Maximum SSD pages read per request               |
| `max_bytes`               | Maximum bytes read per request                   |
| `max_iops`                | Maximum read operations per request              |
| `max_queue_depth`         | Maximum outstanding read operations              |
| `max_await_micros`        | Maximum cumulative await time (microseconds)     |
| `max_read_amplification`  | Pages read per vector rescored (fixed-point)     |
| `access_mode`             | `Direct`, `Buffered`, or `Mmap`                  |

A request may not traverse indefinitely after its deadline or page cap.
Exhaustion is typed via `ResourceExhaustion` (see below).

### Concurrency budgets

`ConcurrencyBudget` — bounded concurrent work:

- shared `max_workers` across all categories;
- per-category sub-limits: retrieval, storage, model, background;
- each sub-limit `<= max_workers` so no single category can starve the
  others or evict the entire search working set;
- no per-shard thread or allocator arena.

### Process isolation

`ProcessIsolationSpec` — separate build/compaction from online serving:

- opaque `cgroup_slice_id` (the contract never stores a caller-controlled
  path string);
- `CpuPriorityClass`: best-effort, normal, elevated;
- `IoPriorityClass`: idle, best-effort, realtime;
- `dedicated_workers`: build/compaction must declare `true`;
- `is_build_isolated()`: true when dedicated + best-effort CPU + idle IO.

Deterministic shutdown cancels I/O and releases mapped files/caches — this
is an adapter responsibility; the contract declares the spec only.

## Typed exhaustion

`ResourceExhaustion` — the single enum covering all budget dimensions:

```text
MemoryHighExceeded
MemoryMaxExceeded
PageBudgetExceeded
ByteBudgetExceeded
IopsExceeded
AwaitExceeded
ReadAmplificationExceeded
ConcurrencySaturated
DeadlineExceeded
```

Budget exhaustion is typed and observable, never an unmarked empty or
partial result. `ResourceAccount` is the running ledger: each charge is
checked immediately, the first exceeded dimension returns its typed code,
and subsequent charges fail fast with the same code.

## Error model

All errors are fail-closed and diagnostic-code-only. `RetrievalBudgetError`
carries no payload — no cgroup path, no caller identifier, no vector data.
The redacted `Debug` renders only the diagnostic code name; `Display`
prefixes with `retrieval-budget.`.

## Future adapters

Adapters that bind this contract to live systems must:

1. Read cgroup v2 files (`memory.current`, `memory.high`, `memory.max`) and
   feed observed values to `MemoryBudgetProfile::check_current`.
2. Enforce every `CacheCapacity` at cache construction; reject unbounded
   caches.
3. Wrap every read path in a `ResourceAccount` bound to the request's
   `IoBudget`; surface `ResourceExhaustion` as a typed partial/failure.
4. Gate concurrent work through a shared semaphore sized to
   `ConcurrencyBudget::max_workers` with per-category sub-semaphores.
5. Launch build/compaction under a `ProcessIsolationSpec` with
   `dedicated_workers = true`.

## References

- https://docs.kernel.org/6.10/admin-guide/cgroup-v2.html
- `docs/architecture/search-budget-planner.md`
- `docs/architecture/exact-filtered-scans.md`
- `docs/architecture/aisaq-page-layout.md`
- `docs/architecture/overfetch-elimination.md`
