# Named vector spaces, multimodal routing, and late interaction

`named_vector_spaces` is the typed **VECTOR-SSD-009 walking-skeleton contract**
for a future DiskANN3-first named-vector architecture. It defines durable,
validated descriptions and routing/lifecycle facts; it does **not** implement a
live DiskANN3, Qdrant, or LanceDB multivector client, SSD I/O, vector scoring,
or #381 fusion. Existing `vector_shards`, `generation_publication`, and
`hybrid_fusion` implementations remain their owners.

## Representation and physical layout

A logical chunk or evidence object can have independently versioned derived
representations: body and title text, image/layout/audio, domain or multilingual
spaces, query/document asymmetric spaces, token/region multivectors,
classification, and duplicate detection. A `NamedVectorSpaceSpec` binds each
one to all compatibility-relevant properties:

- a bounded name and modality;
- model/document-encoder identity;
- its complete native dimension, metric, normalization, and storage encoding;
- a candidate-index profile and nonzero publication generation; and
- the supported typed query operations.

Native dimension is the dimension supplied by that model. The contract has no
MRL target dimension or truncation field; a future writer cannot represent an
arbitrary reduction as a valid space specification.

A backend-neutral `ObjectSpaceMapping` routes one stable object ID to zero, one,
or many locations **within exactly one named space and generation**:

```text
logical chunk/evidence ID
  -> body-semantic shard / vector ID
  -> title-semantic shard / vector ID
  -> image shard / vector ID(s)
  -> late-interaction shard / page-aligned vector range
```

This is intentionally not “one database point contains every representation.”
Each physical index remains homogeneous in dimension, metric, encoder, and
profile. The mapping has a bounded number of compact locations and no pairwise
cross-space/cross-source field, so it cannot materialize representation pairs.
The storage contract explicitly accounts only for:

```text
O(sum(N_space * D_native))
```

with original native vectors retained where exact interaction/audit requires
them. The contract rejects invalid/overflowing storage terms rather than
claiming a hidden quadratic arrangement.

## Typed query routing and partial state

`NamedVectorQueryPlan` compiles only the explicitly requested named clauses.
It gives every eligible space/modality a single shared `SearchBudget`, validates
the clause shape against the native specification, and binds the output to an
explicit versioned `FusionProfileIdentity` for #381. It does not implement that
fusion profile. `SpaceCandidate` preserves raw rank, raw score, vector-space
identity, candidate-index-profile identity, and an inclusion reason before
future fusion and hydration.

Capability handling is fail-closed. An adapter without named-vector or
late-interaction capability produces a closed `unsupported_backend_capability`
diagnostic. A `DegradedProfile` is named, bounded data that a future adapter
must explicitly select; passing it to ordinary compilation cannot silently
substitute a mode. Missing, unavailable, optional, stale, and wrong-generation
spaces are represented by `SpaceAvailability`, not hidden by fallback.

## Late interaction on SSD

For ColBERT-style multivectors, the contract distinguishes two stages:

1. `LateInteractionCandidateStage` limits the query-token frontier and object
   candidate pool. It may be approximate and therefore its **candidate recall**
   is measured separately.
2. `ExactInteraction::MaxSimFullPrecision` requires the original vectors and is
   the declared final interaction. Its **final interaction quality** is a
   separate measurement; exact rescoring of retrieved candidates does not make
   an approximate candidate stage exact.

`VectorRange` records nonempty page-aligned contiguous token/region ranges,
which is the required SSD-friendly layout for bounded online rescoring. Future
physical implementations should keep compatible candidate indexes and offset
maps on SSD, fetch only these contiguous original-vector pages, and report
candidate recall alongside final MaxSim quality.

## Publication, deletion, and retention

`StagedSpaceArtifact` represents per-space plus mapping artifacts. An atomic
`NamedVectorPublicationManifest` binds every listed complete, optional,
unavailable, or stale space to one generation. Replacement and deletion use
versioned `DerivedRepresentationOperation`; `TombstoneAll` explicitly denotes
removal of every derived representation. Independent retention is allowed only
when `SpaceRetentionRequest` proves no evidence remains referenced, preventing
GC from deleting an active representation.

All durable constructors are re-run during serde deserialization. Diagnostics
are a closed code-only taxonomy; public error `Debug` and `Display` expose no
caller-controlled object ID, model identifier, shard path, score, or receipt.
No sealed adapter marker is exported and the module exposes no free-form
failure-receipt minting API.

## Backend positioning and deliberate boundary

DiskANN3 remains the intended primary low-DRAM SSD implementation: it should
use vector-space-specific indexes plus this mapping layer. Qdrant named vectors
and multivectors are a native-capability reference; LanceDB can expose the
compatible reference subset. Their dimension/metric/index constraints must be
reported through capability/state types, never concealed. This module provides
no client integration or assertion that those backends already conform.

See also [immutable vector shards](immutable-vector-shards.md),
[Qdrant reference backend](qdrant-reference-backend.md), [LanceDB reference
backend](lancedb-reference-backend.md), and [hybrid fusion](hybrid-fusion.md).

## Related work

- #331 — metric, normalization, and profile semantics
- #372 — primary vector adapter
- #373 — shard identity and mapping
- #376 — original-vector exact scoring
- #379 — atomic publication
- #381 — versioned hybrid fusion
- #383 — Qdrant reference capability
- #384 — LanceDB reference capability
