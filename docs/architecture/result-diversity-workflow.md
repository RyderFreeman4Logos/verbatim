# Result diversity / near-duplicate collapse workflow (issue #361)

Status: walking skeleton for [#361](https://github.com/RyderFreeman4Logos/verbatim/issues/361).
Code: `crates/verbatim-core/src/result_diversity/`.

## Purpose

A retrieval ranker can legitimately return many chunks which repeat one passage,
parent/child copy, thread fragment, mirrored source, or source version. This
contract lets a presentation or context-building adapter make that redundancy
inspectable **without changing the ranking used for recall or exhaustive
accounting**.

```text
immutable RawCandidateRanking
  → group identity + recorded collapse reason
  → direct-evidence-safe representative selection
  → DiversityStageOutput (representatives + all grouped members + raw ranks)
```

## Contract summary

| Type | Role |
| --- | --- |
| `RawCandidateRanking` / `RawCandidate` | Immutable raw hit identity, rank, evidence strength, semantic distinction, and non-zero exhaustive occurrence count. No public mutation API rewrites the rank or count. |
| `RawRank` / `OccurrenceCount` | Non-zero typed values retained with both raw and collapsed views. |
| `GroupIdentity` | Explicit `exact_duplicate`, `near_duplicate`, `overlap`, `parent_child`, `thread`, `source`, `mirror`, or `version` provenance. |
| `DiversityProfile` | Policy version, deterministic SHA-256 profile hash, near-duplicate threshold, optional source/thread quotas, and optional MMR hook. |
| `DiversityGroup` / `GroupedMember` | Representative and all original group members; every member keeps its raw rank and each non-representative has a collapse reason. |
| `DiversityStageOutput` | Auditable projection containing the profile, complete raw ranking, groups, and checked usage. JSON decode revalidates profile, raw-ranking, attribution, and usage invariants. |
| `ExploratorySearch`, `PrecisionRetrieve`, `ContextPack`, `Exhaustive` | Type-level mode markers for adapters. |
| `DiversityRun` / `DiversityStage` | Ordered `grouping → selecting_representatives → emitting_collapse_report → terminal` state machine. |
| `ResultDiversityWorkflow` | Async adapter boundary: `group → select_representatives → emit_collapse_report`. |

## Fail-closed rules

1. A `DiversityStageOutput` rejects missing or duplicated group members. Every
   raw candidate must be attributed exactly once, so a collapsed item is still
   discoverable through `groups()` and `collapsed_member()`.
2. Raw ranks and exhaustive occurrence counts are private, immutable fields on
   raw candidates. A diversity output retains the full `RawCandidateRanking`;
   collapse cannot overwrite occurrence accounting.
3. Group construction requires the representative to have the strongest
   `EvidenceStrength` in its group. This prevents merely diverse thematic or
   corroborating evidence from replacing direct evidence.
4. `LegallyDistinctVersion` and `SemanticallyDistinctTranslation` candidates
   reject a `NearDuplicate` collapse reason. An adapter needs explicit identity
   or policy evidence (for example `ExplicitEquivalentVersion`) rather than a
   similarity score alone.
5. Group provenance keys, profile version, thresholds, quotas, member IDs, raw
   ranks, and budget caps validate before a stage report is accepted. Budget
   excess and illegal stage transitions use typed errors.
6. MMR is only a versioned profile hook in this slice. Embedding calculation,
   scoring, and live ranker behavior remain outside `verbatim-core`'s contract.

## Mode and pagination notes

The type markers force a future adapter to state whether a projection is for
exploratory search, precision retrieval, a context pack, or exhaustive work.
They do not alter raw rank ordering. Pagination/cursor adapters should bind a
page to the raw ranking snapshot and profile hash, then expose representatives
only as a projection; regrouping cannot silently shift the underlying raw
cursor or exhaustive occurrence total.

## What this slice wires

- Pure serializable types, profile hashing, validation, typed budget/errors,
  state machine, and adapter trait
- Explicit group identities and collapse reasons
- Immutable raw ranks/occurrence counts carried into an inspectable collapse
  report
- Direct-evidence and distinct-version/translation fail-closed guards

## Residual work

- Live retrieval/ranking/model adapters, embeddings, and MMR implementation
- Source/thread metadata resolution and real threshold evaluation
- Store/SQL/filesystem/daemon/CLI wiring and cursor integration
- UI "show more from group" behavior, benchmark corpora, and issue-state changes
