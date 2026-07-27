# SQLite durability profiles (OPS-006)

Status: walking skeleton for [#362](https://github.com/RyderFreeman4Logos/verbatim/issues/362).
Code: `crates/verbatim-core/src/durability/`.

## Purpose

This is a pure, serializable policy contract for selecting and validating a
SQLite durability profile. It names the intended pragmas, checkpoint cadence,
disk reserve behavior, publication ordering, abnormal-recovery checks, and
local RPO/RTO boundary before a future adapter wires them to SQLite or daemon
lifecycle code.

```text
DurabilityProfile
  → DurabilityConfig validation
  → DiskSpacePolicy preflight / fail-closed SQLITE_FULL or ENOSPC
  → PublicationOrder source → index → task → cache
  → RecoveryPolicy after abnormal shutdown
```

## Explicit profile defaults

| Profile | `journal_mode` | `synchronous` | WAL auto-checkpoint | busy timeout | scheduled checkpoint | power-loss RPO / RTO |
| --- | --- | --- | ---: | ---: | --- | --- |
| `durable` | `WAL` | `FULL` | 1,000 pages | 30 s | `FULL` every 30 s | acknowledged commits / 300 s |
| `balanced` | `WAL` | `NORMAL` | 1,000 pages | 10 s | `PASSIVE` every 60 s | unbounded power-loss loss / 600 s |
| `ephemeral` | `DELETE` | `OFF` | 100 pages | 1 s | `TRUNCATE` every 300 s | no power-loss guarantee / 900 s |

`DurabilityProfile::default()` is `balanced`; callers must choose a profile
explicitly when their deployment needs a stronger or weaker stated guarantee.
`DurabilityConfig::validate_for` accepts only the named profile's complete
contract defaults. In particular, `durable` always rejects non-`WAL` journaling
and any synchronous mode other than `FULL`.

## Fail-closed rules

1. `DiskSpacePolicy` requires a non-zero reserve and an alert threshold at or
   above it. `SQLITE_FULL` and `ENOSPC` surface only as closed diagnostic codes
   and reject the write while preserving the active generation; no best-effort
   publication fallback exists in this contract.
2. `PublicationOrder` accepts exactly `source replacement → index publication
   → task status → cache invalidation`. A future commit-boundary adapter must
   validate this order before publishing derived state.
3. After an abnormal shutdown, `durable` and `balanced` require both SQLite
   integrity and foreign-key checks. `ephemeral` records that these checks are
   not a profile requirement.
4. The database, rollback files, and WAL **must remain on one host and one
   local filesystem**. Network filesystems, split-host database/WAL layouts,
   and cross-filesystem rename assumptions are outside the contract and must be
   rejected by future deployment wiring.
5. Error `Display` and `Debug` contain only a closed diagnostic code; they do
   not retain filesystem paths, SQL, OS text, or other arbitrary input.

## RPO/RTO and DR-001

The RPO rows above apply only to local host power-loss behavior. They do not
protect against host, storage-media, site, or operator loss. Every profile's
`RpoContract` therefore requires DR-001 backups for host or media loss. The
contract does not claim a backup implementation, scheduling policy, restore
exercise, or an off-host RPO; those belong to DR-001 integration.

## What this slice wires

- Pure serializable profile/config/policy types and JSON config round trips
- Typed diagnostic-only validation errors
- Fail-closed disk-full, publication-order, recovery, and DR-001 requirements
- A `DurabilityProfileWorkflow` adapter trait ordered as `resolve_profile →
  validate_config → checkpoint → recover`
- Focused unit coverage for all profiles × journal/synchronous combinations

## Residual work

- Applying and verifying PRAGMAs against a live SQLite connection
- Scheduling checkpoints, handling long readers, and shutdown checkpoints
- Measuring local filesystem reserve/alerts and mapping actual SQLite/OS errors
- Commit-boundary publication implementation, power-loss/disk-full simulation,
  daemon/CLI configuration, and DR-001 backup/restore wiring
- Issue-state changes
