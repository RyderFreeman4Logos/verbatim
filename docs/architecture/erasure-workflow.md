# Cross-backend deletion and verifiable erasure (RIGHTS-002)

Status: walking skeleton for [#363](https://github.com/RyderFreeman4Logos/verbatim/issues/363).
Code: `crates/verbatim-core/src/erasure/`.

## Purpose

This is a pure, serializable contract for a source-bounded deletion lifecycle.
It inventories authoritative and derived products across SQLite, search/vector
indexes, graph materialization, blobs, caches, workflow artifacts, and Qdrant;
then it fails closed while the future adapter propagates deletion and reconciles
remote work. It deliberately does **not** open a database, invoke a daemon, or
contact a remote backend.

```text
DeletionScope (logical-delete stale-read fence)
  → DeletionPlan (policy + complete matrix validation)
  → authoritative → derived → cache → remote replica propagation
  → RemoteReconciliation (retry + dead-letter + operator alert)
  → DeletionProof (content and identifiers omitted)
```

## Complete inventory and ordering

`DeletionPropagationMatrix::canonical()` requires exactly one entry for every
target. Its order cannot be changed or partially selected:

| Phase | Targets | Product / deletion state |
| --- | --- | --- |
| Authoritative | SQLite source/evidence/chunks/vectors/cache rows | `authoritative` / `tombstone` |
| Derived | Tantivy, HNSW, graph nodes/edges, tasks, workflow artifacts | `derived_rebuildable` / physical erase or quarantine |
| Derived | graph reports | `retained_for_audit` / delayed backup expiry |
| Derived | blobs, images, exports, temporary uploads | `must_delete` / immediate physical erase |
| Cache | query, context, answer cache | `tombstoned` / logical delete |
| Remote replica | Qdrant | `derived_rebuildable` / immediate physical erase |

The complete `DataProduct × DeletionTarget × DeletionState` space is exercised
by the unit suite. Only the canonical classification/state pair for each target
is accepted; all other combinations fail closed. This guards against a future
adapter silently treating an index, cache, or remote replica as optional.

## Fail-closed lifecycle rules

1. `DeletionScope::new` starts at `logical_delete`, and `blocks_serving()` is
   always true. Deleted/revoked material must not serve while cleanup is
   asynchronous.
2. Every scope must include every matrix target and matching product
   classification. Duplicate/blank source IDs and partial target sets reject
   without echoing caller input in an error.
3. `DeletionPolicy` requires stale-read fencing and propagation to cache keys,
   active cursors, derived artifacts, and model eligibility. No one surface may
   be disabled for a deletion plan.
4. `legal_hold` rejects plan creation before propagation. `DeletionState` also
   makes legal hold terminal for deletion transitions.
5. Remote failure is never best effort: Qdrant failures require a positive,
   bounded retry policy, a persisted dead-letter state, and an operator alert.
   A reconciliation receipt must exactly match pending remote targets to those
   remote failures.
6. Errors expose only `ErasureDiagnosticCode` in `Display` and `Debug`. They
   retain no free-form source IDs, content, paths, backend responses, or other
   untrusted strings.

## Retention and cryptographic erasure

`retained_for_audit` records use `delayed_backup_expiry`; the policy carries a
backup retention window. When physical backup rewrite is impractical,
`CryptographicErasure` requires `KeyRotationRequirement::Required`. A future
adapter must rotate/revoke the data-encryption key and record its completion;
this contract makes no claim that a backup rewrite, KMS call, or key rotation
has already occurred.

## Redaction-safe proof

`DeletionProof` stores a schema version, source count, target outcomes, and a
SHA-256 commitment over private source identifiers. It intentionally omits the
identifiers themselves and all deleted/restricted content, excerpts, vectors,
and credentials. `DeletionProof::verify_for` recomputes the commitment and
outcomes from a private plan plus reconciliation receipt. Proofs can therefore
be retained as audit evidence without serving as a content leak.

## Adapter boundary and residual work

`DeletionWorkflow` forces the only lifecycle sequence:
`plan → propagate → reconcile → report`. Future implementations must use the
validated `DeletionPlan`, record ordered acknowledgements, persist dead-letter
work, issue alerts, enforce the stale-read fence at every serving surface, and
only then construct/verify a proof.

Still intentionally out of scope for this first contract slice:

- live SQLite, Tantivy, HNSW, Qdrant, graph, blob/image, cache, and task wiring;
- database transactions, remote retries, KMS/key rotation, backup media rewrite,
  operator-alert transport, or durable audit storage;
- daemon/CLI scheduling, retention-expiry jobs, and runtime authorization hooks;
- issue-state changes.
