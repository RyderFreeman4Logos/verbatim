//! Durability-specific `Store` construction, health, recovery, and regression tests.

use super::*;

/// ASCII fourcc `VBTM`, stored in SQLite's 32-bit application identifier.
pub(crate) const STORE_APPLICATION_ID: u32 = u32::from_be_bytes(*b"VBTM");
pub(crate) const STORE_USER_VERSION: u32 = 1;

fn validate_store_identity(conn: &Connection) -> Result<()> {
    let application_id: i64 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if application_id != 0 && application_id != i64::from(STORE_APPLICATION_ID) {
        bail!(
            "SQLite application_id {application_id} does not identify a Verbatim store (expected {})",
            STORE_APPLICATION_ID
        );
    }
    if user_version > i64::from(STORE_USER_VERSION) {
        bail!(
            "SQLite user_version {user_version} is newer than this Verbatim binary supports ({STORE_USER_VERSION})"
        );
    }
    Ok(())
}

fn stamp_store_identity(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "application_id", STORE_APPLICATION_ID)?;
    conn.pragma_update(None, "user_version", STORE_USER_VERSION)?;
    Ok(())
}

impl Store {
    /// Return the on-disk database path, if this store is not in-memory.
    #[allow(dead_code)]
    pub(crate) fn database_path(&self) -> Option<&Path> {
        self.database_path.as_deref()
    }

    /// Return the durability profile selected when this store was opened.
    #[allow(dead_code)]
    pub(crate) fn durability_profile(&self) -> SqliteDurabilityProfile {
        self.durability_profile
    }

    pub fn new(path: &Path) -> Result<Self> {
        Self::new_with_durability_profile(path, SqliteDurabilityProfile::default())
    }

    /// Open a local SQLite store with an explicit, inspectable durability contract.
    ///
    /// The store requires a local filesystem with SQLite WAL locking semantics;
    /// network and multi-host filesystems are intentionally rejected if SQLite
    /// cannot enable WAL.
    pub fn new_with_durability_profile(
        path: &Path,
        durability_profile: SqliteDurabilityProfile,
    ) -> Result<Self> {
        let conn = Connection::open(path)?;
        sqlite_durability::apply_profile_pragmas(
            &conn,
            durability_profile,
            true,
            SQLITE_MMAP_SIZE,
            SQLITE_CACHE_SIZE_KB,
        )?;
        validate_store_identity(&conn)?;
        let store = Self {
            conn,
            durability_profile,
            database_path: Some(path.to_path_buf()),
            sql_statement_counting_available: false,
            #[cfg(test)]
            source_relocation_before_mutation_hook: std::cell::RefCell::new(None),
            #[cfg(test)]
            source_relocation_before_parse_hook: std::cell::RefCell::new(None),
            #[cfg(test)]
            source_relocation_after_parse_hook: std::cell::RefCell::new(None),
        };
        store.ensure_write_capacity(SqliteWriteOperation::Migration)?;
        store
            .migrate()
            .map_err(|error| map_storage_error(SqliteWriteOperation::Migration, error))?;
        stamp_store_identity(&store.conn)
            .map_err(|error| map_storage_error(SqliteWriteOperation::Migration, error))?;
        // SQLite handles WAL recovery during open. Checking every writable
        // open makes the recovery policy fail closed instead of allowing a
        // corrupted database to become an active generation.
        store.verify_integrity_after_recovery()?;
        Ok(store)
    }

    pub fn open_existing_readonly(path: &Path) -> Result<Self> {
        Self::open_existing_readonly_with_durability_profile(
            path,
            SqliteDurabilityProfile::default(),
        )
    }

    pub fn open_existing_readonly_with_durability_profile(
        path: &Path,
        durability_profile: SqliteDurabilityProfile,
    ) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_millis(
            durability_profile.policy().busy_timeout_millis,
        ))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA query_only = ON;")?;
        validate_store_identity(&conn)?;
        Ok(Self {
            conn,
            durability_profile,
            database_path: Some(path.to_path_buf()),
            sql_statement_counting_available: true,
            #[cfg(test)]
            source_relocation_before_mutation_hook: std::cell::RefCell::new(None),
            #[cfg(test)]
            source_relocation_before_parse_hook: std::cell::RefCell::new(None),
            #[cfg(test)]
            source_relocation_after_parse_hook: std::cell::RefCell::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_query_only_for_test(&self, enabled: bool) -> Result<()> {
        let pragma = if enabled {
            "PRAGMA query_only = ON;"
        } else {
            "PRAGMA query_only = OFF;"
        };
        self.conn.execute_batch(pragma)?;
        Ok(())
    }

    /// Return the selected profile and the PRAGMAs SQLite reports at runtime.
    pub fn effective_durability(&self) -> Result<SqliteEffectiveDurability> {
        sqlite_durability::effective_durability(&self.conn, self.durability_profile)
    }

    /// Return effective PRAGMAs together with filesystem headroom, for health
    /// endpoints and operator diagnostics.
    pub fn durability_status(&self) -> Result<SqliteDurabilityStatus> {
        let disk = self
            .database_path
            .as_deref()
            .map(|path| sqlite_durability::disk_space_status(path, self.durability_profile))
            .transpose()?;
        Ok(SqliteDurabilityStatus {
            effective: self.effective_durability()?,
            wal_bytes: self
                .database_path
                .as_deref()
                .map(sqlite_durability::wal_size_bytes)
                .unwrap_or(0),
            disk,
        })
    }

    /// Fail before a write-heavy operation can consume the filesystem reserve.
    /// Callers must not publish a new generation after this returns an error.
    pub fn ensure_write_capacity(&self, operation: SqliteWriteOperation) -> Result<()> {
        if let Some(path) = self.database_path.as_deref() {
            sqlite_durability::ensure_disk_reserve(path, self.durability_profile, operation)?;
        }
        Ok(())
    }

    /// Run a profile-scheduled passive checkpoint and surface a typed alert when
    /// a long reader retains a WAL beyond the configured bound.
    pub fn checkpoint_wal(&self) -> Result<SqliteCheckpointStatus> {
        sqlite_durability::checkpoint(
            &self.conn,
            self.database_path.as_deref(),
            self.durability_profile,
            self.durability_profile.policy().checkpoint_mode,
        )
    }

    /// Compact WAL on graceful shutdown. A failed checkpoint is returned to the
    /// caller so it can be logged and the next open can run recovery checks.
    pub fn checkpoint_wal_on_shutdown(&self) -> Result<SqliteCheckpointStatus> {
        sqlite_durability::checkpoint(
            &self.conn,
            self.database_path.as_deref(),
            self.durability_profile,
            SqliteCheckpointMode::Truncate,
        )
    }

    /// Validate database and foreign-key integrity after SQLite has recovered
    /// a previous WAL on open or after an operator detects an abnormal stop.
    pub fn verify_integrity_after_recovery(&self) -> Result<()> {
        sqlite_durability::verify_integrity_after_recovery(&self.conn)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let durability_profile = SqliteDurabilityProfile::default();
        sqlite_durability::apply_profile_pragmas(
            &conn,
            durability_profile,
            false,
            SQLITE_MMAP_SIZE,
            SQLITE_CACHE_SIZE_KB,
        )?;
        validate_store_identity(&conn)?;
        let store = Self {
            conn,
            durability_profile,
            database_path: None,
            sql_statement_counting_available: false,
            #[cfg(test)]
            source_relocation_before_mutation_hook: std::cell::RefCell::new(None),
            #[cfg(test)]
            source_relocation_before_parse_hook: std::cell::RefCell::new(None),
            #[cfg(test)]
            source_relocation_after_parse_hook: std::cell::RefCell::new(None),
        };
        store.migrate()?;
        stamp_store_identity(&store.conn)?;
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn database_identity(path: &Path) -> (u32, u32) {
        let conn = Connection::open(path).unwrap();
        let application_id = conn
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        let user_version = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        (application_id, user_version)
    }

    #[test]
    fn store_identity_fresh_database_is_stamped_and_reopens() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fresh.db");

        drop(Store::new(&path).unwrap());
        assert_eq!(
            database_identity(&path),
            (STORE_APPLICATION_ID, STORE_USER_VERSION)
        );

        Store::open_existing_readonly(&path).unwrap();
        Store::new(&path).unwrap();
    }

    #[test]
    fn store_identity_wrong_application_id_fails_writable_and_readonly() {
        for readonly in [false, true] {
            let dir = tempdir().unwrap();
            let path = dir.path().join("wrong-product.db");
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "application_id", u32::from_be_bytes(*b"NOPE"))
                .unwrap();
            drop(conn);

            let result = if readonly {
                Store::open_existing_readonly(&path)
            } else {
                Store::new(&path)
            };
            let error = result
                .err()
                .expect("wrong-product database must fail closed");
            assert!(error.to_string().contains("application_id"));
        }
    }

    #[test]
    fn store_identity_newer_user_version_fails_writable_and_readonly() {
        for readonly in [false, true] {
            let dir = tempdir().unwrap();
            let path = dir.path().join("newer.db");
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", STORE_USER_VERSION + 1)
                .unwrap();
            drop(conn);

            let result = if readonly {
                Store::open_existing_readonly(&path)
            } else {
                Store::new(&path)
            };
            let error = result.err().expect("newer database must fail closed");
            assert!(error.to_string().contains("user_version"));
        }
    }

    #[test]
    fn store_identity_legacy_unstamped_database_migrates_and_is_stamped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                request_json TEXT NOT NULL,
                result_json TEXT,
                error TEXT
            );",
        )
        .unwrap();
        drop(conn);

        let store = Store::new(&path).unwrap();
        assert!(table_has_column(store.connection(), "tasks", "progress_json").unwrap());
        drop(store);
        assert_eq!(
            database_identity(&path),
            (STORE_APPLICATION_ID, STORE_USER_VERSION)
        );
    }

    #[test]
    fn durability_profiles_apply_and_report_effective_pragmas() {
        let dir = tempdir().unwrap();
        let cases = [
            (SqliteDurabilityProfile::Durable, "full", 100, "RPO=0"),
            (
                SqliteDurabilityProfile::Balanced,
                "normal",
                1_000,
                "no bounded",
            ),
            (
                SqliteDurabilityProfile::Ephemeral,
                "off",
                10_000,
                "no durability RPO",
            ),
        ];

        for (profile, synchronous, wal_autocheckpoint_pages, rpo_fragment) in cases {
            let path = dir.path().join(format!("{profile:?}.db"));
            let store = Store::new_with_durability_profile(&path, profile).unwrap();
            let effective = store.effective_durability().unwrap();

            assert_eq!(effective.profile, profile);
            assert_eq!(effective.journal_mode, "wal");
            assert_eq!(effective.synchronous, synchronous);
            assert_eq!(effective.wal_autocheckpoint_pages, wal_autocheckpoint_pages);
            assert!(effective.busy_timeout_millis > 0);
            assert!(effective.rpo.contains(rpo_fragment));
        }
    }

    #[test]
    fn disk_reserve_failures_are_typed_for_every_write_heavy_operation() {
        let profile = SqliteDurabilityProfile::Durable;
        let reserve = profile.policy().disk_reserve_bytes;
        for operation in [
            SqliteWriteOperation::Ingest,
            SqliteWriteOperation::Migration,
            SqliteWriteOperation::Backup,
            SqliteWriteOperation::IndexBuild,
            SqliteWriteOperation::WalCheckpoint,
        ] {
            let error = sqlite_durability::ensure_disk_reserve_for_available_bytes(
                profile,
                operation,
                reserve - 1,
            )
            .unwrap_err();
            let typed = error.downcast_ref::<SqliteDurabilityError>().unwrap();
            assert!(matches!(
                typed,
                SqliteDurabilityError::DiskReserve {
                    operation: actual,
                    available_bytes,
                    reserve_bytes,
                } if *actual == operation && *available_bytes == reserve - 1 && *reserve_bytes == reserve
            ));
        }
    }

    #[test]
    fn sqlite_full_and_enospc_are_typed_disk_full_failures() {
        let sqlite_full = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        );
        let sqlite_error = map_storage_error(SqliteWriteOperation::Ingest, sqlite_full.into());
        assert!(matches!(
            sqlite_error.downcast_ref::<SqliteDurabilityError>(),
            Some(SqliteDurabilityError::DiskFull {
                operation: SqliteWriteOperation::Ingest
            })
        ));

        let enospc = std::io::Error::from_raw_os_error(libc::ENOSPC);
        let io_error = map_storage_error(SqliteWriteOperation::IndexBuild, enospc.into());
        assert!(matches!(
            io_error.downcast_ref::<SqliteDurabilityError>(),
            Some(SqliteDurabilityError::DiskFull {
                operation: SqliteWriteOperation::IndexBuild
            })
        ));
    }

    #[test]
    fn sqlite_busy_and_locked_are_detected_by_typed_error_code() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            assert!(is_sqlite_busy_error(&error.into()));
        }

        let full = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        );
        assert!(!is_sqlite_busy_error(&full.into()));
    }

    #[test]
    fn passive_checkpoint_reports_long_reader_without_unbounded_wal_growth() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("checkpoint.db");
        let store =
            Store::new_with_durability_profile(&path, SqliteDurabilityProfile::Durable).unwrap();
        store
            .conn
            .execute_batch("CREATE TABLE checkpoint_test (value INTEGER NOT NULL);")
            .unwrap();

        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN DEFERRED;").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM checkpoint_test", [], |row| row.get(0))
            .unwrap();
        store
            .conn
            .execute("INSERT INTO checkpoint_test (value) VALUES (1)", [])
            .unwrap();

        let checkpoint = store.checkpoint_wal().unwrap();
        assert!(checkpoint.blocked);
        assert!(checkpoint.log_frames > checkpoint.checkpointed_frames);

        reader.execute_batch("ROLLBACK;").unwrap();
        let shutdown = store.checkpoint_wal_on_shutdown().unwrap();
        assert_eq!(shutdown.mode, SqliteCheckpointMode::Truncate);
    }

    #[test]
    fn recovery_integrity_check_accepts_a_clean_database() {
        let store = Store::in_memory().unwrap();
        store.verify_integrity_after_recovery().unwrap();
    }

    #[test]
    fn reopen_recovers_committed_wal_rows_retained_by_a_reader() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recovery.db");
        let store =
            Store::new_with_durability_profile(&path, SqliteDurabilityProfile::Durable).unwrap();
        store
            .conn
            .execute_batch("CREATE TABLE recovery_test (value INTEGER NOT NULL);")
            .unwrap();

        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN DEFERRED;").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM recovery_test", [], |row| row.get(0))
            .unwrap();
        store
            .conn
            .execute("INSERT INTO recovery_test (value) VALUES (7)", [])
            .unwrap();
        drop(store);

        let reopened =
            Store::new_with_durability_profile(&path, SqliteDurabilityProfile::Durable).unwrap();
        let rows: i64 = reopened
            .conn
            .query_row("SELECT COUNT(*) FROM recovery_test", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "committed WAL row must survive a writer restart");
        reopened.verify_integrity_after_recovery().unwrap();
        reader.execute_batch("ROLLBACK;").unwrap();
    }
}
