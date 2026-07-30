# DiskANN3 retrieval-service contract

Status: `DIST-SSD-001` types-only walking skeleton (Refs #386).

Code: `crates/verbatim-core/src/diskann3_service/`.

## Boundary and honesty

The service owns **derived**, immutable vector shards and compact routing/filter
summaries. It is not the authoritative catalog, evidence database, or evidence
hydration API. Requests carry a validated service/vector-space/profile/generation
identity, metadata predicate plan, authorization context, trace correlation token,
`SearchBudget`, one shared deadline, 4,096-dimensional finite query vector, and idempotency key. Responses carry
only compact numeric IDs, raw scores, the serving generation, explicit completion, and
aggregate work telemetry. They never include full evidence text.

This is a pure contract slice: it creates no gRPC server or protobuf output, makes no
network or DiskANN calls, starts no daemon, probes no health endpoint, enforces no
cgroup, and performs no cancellation itself. The protocol and adapter types state the
required conformance surface for later implementations; they do not claim that those
runtime features exist.

## Semantic transport surface

`VectorSearchAdapter` is deliberately sealed. `InProcessAdapter` and `RemoteAdapter`
share the same identity-preserving semantic surface. `ProtocolSearchRequest` provides
the version-1 wire-equivalent operation envelope for search; the versioned operation
enum also reserves range search, exact rescore, staged mutations, checkpoint/validate,
capability/generation discovery, health/readiness/shard status, and cancellation.
A remote implementation must preserve generation, predicate, budget, deadline,
idempotency, and completion semantics rather than re-plan or reset work.

## Routing and failures

The pure router first checks authorization, then matches tenant/source/collection/ACL
metadata **before** vector I/O. Authorization uncertainty fails closed, and an
unattested or incomplete ACL cannot become a partial answer. A manifest is bound to
one request identity and immutable generation; stale requests and mixed identities are
rejected. Fan-out is fixed by `ShardRouterConfig`. Required unavailable shards remain
an explicit partial route, never an unmarked empty result. Health/circuit state is an
input to routing/admission, not a live probe in this slice.

## Replicas, durability, and storage

`ImmutableReplicaSet` accepts local-NVMe immutable replicas for one generation only.
`ActiveGenerationSet` permits exactly one active set, preventing incompatible dual
active generations during rollout/rollback. Mutable shared NFS and SMB index storage
are explicit rejected deployment modes. Build, validation, and publication occur
outside this contract and publish a new immutable manifest/pointer.

ANN files are not a durability claim for updates or deltas. `DeltaRecoveryContract`
requires an authoritative durable-log/recovery contract; ANN-library-only durability
is rejected. This slice does not implement replication, recovery, or a mutable index.

## Backpressure and isolation

`BackpressureConfig` gives fixed active-query and queue bounds, per-tenant work caps,
and an original/retry `SearchBudget` relation. Retry work must be narrower, so there is
no budget reset or retry storm. `BackpressureGate` emits typed circuit, active-query,
and queue overload codes. Build, update, and compaction have distinct worker-pool
identities; live cgroup/process placement and deterministic cancellation of disk reads
remain requirements for the later runtime implementation.

## Follow-up runtime work

A production implementation must generate the declared protobuf/gRPC surface; bind a
real DiskANN3 `DataProvider` over locally owned NVMe shards; enforce global/per-tenant
memory, I/O, queue, retry, deadline, and cancellation controls; maintain health-aware
replica selection; and prove cgroup page-cache behavior. It must not silently replace
these boundaries with an NFS/SMB mutable index or post-filtered global ANN search.
