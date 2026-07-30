# Legacy SQLite/HNSW serving cutover

**Status:** typed walking skeleton — no live cutover automation.
**Scope:** MIGRATE-SSD-001 / Refs #388.

## Decision

DiskANN3 is the default **full-dimensional enterprise** vector-serving path only
*after* every cutover gate below passes for a candidate publication generation.
Compilation alone does not authorize a cutover. The bounded exact filtered scan
remains available for tests and deliberately small-scope production work.

The following production serving identities are explicitly being retired, not
silently reinterpreted as the exact-scan reference path:

1. `low_memory` SQLite whole-table vector serving;
2. `resident_hnsw` based on `instant-distance`;
3. unconditional local pre-search before a selected remote backend.

Configurations and operator guidance must not present any of those paths as the
enterprise recommendation after this cutover. Historical benchmark readers may
remain when useful.

## Contract boundary

`verbatim_core::legacy_vector_cutover` is a pure, fail-closed contract surface.
It has closed diagnostic codes; public `Display` and `Debug` output contains
only those codes. Durable `PublicationGeneration` and `CutoverManifest` values
deserialize through validating constructors. `MigrationValidation` and
`ReleasePolicyApproval` have private fields and can only be obtained through
their validating constructors; the final retirement boundary requires the
validation together with the exact reusable `AuthoritativeVectorSource`
(including its concrete source generation) and concrete `CutoverManifest` it
bound. The module does **not** run a DiskANN3 build, contact a service, observe
cgroups, delete SQLite/HNSW artifacts, re-embed vectors, or alter runtime
routing.

A real operator/automation implementation must collect the evidence described
here and invoke the typed boundary before it performs side effects. This design
therefore makes no claim that live cutover automation exists.

## Gates before promotion and retirement

All of these independent gates must pass, with no fallback for missing
measurements:

- `VectorSearch` conformance;
- exact metric, embedding profile, and source-generation validation;
- filtered authorized-subset recall against the exact reference;
- cgroup memory and SSD-I/O gates;
- cold and warm latency plus concurrency gates;
- update, delete, compaction, and recovery exercises;
- staged dual-generation shadow comparison of results and resources;
- rollback and disaster-recovery exercise;
- operator documentation and migration-tooling readiness.

`CutoverGates::compile_only()` is deliberately rejected with the distinct
`diskann3_compile_only` diagnostic. Every missing gate class emits its own
stable diagnostic code. A passed shadow can bind a promotion only when the
same `CutoverGates` aggregate is complete; there is no ungated public promotion
binding.

## Migration and publication lifecycle

```text
authoritative stored vector bytes + validated profile
  -> build a DiskANN3 candidate generation without re-embedding
  -> validate counts, hashes, dimensions, metric, normalization, IDs, filters,
     sampled exact recall, mutation/recovery artifacts, resources, and manifest
  -> shadow distinct incumbent and candidate generations with mirrored traffic
  -> bind promotion to the exact shadowed candidate generation and complete gates
  -> publish under the generation-publication contract (Refs #379)
  -> retain incumbent through declared rollback window
  -> after the window and release policy, perform backup-aware maintenance
```

When profile metadata or authoritative vector bytes are invalid, the contract
returns `ReembeddingRequired`; it rejects any attempt to proceed as a silent
re-embedding. Target dimensionality must equal the authoritative dimension;
there is no quality-loss or dimension-reduction path.

Promotion is bound to the candidate generation observed by the passed,
**distinct-generation** shadow comparison and requires the complete aggregate
of cutover gates. Final retirement additionally checks that the source and
manifest are precisely those bound into the non-forgeable migration-validation
artifact. This aligns the cutover with the publication-generation guarantee:
queries must not mix an incumbent and candidate generation.

## Retention and destructive maintenance

The previous generation remains readable for the declared rollback window.
Legacy artifacts are not eligible for removal until that window expires and a
validated `ReleasePolicyApproval` exists. Removal plans require verified backups
and an explicit disposition for both serialized resident-HNSW artifacts and
stale vector JSON copies. The policy token cannot be field-constructed outside
the module, and a missing or denied policy returns the closed
`release_policy_approval_required` diagnostic.

Retirement must retain:

- authoritative stored vector bytes and profile metadata required to rebuild;
- bounded exact scan support;
- backend conformance fixtures;
- historical benchmark readers where useful.

This prevents removal of a production legacy path from also removing canonical
rebuild material or the reference behavior used to validate future generations.

## Related contracts

- `generation_publication` (Refs #379): atomic generation publication,
  promotion, rollback, and dual-generation discipline.
- `exact_scan` (Refs #376): bounded exact filtered scans and exact-reference
  rescoring/recall.
- `ssd_vector_benchmark`: quality and resource gate vocabulary.
- `diskann3_service`: the types-only shared `VectorSearch` service boundary.
