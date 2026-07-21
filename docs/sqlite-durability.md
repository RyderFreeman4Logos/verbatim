# SQLite durability and recovery

Verbatim's local metadata and vector store is a single SQLite database. It is
opened in WAL mode and must live on a local filesystem that supports SQLite file
locking. Do not put one database on an NFS/SMB share or open it concurrently
from multiple hosts.

## Selecting a profile

The setting is intentionally part of the persisted-store configuration and is
reported by `verbatim daemon status --details` and `GET /health`:

```toml
[store]
# durable | balanced | ephemeral
# balanced is the default.
durability = "balanced"
```

| Profile | SQLite synchronous mode | WAL auto-checkpoint | Disk reserve | RPO contract |
| --- | --- | ---: | ---: | --- |
| `durable` | `FULL` | 100 pages | 512 MiB | **RPO=0** for acknowledged SQLite commits across process or OS crashes. The host's storage hardware and filesystem must actually provide durable flushes. |
| `balanced` | `NORMAL` | 1,000 pages | 256 MiB | An acknowledged transaction can be lost after power failure. No bounded time-based RPO is promised. |
| `ephemeral` | `OFF` | 10,000 pages | 128 MiB | Acknowledged writes can be lost after process, OS, or host failure. No durability RPO is provided. |

All profiles use WAL and a 30-second SQLite busy timeout except `ephemeral`,
which uses a five-second timeout. Profile changes require a daemon restart;
the daemon health response shows the selected profile **and the effective
SQLite PRAGMAs**, so operators can detect an unsupported filesystem or an
unexpected configuration.

Verbatim does **not** promise a fixed RTO. A clean restart is bounded by
SQLite WAL recovery plus integrity checks; a damaged or lost database requires
restoring a backup and rebuilding derived lexical/vector artifacts as needed.
Measure that restore procedure against your own data size and recovery target.

## Checkpoint and recovery policy

SQLite's auto-checkpoint is the primary write-path policy. Verbatim also runs a
non-blocking `PASSIVE` checkpoint after a successful ingest or vector-index
build and a `TRUNCATE` checkpoint on graceful daemon shutdown. A PASSIVE
checkpoint never cancels readers. If a long-lived reader prevents frame
reclamation, the checkpoint reports its `busy`, log-frame, and checkpointed-
frame state; it becomes a hard error only after the WAL reaches the profile's
alert size (128/256/512 MiB for durable/balanced/ephemeral).

On every writable open, Verbatim verifies `integrity_check` and
`foreign_key_check` after migration. Recovery failures are fatal: do not
continue serving or ingesting against a database that has failed those checks.

## Disk-full behavior

Before migration, ingest, vector-index construction, and WAL checkpointing,
Verbatim checks free space on the database filesystem against the selected
reserve. An insufficient reserve rejects the operation before SQLite begins
large writes; daemon ingest/index requests return HTTP 507 and record the
failure instead of reporting a completed task. `SQLITE_FULL` and filesystem
`ENOSPC` errors are converted to typed disk-full errors at the task boundary.
Backup tooling must perform the same reserve check before making a local copy.

The source-ingest commit path remains transactional: an unsuccessful source
replacement does not publish a partial generation or claim a successful task.
Treat any disk-reserve/disk-full error as an alert, free capacity, then retry
the failed operation. The `/health` durability object and detailed daemon
status expose current available bytes, reserve, and below-reserve state for
monitoring.

## Backups and restore drills

Take SQLite-consistent backups. Prefer SQLite's backup API or perform a
checkpoint first and copy the database **with its `-wal` and `-shm` companions
when they exist**. A file-only copy of `verbatim.db` while WAL is active is not
a reliable backup. Backups are the only protection against media loss, operator
error, and the no-RPO profiles.

At least once per operational release, restore a backup into a separate data
directory, start Verbatim against it, and verify:

1. startup recovery/integrity checks pass;
2. `verbatim daemon status --details` reports the intended effective profile;
3. a representative retrieve request returns expected evidence;
4. the original database remains untouched.

Keep enough free disk capacity for the configured reserve **in addition to**
the largest expected WAL and backup copy.
