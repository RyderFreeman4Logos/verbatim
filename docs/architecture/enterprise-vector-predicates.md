# Enterprise vector predicates for DiskANN3-first retrieval

Status: first walking-skeleton contract for
[#375](https://github.com/RyderFreeman4Logos/verbatim/issues/375).
Code: `crates/verbatim-core/src/enterprise_predicates/`.
Parent program:
[#369](https://github.com/RyderFreeman4Logos/verbatim/issues/369).

## Decision

Enterprise authorization and metadata constraints (source, tenant, ACL,
lifecycle, date ranges, typed metadata) are pushed into vector candidate
generation **before or during** DiskANN3 traversal. Post-filtering a global
Top-K is not an acceptable correctness or performance strategy. An unsupported
strict predicate is a typed failure, never a silent fallback to global ANN plus
best-effort filtering.

This contract is deliberately pure. It defines typed AST, selectivity
classification, generation binding, redaction, hydration revalidation, and
fail-closed errors only. It adds no live vector search, no DiskANN3 binding, no
backend integration, no SQLite, no filesystem, and no daemon wiring. Future
adapters compile the typed AST into their own internal representation; backend
JSON syntax must not become the stable API. The typed `QueryPlan` remains the
public contract.

This module is the first walking skeleton for issue #375; it owns the contract
types only. Related issues own adjacent concerns:

- [#296](https://github.com/RyderFreeman4Logos/verbatim/issues/296) owns
  deterministic structured constraints and set algebra.
- [#329](https://github.com/RyderFreeman4Logos/verbatim/issues/329) owns
  normalized metadata and temporal/lifecycle semantics.
- [#331](https://github.com/RyderFreeman4Logos/verbatim/issues/331) owns filter
  capability declarations.
- [#337](https://github.com/RyderFreeman4Logos/verbatim/issues/337) owns
  authorization policy.
- [#371](https://github.com/RyderFreeman4Logos/verbatim/issues/371) owns planner
  selection.

## Typed predicate AST

`EnterprisePredicate` is a closed typed enum — not raw backend JSON. A bounded
`EnterprisePredicateConjunction` holds up to `MAX_PREDICATES` (16) predicates.
Supported predicate kinds:

- `Source` — restrict to one source.
- `Collection` — restrict to one collection.
- `Tenant` — restrict to one tenant/workspace.
- `AclPrincipal` — authorization grant for a principal or group.
- `AclDeny` — deny precedence for a principal or group.
- `Lifecycle` — `Active`, `Archived`, or `Retained` visibility.
- `DateRange` — effective date/time range in unix milliseconds (inclusive).
- `MetadataEq` — typed metadata equality (`String`, `Integer`, `Boolean`,
  `Float`).

Authorization predicates (`Tenant`, `AclPrincipal`, `AclDeny`) are mandatory and
fail closed. Every predicate value is bounded-validated; oversized, empty, or
non-finite values are rejected with `InvalidPredicateValue`. Date ranges with
`start > end` are rejected.

## Selectivity behavior

`SelectivityClass` classifies the *authorized* candidate cardinality against
benchmark-derived `SelectivityThresholds`. No class carries a raw corpus size;
only the class and its position in the ordered crossover ladder is reported.

| Class  | Authorized-cardinality band                | Path                 |
|--------|-------------------------------------------|----------------------|
| Zero   | 0                                         | Return immediately   |
| Small  | `1..=exact_scan_max_matches`              | Exact SIMD scan      |
| Medium | `exact_scan_max_matches+1 .. pred_min-1`  | Planner-selected     |
| Broad  | `>= predicate_aware_min_matches`          | Predicate-aware ANN  |

- **Small authorized set** chooses exact full-dimensional scan rather than
  global ANN. This covers the single-vector case.
- **Medium authorized set** is planner-selected exact or predicate-aware ANN.
- **Broad authorized set** uses predicate-aware DiskANN3 traversal.
- **Zero authorized candidates** returns immediately without touching vector
  pages.
- **Unsupported strict predicate** yields a typed failure
  (`UnsupportedStrictPredicate`), never a global ANN plus post-filter.

`evaluate_predicates` returns a `PredicateEvaluation` carrying the selected
`CandidateGenerationPath`.

## Generation binding

Filter structures and query results are bound to exactly one policy generation
and one publication generation via `GenerationBinding`. A mismatch is a closed
failure; it cannot combine old and new filter structures or shards during
publication or rollback. Both `PolicyGeneration` and
`PublicationGenerationBinding` are non-zero and serde-validated.

## Hydration revalidation

Returned candidates are revalidated during authoritative hydration as defense in
depth. The `HydrationRevalidation` trait revalidates bounded
`RevalidationBatch` instances of `CandidateIdentifier`s against the authoritative
ACL, lifecycle, and tombstone state. Outcomes:

- `Accepted` — the candidate is authorized, lifecycle-visible, and not tombstoned.
- `Tombstoned` — tombstoned after candidate generation.
- `LifecycleRejected` — failed the lifecycle predicate during hydration.
- `AclRevoked` — ACL grant revoked during hydration.

Any non-accepted candidate is dropped; `revalidate_one` fails closed with
`HydrationRevalidationFailed`. Candidate identifiers never leak in diagnostics.

## Security requirements

- No unauthorized IDs, counts, distances, timing details, or payload previews
  leak in metrics, logs, `Debug`, or `Display`. Every type carrying a tenant,
  ACL principal, source id, collection id, predicate value, or candidate
  identifier renders a closed `[REDACTED]` placeholder.
- Filter structures are bound to ACL/policy generation.
- Every returned ID is validated during batched hydration.
- Sensitive predicate values are redacted from metrics/logs via `RedactionReport`.

## Fail-closed errors

`EnterprisePredicateError` is diagnostic-code-only. No variant retains
caller-controlled input. The closed taxonomy:

| Code                          | Meaning                                                    |
|-------------------------------|------------------------------------------------------------|
| `invalid_predicate_value`     | A typed predicate value failed bounded validation.         |
| `predicate_payload_too_large` | The predicate payload exceeded the bounded filter count.   |
| `unsupported_strict_predicate`| A strict predicate is unsupported by the selected path.    |
| `zero_authorized_candidates`  | The authorized candidate set is empty.                     |
| `generation_binding_invalid`  | Policy or publication generation is missing or invalid.    |
| `hydration_revalidation_failed`| A returned candidate failed hydration revalidation.       |
| `invalid_selectivity_threshold`| Selectivity thresholds are malformed or unordered.        |
| `invalid_hydration_budget`    | The candidate hydration budget is zero or malformed.       |

## Contract schema version

`ENTERPRISE_PREDICATES_CONTRACT_SCHEMA_VERSION = 1`.

Refs #375
