# AISAQ co-located SSD page layout contract

Status: first walking-skeleton contract for [#374](https://github.com/RyderFreeman4Logos/verbatim/issues/374).
Code: `crates/verbatim-core/src/page_layout/`.
Parent program: [#369](https://github.com/RyderFreeman4Logos/verbatim/issues/369).

## Purpose

This module defines a **pure contract** for AISAQ-style co-located SSD page
layouts on the DiskANN3 `DataProvider`. It encodes the near-zero-DRAM
co-location trade from AISAQ: duplicate a bounded amount of compressed
neighbor information beside graph records so candidate codes do not have to
remain proportional to corpus size in RAM and search requires fewer unrelated
random reads. Disk use rises by a constant factor but stays O(N).

This slice is deliberately pure. It contains:

- no live SSD I/O;
- no upstream DiskANN3 dependency or binding;
- no daemon wiring;
- no ANN core;
- no O(N) in-RAM code table.

```text
verbatim-domain / verbatim-core / retrieval / storage ports
                              |
                  AISAQ page-layout contract (this module)
                              |
                  future DiskANN3 DataProvider adapter
                              |
                  co-located SSD pages (graph + candidate codes)
```

## Why co-locate

A conventional disk ANN arrangement may keep compressed/PQ representations in
RAM while storing the raw vectors and graph on SSD. At millions of
4,096-dimensional vectors, even a heavily compressed representation can exceed
Verbatim's desired online memory budget. Simply mmaping everything does not
solve the constraint because page cache is memory and cold random graph
traversal can collapse tail latency.

AISAQ demonstrates a different trade: duplicate a bounded amount of compressed
neighbor information beside graph records on disk so one page read can
evaluate more expansion choices. Disk use rises by a constant factor, but
remains linear in N and can reduce DRAM and IOPS. This matches the product
requirement that SSD may scale with information while memory stays in the tens
or hundreds of MiB.

## Contract types

| Type | Role |
| --- | --- |
| `PageLayoutStrategy` | Enum of three comparable layouts: `VectorFirst`, `GraphFirst`, `ColocatedScale`. |
| `PageSize` | Validated power-of-two page size (4 KiB / 16 KiB / 64 KiB / custom aligned), within the NVMe floor and a ceiling. |
| `PageAlignment` | Validated power-of-two byte alignment that evenly divides the page size. |
| `ChecksumPolicy` / `PageChecksum` | Torn-page detection: SHA-256 truncated to 8 bytes, with an enable/disable policy. |
| `ColocationRule` | Which data co-locates beside a graph vertex, and the accepted linear SSD-redundancy tradeoff. |
| `ReadAmplificationBound` | Bounded reads per query (max pages, max bytes), with a typed exhaustion state. |
| `PageLayoutSpec` | Aggregating, fully cross-validated specification binding to `SearchBudget`. |
| `PageLayoutError` | Fail-closed, diagnostic-code-only error; `Copy`, no payload, redacted `Debug`/`Display`. |

## The three layouts (provider variants)

The issue requires at least three comparable layouts behind the same DiskANN3
and Verbatim contracts. This contract names them and records each tradeoff:

1. **`vector-first`** (`VectorFirst`) — upstream/reference disk layout. Vectors
   and graph stored separately on SSD; compressed/PQ candidate codes kept
   proportional to corpus size in RAM. Maximally conservative on SSD
   redundancy; highest DRAM of the three. Maps to the issue's
   `standard-diskann` provider variant. Pairs with `ColocationRule::Separated`
   (redundancy factor 1).

2. **`graph-first`** (`GraphFirst`) — graph vertex, neighbor IDs, candidate
   codes, and necessary metadata arranged to minimize reads, accepting more
   linear SSD redundancy. Trades a bounded constant-factor SSD increase
   (ceiling 4x) for lower DRAM and IOPS. Maps to the issue's
   `colocated-performance` provider variant. Pairs with
   `ColocationRule::FullColocation`.

3. **`colocated-scale`** (`ColocatedScale`) — less redundancy and more SSD
   I/O, targeting the smallest SSD footprint while still avoiding an O(N)
   in-RAM code table (ceiling 2x). Maps to the issue's `colocated-scale`
   provider variant. Pairs with `ColocationRule::PartialColocation`.

All three strategies keep disk growth O(N); the redundancy factor ceiling is
the documented constant factor on that linear term, not a live measurement.

## Co-location rules and redundancy

Each `ColocationRule` carries a `redundancy_factor` validated against its
strategy's ceiling:

| Strategy | Compatible rule | Redundancy ceiling |
| --- | --- | --- |
| `VectorFirst` | `Separated` | 1 |
| `GraphFirst` | `FullColocation` | `COLOCATION_REDUNDANCY_CEILING_PERFORMANCE` (4) |
| `ColocatedScale` | `PartialColocation` | `COLOCATION_REDUNDANCY_CEILING_SCALE` (2) |

Compatibility is fixed: a co-locating rule never pairs with the
non-co-locating reference strategy, and vice versa. A co-locating rule must
run under checksums so a torn page cannot silently corrupt duplicated
candidate codes; the separated reference rule may run without checksums.

## Page sizes and alignment

Page sizes are constrained to power-of-two values no smaller than the NVMe
logical-block floor (4 KiB) and no larger than a validated ceiling (1 MiB).
Alignment must be a power of two that evenly divides the page size. These are
the physical-design inputs the issue lists as empirical questions (4 KiB,
16 KiB, 64 KiB, or measured alternatives); this module validates them without
performing I/O.

## Read-amplification bounds and budget binding

`ReadAmplificationBound` bounds reads per query two ways: by a maximum number
of SSD pages and by a maximum number of bytes. The byte budget must be at
least one page so a single vertex expansion is always possible.
`ReadAmplificationExhaustion` is the typed partial state returned when a bound
is exceeded.

`PageLayoutSpec::bind_to_budget` maps the spec's `max_pages` onto
`SearchBudget::max_ssd_pages` and its `max_bytes` onto
`SearchBudget::max_bytes_read`, rejecting any widening. A future provider
returns the typed partial search state required by the `SearchBudget`
exhaustion rule when this binding is exceeded.

## Checksums and torn-page detection

Every co-located page carries a `PageChecksum`: SHA-256 truncated to 8 bytes
over the page payload. `PageChecksum::verify` recomputes and rejects a
mismatch, indicating a torn write or corruption. The digest is never stored
on errors, so a partial page payload cannot leak through `Debug`/`Display`.

## Quality rule

Original full 4,096-dimensional `float32` vectors are always preserved
**separately** on SSD. Co-located compressed representations are
candidate-generation aids, not replacements for exact originals.
Full-precision rescoring runs under a separate contract
([#376](https://github.com/RyderFreeman4Logos/verbatim/issues/376)). Any
recall difference from provider layout or candidate compression must pass the
same exact-ground-truth gate.

## Fail-closed discipline

All validation rejects invalid input. Errors are diagnostic-code-only: no
payload, vector data, neighbor IDs, offsets, or checksum bytes are retained on
an error. There is no `unwrap`/`expect`/`panic` in production code. The error
type is `Copy` and renders only a stable diagnostic code string.

## Algorithmic lineage and license review

The co-location trade is based on **published interfaces and research**, not
copied private Milvus internals:

- DiskANN3 `DataProvider` interface:
  https://github.com/microsoft/DiskANN
- AISAQ documentation (published Milvus docs):
  https://milvus.io/docs/aisaq.md
- Linux cgroup v2 (for the resource tests called for by the issue):
  https://docs.kernel.org/6.10/admin-guide/cgroup-v2.html

License review note: this contract module adds no upstream DiskANN3 dependency
and copies no private Milvus internals. The design is derived from the
published AISAQ documentation and the public DiskANN3 `DataProvider` interface
description. The implementation must continue to be based on published
interfaces and research.

## What this contract does not do (yet)

This is a walking skeleton. The following remain for follow-up issues:

- live SSD I/O and a real DiskANN3 `DataProvider` adapter;
- adjacency and candidate-code packing formats;
- optional neighbor-code duplication limits beyond the ceiling;
- direct I/O versus buffered I/O versus mmap under cgroup limits;
- asynchronous prefetch and queue depth;
- per-query read coalescing;
- hot entry-point and upper-layer caching with fixed caps;
- update/delta-page behavior without rewriting unbounded regions;
- the cgroup-bounded resource and recall measurements called for by the issue.

Refs #374.
