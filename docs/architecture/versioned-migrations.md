# Versioned migrations framework (MIGRATE-001)

Status: walking skeleton for
[#335](https://github.com/RyderFreeman4Logos/verbatim/issues/335).
Code: `crates/verbatim-core/src/migration_framework.rs`.

## Problem

SQLite schema, configuration, index metadata, and persisted artifacts
(QueryPlan, EvidencePack, ContextPack, GraphReport, WorkflowRun, cursors, task
profiles, publication manifests) evolve independently of binary releases.
Without one migration framework, components silently reinterpret evidence,
cursors, ContextPacks, graph reports, or task state; upgrades are non-atomic;
newer stores are opened as if compatible; and interrupted upgrades leave
irrecoverable half-states.

## Contract summary

| Type | Role |
| --- | --- |
| `SchemaVersion` | Semantic `major.minor.patch` application/schema version |
| `CompatibilityWindow` | Inclusive rolling-upgrade window `[min_supported, max_supported]` |
| `VersionRelation` | Compatible / TooOld / TooNew classification |
| `AccessMode` | `ReadWrite` or explicit `ReadOnlyAdapter` |
| `MigrationStep` | Ordered, transactional, idempotent step with checksum |
| `MigrationHistory` / `MigrationHistoryEntry` | Applied history with checksums and status |
| `DiskSpaceRequirement` | Preflight free-space fence |
| `BackupHook` | Pre-destructive backup contract |
| `PreflightReport` | Window + disk + backup outcome before mutation |
| `VersionDecision` | UpToDate / NeedsUpgrade / NewerThanSupported / OlderThanSupported |
| `ArtifactKind` / `ArtifactVersion` | Version stamps for persisted artifacts |
| `MigrationFramework` | Registry, planner, preflight, pure apply/recover |
| `MIGRATION_FRAMEWORK_SCHEMA_VERSION` | Document schema; unknown versions fail closed |

### Design principles

1. **One framework** for schema, config, artifacts, and index metadata contracts.
2. **Transactional + idempotent** steps with registry checksums mirrored in history.
3. **Fail closed** on newer-than-supported stores unless an explicit read-only
   adapter is requested.
4. **Preflight** validates compatibility window, disk space, history integrity,
   and optional backup hooks before any apply.
5. **Immutable historical artifacts** are adapted or copied at read time, never
   semantically rewritten in place.
6. **No arbitrary downgrade** promise; downgrade is rejected in preflight.
7. **Interrupted apply** records `Interrupted`/`Failed` at the history tail;
   recovery drops the unrecovered tail so the prior completed version is
   restorable, then re-apply.

### Version detection

`MigrationFramework::detect_version(current, allow_read_only_adapter)`:

- Inside window and equal to `max_supported` → `UpToDate` + `ReadWrite`
- Inside window and below target → `NeedsUpgrade` with ordered step ids
- Below `min_supported` → `OlderThanSupported` (offline upgrade required)
- Above `max_supported` without adapter flag → hard error
- Above `max_supported` with adapter flag → `NewerThanSupported` + `ReadOnlyAdapter`

### Preflight

`preflight` runs pure validation first and only then the optional backup hook,
so a failed check never leaves an orphan backup:

1. compatibility window (current + target) and downgrade fence
2. history / document schema integrity (checksums, unrecovered interruption)
3. disk space
4. upgrade path completeness (`plan_upgrade` when `current != target`)
5. `BackupHook::create_backup` (side-effecting; last)

`preflight` refuses:

- unknown framework document schema versions
- history checksum mismatch vs registry
- unrecovered interruption at history tail
- current/target outside window
- downgrade (`target < current`)
- insufficient disk space
- incomplete upgrade path
- backup hook failure

### Artifact versions

`ArtifactVersion` stamps `kind`, `SchemaVersion`, content `checksum`, and
`immutable`. Newer artifacts require the same explicit read-only adapter escape
hatch; immutable packs are never in-place mutated by migrations.

## What this slice wires

- Module export from `verbatim-core` (`pub mod migration_framework`)
- Typed schema versions, compatibility window, migration steps/history
- Pure in-memory planner/apply/recover loop for contract tests
- Preflight disk + backup hook + window checks
- Artifact version classification with read-only adapter
- Live SQLite open paths validate `application_id` / `user_version`, fail
  closed on wrong-product and newer stores, and stamp successful writable
  migrations with the `VBTM` application id and user version `1`
- Read-only opens do not migrate or stamp; legacy unstamped stores remain
  readable until historical production databases can be proven fully stamped
- Unit tests: idempotent re-apply, forward-version fail-closed, checksum
  mismatch, disk space, compatibility window, read-only escape hatch,
  interrupted migration recovery, serde/unknown-schema rejection,
  incomplete-path preflight without backup side effects, register
  id/sequence collision, `MigrationHistory::default` schema version

## What this slice does **not** do (residual)

- Replace the existing ad-hoc SQLite `migrate()` upgrades with ordered,
  transactional `MigrationFramework` SQL steps and persisted history
- Auto-migrate on-disk configs beyond the contract types
- Golden databases/configs/artifacts from every historical release
- Adjacent service rolling-upgrade matrix in CI
- Post-migration retrieval/citation/cache/graph invariant suites
- Closing epic #335

## Integration notes

When a later slice owns a store open path, construct `MigrationFramework` with
the product `application_id` and supported `CompatibilityWindow`, load
`MigrationHistory`, run `detect_version`, then `preflight` (with a real
`BackupHook` and measured `DiskSpaceRequirement`) before `apply_pending`.
Persist history checksums next to the schema version. Prefer adapters in
non-capped modules — do not grow `store.rs`, `main.rs`, or `client.rs` solely
to adopt this contract. Immutable artifacts (ContextPack, EvidencePack, run
reports) must be adapted or copied, never rewritten under a new schema in place.
