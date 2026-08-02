use std::ffi::CString;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, Error as SqliteError};
use serde::{Deserialize, Serialize};

/// The durability contract selected for the local SQLite store.
///
/// All profiles require a local filesystem that supports SQLite locking and
/// WAL. Network filesystems and multi-host access are deliberately unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SqliteDurabilityProfile {
    /// Acknowledged SQLite commits use `synchronous=FULL`.
    Durable,
    /// The default: WAL throughput with SQLite's `synchronous=NORMAL` trade-off.
    #[default]
    Balanced,
    /// Local scratch data only; acknowledged writes can be lost on a host failure.
    Ephemeral,
}

impl fmt::Display for SqliteDurabilityProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Durable => "durable",
            Self::Balanced => "balanced",
            Self::Ephemeral => "ephemeral",
        })
    }
}

/// The explicit checkpoint invocation policy. SQLite's per-connection
/// autocheckpoint remains the normal scheduling mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqliteCheckpointMode {
    Passive,
    Truncate,
}

impl SqliteCheckpointMode {
    fn pragma(self) -> &'static str {
        match self {
            Self::Passive => "PASSIVE",
            Self::Truncate => "TRUNCATE",
        }
    }
}

/// Read-only policy values that make a profile's persistence semantics explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteDurabilityPolicy {
    pub profile: SqliteDurabilityProfile,
    pub synchronous: &'static str,
    pub busy_timeout_millis: u64,
    pub wal_autocheckpoint_pages: u64,
    pub checkpoint_mode: SqliteCheckpointMode,
    pub wal_alert_bytes: u64,
    pub disk_reserve_bytes: u64,
    pub rpo: &'static str,
}

impl SqliteDurabilityProfile {
    pub const fn policy(self) -> SqliteDurabilityPolicy {
        const MIB: u64 = 1024 * 1024;
        match self {
            Self::Durable => SqliteDurabilityPolicy {
                profile: Self::Durable,
                synchronous: "FULL",
                busy_timeout_millis: 30_000,
                wal_autocheckpoint_pages: 100,
                checkpoint_mode: SqliteCheckpointMode::Passive,
                wal_alert_bytes: 128 * MIB,
                disk_reserve_bytes: 512 * MIB,
                rpo: "RPO=0 for acknowledged SQLite commits across process or OS crashes; hardware and filesystem durability remain dependencies of the local host.",
            },
            Self::Balanced => SqliteDurabilityPolicy {
                profile: Self::Balanced,
                synchronous: "NORMAL",
                busy_timeout_millis: 30_000,
                wal_autocheckpoint_pages: 1_000,
                checkpoint_mode: SqliteCheckpointMode::Passive,
                wal_alert_bytes: 256 * MIB,
                disk_reserve_bytes: 256 * MIB,
                rpo: "An acknowledged transaction can be lost after a power failure; this profile has no bounded time-based RPO.",
            },
            Self::Ephemeral => SqliteDurabilityPolicy {
                profile: Self::Ephemeral,
                synchronous: "OFF",
                busy_timeout_millis: 5_000,
                wal_autocheckpoint_pages: 10_000,
                checkpoint_mode: SqliteCheckpointMode::Passive,
                wal_alert_bytes: 512 * MIB,
                disk_reserve_bytes: 128 * MIB,
                rpo: "Acknowledged writes can be lost after process, OS, or host failure; no durability RPO is provided.",
            },
        }
    }
}

/// The values SQLite actually reports after a profile has been applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteEffectiveDurability {
    pub profile: SqliteDurabilityProfile,
    pub journal_mode: String,
    pub synchronous: String,
    pub busy_timeout_millis: u64,
    pub wal_autocheckpoint_pages: u64,
    pub checkpoint_mode: SqliteCheckpointMode,
    pub wal_alert_bytes: u64,
    pub disk_reserve_bytes: u64,
    pub rpo: String,
}

/// Free-space state for the filesystem holding the SQLite database and WAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteDiskSpaceStatus {
    pub available_bytes: u64,
    pub reserve_bytes: u64,
    pub below_reserve: bool,
}

/// The observable durability state exposed to daemon health and callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteDurabilityStatus {
    pub effective: SqliteEffectiveDurability,
    pub wal_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<SqliteDiskSpaceStatus>,
}

/// A checkpoint result. `blocked` identifies a reader that is retaining WAL
/// frames even when SQLite did not return an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteCheckpointStatus {
    pub mode: SqliteCheckpointMode,
    pub busy: bool,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
    pub wal_bytes: u64,
    pub blocked: bool,
}

/// Write paths that must reserve room before SQLite starts a large operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteWriteOperation {
    Ingest,
    Migration,
    Backup,
    IndexBuild,
    WalCheckpoint,
}

impl SqliteWriteOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Migration => "migration",
            Self::Backup => "backup",
            Self::IndexBuild => "index build",
            Self::WalCheckpoint => "WAL checkpoint",
        }
    }
}

impl fmt::Display for SqliteWriteOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Typed storage failures callers can use to fail closed without publishing a
/// partial generation or claiming a successful operation.
#[derive(Debug)]
pub enum SqliteDurabilityError {
    DiskReserve {
        operation: SqliteWriteOperation,
        available_bytes: u64,
        reserve_bytes: u64,
    },
    DiskFull {
        operation: SqliteWriteOperation,
    },
    WalCheckpointBlocked {
        log_frames: u64,
        checkpointed_frames: u64,
        wal_bytes: u64,
        alert_bytes: u64,
    },
    IntegrityCheck {
        detail: String,
    },
    JournalModeUnavailable {
        effective: String,
    },
}

impl fmt::Display for SqliteDurabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiskReserve {
                operation,
                available_bytes,
                reserve_bytes,
            } => write!(
                formatter,
                "refusing {operation} before SQLite runs: {available_bytes} bytes free is below the {reserve_bytes}-byte disk reserve"
            ),
            Self::DiskFull { operation } => {
                write!(formatter, "SQLite reported disk full during {operation}")
            }
            Self::WalCheckpointBlocked {
                log_frames,
                checkpointed_frames,
                wal_bytes,
                alert_bytes,
            } => write!(
                formatter,
                "WAL checkpoint is blocked by a long-lived reader ({checkpointed_frames}/{log_frames} frames checkpointed, WAL {wal_bytes} bytes >= alert {alert_bytes} bytes)"
            ),
            Self::IntegrityCheck { detail } => write!(formatter, "SQLite recovery integrity check failed: {detail}"),
            Self::JournalModeUnavailable { effective } => write!(
                formatter,
                "SQLite WAL is required for this store, but journal_mode is {effective}"
            ),
        }
    }
}

impl std::error::Error for SqliteDurabilityError {}

pub(crate) fn apply_profile_pragmas(
    conn: &Connection,
    profile: SqliteDurabilityProfile,
    require_wal: bool,
    mmap_size: i64,
    cache_size_kb: i64,
) -> Result<()> {
    let policy = profile.policy();
    let journal_mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if require_wal && !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(SqliteDurabilityError::JournalModeUnavailable {
            effective: journal_mode,
        }
        .into());
    }

    let synchronous = if journal_mode.eq_ignore_ascii_case("wal") {
        policy.synchronous
    } else {
        // SQLite in-memory databases cannot use WAL. Preserve the prior
        // fail-safe behavior for tests and transient scratch connections.
        "FULL"
    };
    conn.execute_batch(&format!(
        "PRAGMA synchronous = {synchronous};\
         PRAGMA mmap_size = {mmap_size};\
         PRAGMA cache_size = {cache_size_kb};\
         PRAGMA wal_autocheckpoint = {};",
        policy.wal_autocheckpoint_pages
    ))?;
    conn.busy_timeout(std::time::Duration::from_millis(policy.busy_timeout_millis))?;
    Ok(())
}

pub(crate) fn effective_durability(
    conn: &Connection,
    profile: SqliteDurabilityProfile,
) -> Result<SqliteEffectiveDurability> {
    let policy = profile.policy();
    let journal_mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let synchronous: i64 = conn.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    let busy_timeout_millis: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
    let wal_autocheckpoint_pages: i64 =
        conn.query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))?;

    Ok(SqliteEffectiveDurability {
        profile,
        journal_mode: journal_mode.to_ascii_lowercase(),
        synchronous: synchronous_name(synchronous).to_string(),
        busy_timeout_millis: u64::try_from(busy_timeout_millis).unwrap_or(0),
        wal_autocheckpoint_pages: u64::try_from(wal_autocheckpoint_pages).unwrap_or(0),
        checkpoint_mode: policy.checkpoint_mode,
        wal_alert_bytes: policy.wal_alert_bytes,
        disk_reserve_bytes: policy.disk_reserve_bytes,
        rpo: policy.rpo.to_string(),
    })
}

pub(crate) fn disk_space_status(
    path: &Path,
    profile: SqliteDurabilityProfile,
) -> Result<SqliteDiskSpaceStatus> {
    let available_bytes = available_bytes(path)?;
    Ok(disk_space_status_for_available_bytes(
        profile,
        available_bytes,
    ))
}

pub(crate) fn ensure_disk_reserve(
    path: &Path,
    profile: SqliteDurabilityProfile,
    operation: SqliteWriteOperation,
) -> Result<SqliteDiskSpaceStatus> {
    let status = disk_space_status(path, profile)?;
    ensure_disk_reserve_status(status, operation)?;
    Ok(status)
}

#[cfg(test)]
pub(crate) fn ensure_disk_reserve_for_available_bytes(
    profile: SqliteDurabilityProfile,
    operation: SqliteWriteOperation,
    available_bytes: u64,
) -> Result<SqliteDiskSpaceStatus> {
    let status = disk_space_status_for_available_bytes(profile, available_bytes);
    ensure_disk_reserve_status(status, operation)?;
    Ok(status)
}

pub(crate) fn checkpoint(
    conn: &Connection,
    database_path: Option<&Path>,
    profile: SqliteDurabilityProfile,
    mode: SqliteCheckpointMode,
) -> Result<SqliteCheckpointStatus> {
    if let Some(path) = database_path {
        ensure_disk_reserve(path, profile, SqliteWriteOperation::WalCheckpoint)?;
    }

    let pragma = format!("PRAGMA wal_checkpoint({})", mode.pragma());
    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = conn
        .query_row(&pragma, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|error| map_sqlite_error(SqliteWriteOperation::WalCheckpoint, error))?;
    let wal_bytes = database_path.map(wal_size_bytes).unwrap_or(0);
    let status = SqliteCheckpointStatus {
        mode,
        busy: busy != 0,
        log_frames: u64::try_from(log_frames).unwrap_or(0),
        checkpointed_frames: u64::try_from(checkpointed_frames).unwrap_or(0),
        wal_bytes,
        blocked: busy != 0 || log_frames > checkpointed_frames,
    };
    let alert_bytes = profile.policy().wal_alert_bytes;
    if status.blocked && status.wal_bytes >= alert_bytes {
        return Err(SqliteDurabilityError::WalCheckpointBlocked {
            log_frames: status.log_frames,
            checkpointed_frames: status.checkpointed_frames,
            wal_bytes: status.wal_bytes,
            alert_bytes,
        }
        .into());
    }
    Ok(status)
}

pub(crate) fn verify_integrity_after_recovery(conn: &Connection) -> Result<()> {
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if !integrity.eq_ignore_ascii_case("ok") {
        return Err(SqliteDurabilityError::IntegrityCheck { detail: integrity }.into());
    }

    let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    if let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let rowid: Option<i64> = row.get(1)?;
        return Err(SqliteDurabilityError::IntegrityCheck {
            detail: format!(
                "foreign key violation in {table} at row {}",
                rowid.unwrap_or(-1)
            ),
        }
        .into());
    }
    Ok(())
}

fn disk_space_status_for_available_bytes(
    profile: SqliteDurabilityProfile,
    available_bytes: u64,
) -> SqliteDiskSpaceStatus {
    let reserve_bytes = profile.policy().disk_reserve_bytes;
    SqliteDiskSpaceStatus {
        available_bytes,
        reserve_bytes,
        below_reserve: available_bytes < reserve_bytes,
    }
}

fn ensure_disk_reserve_status(
    status: SqliteDiskSpaceStatus,
    operation: SqliteWriteOperation,
) -> Result<()> {
    if status.below_reserve {
        return Err(SqliteDurabilityError::DiskReserve {
            operation,
            available_bytes: status.available_bytes,
            reserve_bytes: status.reserve_bytes,
        }
        .into());
    }
    Ok(())
}

fn available_bytes(path: &Path) -> Result<u64> {
    let directory = path.parent().unwrap_or(path);
    let c_path = CString::new(directory.as_os_str().as_bytes())
        .context("database path contains an interior NUL byte")?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and `stat` points to writable memory.
    let result = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("inspect free space for {}", directory.display()));
    }
    // SAFETY: a zero return from `statvfs` initializes its output structure.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.f_bavail).saturating_mul(stat.f_frsize))
}

pub(crate) fn wal_size_bytes(database_path: &Path) -> u64 {
    let mut wal_path = database_path.as_os_str().to_os_string();
    wal_path.push("-wal");
    std::fs::metadata(PathBuf::from(wal_path))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn synchronous_name(value: i64) -> &'static str {
    match value {
        0 => "off",
        1 => "normal",
        2 => "full",
        3 => "extra",
        _ => "unknown",
    }
}

fn map_sqlite_error(operation: SqliteWriteOperation, error: SqliteError) -> anyhow::Error {
    map_storage_error(operation, error.into())
}

/// Return whether an error chain contains a typed durability or raw SQLite failure.
pub fn is_sqlite_storage_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<SqliteDurabilityError>().is_some()
            || cause.downcast_ref::<SqliteError>().is_some()
    })
}

/// Return whether an error chain contains a typed SQLite busy or locked failure.
pub fn is_sqlite_busy_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<SqliteError>()
            .and_then(SqliteError::sqlite_error)
            .is_some_and(|sqlite| {
                matches!(
                    sqlite.code,
                    rusqlite::ffi::ErrorCode::DatabaseBusy
                        | rusqlite::ffi::ErrorCode::DatabaseLocked
                )
            })
    })
}

/// Translate SQLite `SQLITE_FULL` and filesystem `ENOSPC` errors found in an
/// error chain into the stable disk-full failure used by task/API boundaries.
pub fn map_storage_error(operation: SqliteWriteOperation, error: anyhow::Error) -> anyhow::Error {
    let disk_full = error.chain().any(|cause| {
        cause
            .downcast_ref::<SqliteError>()
            .and_then(SqliteError::sqlite_error)
            .is_some_and(|sqlite| sqlite.extended_code == rusqlite::ffi::SQLITE_FULL)
            || cause
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error)
                == Some(libc::ENOSPC)
    });
    if disk_full {
        SqliteDurabilityError::DiskFull { operation }.into()
    } else {
        error
    }
}
