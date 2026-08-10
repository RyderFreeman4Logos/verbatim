//! Request-local SQLite and operating-system counters for retrieval telemetry.

use std::cell::Cell;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;

use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::Connection;

use super::Store;
use crate::retrieval_telemetry::RetrievalResourceCounters;

#[derive(Clone, Copy)]
struct SqlStatementCountState {
    count: u64,
    valid: bool,
}

thread_local! {
    static SQLITE_STATEMENT_COUNT: Cell<Option<SqlStatementCountState>> = const { Cell::new(None) };
}

fn record_sql_statement(event: TraceEvent<'_>) {
    if !matches!(event, TraceEvent::Stmt(..)) {
        return;
    }
    SQLITE_STATEMENT_COUNT.with(|slot| {
        let Some(mut state) = slot.get() else {
            return;
        };
        match state.count.checked_add(1) {
            Some(count) => state.count = count,
            None => state.valid = false,
        }
        slot.set(Some(state));
    });
}

struct SqlStatementCountGuard<'connection> {
    connection: &'connection Connection,
    root: bool,
    callback_active: bool,
}

impl<'connection> SqlStatementCountGuard<'connection> {
    fn start(connection: &'connection Connection) -> Self {
        let root = SQLITE_STATEMENT_COUNT.with(|slot| {
            if let Some(mut state) = slot.get() {
                state.valid = false;
                slot.set(Some(state));
                false
            } else {
                slot.set(Some(SqlStatementCountState {
                    count: 0,
                    valid: true,
                }));
                true
            }
        });
        connection.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT,
            Some(record_sql_statement),
        );
        Self {
            connection,
            root,
            callback_active: true,
        }
    }

    fn finish(mut self) -> Option<u64> {
        self.disable_callback();
        if !self.root {
            return None;
        }
        SQLITE_STATEMENT_COUNT.with(|slot| {
            slot.take()
                .and_then(|state| state.valid.then_some(state.count))
        })
    }

    fn disable_callback(&mut self) {
        if self.callback_active {
            self.connection
                .trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, None);
            self.callback_active = false;
        }
    }
}

impl Drop for SqlStatementCountGuard<'_> {
    fn drop(&mut self) {
        self.disable_callback();
        if self.root {
            SQLITE_STATEMENT_COUNT.with(|slot| slot.set(None));
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct ThreadResourceSnapshot {
    major_page_faults: u64,
    minor_page_faults: u64,
    block_input_operations: u64,
    storage_read_bytes: Option<u64>,
}

#[cfg(target_os = "linux")]
impl ThreadResourceSnapshot {
    fn capture() -> Option<Self> {
        let mut usage = MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: `getrusage` initializes the pointed-to `rusage` on a zero return code.
        if unsafe { libc::getrusage(libc::RUSAGE_THREAD, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful call above initialized every field in `usage`.
        let usage = unsafe { usage.assume_init() };
        Some(Self {
            major_page_faults: u64::try_from(usage.ru_majflt).ok()?,
            minor_page_faults: u64::try_from(usage.ru_minflt).ok()?,
            block_input_operations: u64::try_from(usage.ru_inblock).ok()?,
            storage_read_bytes: thread_storage_read_bytes(),
        })
    }

    fn delta(self, end: Self) -> RetrievalResourceCounters {
        RetrievalResourceCounters::observed(
            end.major_page_faults.checked_sub(self.major_page_faults),
            end.minor_page_faults.checked_sub(self.minor_page_faults),
            end.block_input_operations
                .checked_sub(self.block_input_operations),
            self.storage_read_bytes
                .zip(end.storage_read_bytes)
                .and_then(|(start, end)| end.checked_sub(start)),
        )
    }
}

#[cfg(target_os = "linux")]
fn thread_storage_read_bytes() -> Option<u64> {
    let mut contents = String::new();
    let mut reader = File::open("/proc/thread-self/io").ok()?.take(4_096);
    reader.read_to_string(&mut contents).ok()?;
    contents.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == "read_bytes")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

#[cfg(not(target_os = "linux"))]
struct ThreadResourceSnapshot;

#[cfg(not(target_os = "linux"))]
impl ThreadResourceSnapshot {
    fn capture() -> Option<Self> {
        None
    }

    fn delta(self, _end: Self) -> RetrievalResourceCounters {
        RetrievalResourceCounters::default()
    }
}

impl Store {
    /// Run one request-local operation and return its actual SQLite statement count.
    ///
    /// `None` means tracing was unavailable, the counter overflowed, or a
    /// same-thread counting window overlapped this one.
    pub fn count_sql_statements<T>(&self, operation: impl FnOnce() -> T) -> (T, Option<u64>) {
        if !self.sql_statement_counting_available {
            return (operation(), None);
        }
        let guard = SqlStatementCountGuard::start(&self.conn);
        let output = operation();
        (output, guard.finish())
    }

    /// Measure one live retrieval window using the existing Store boundary.
    ///
    /// Resource attribution is enabled only for the same read-only file-backed
    /// Stores that support request-local statement counting. Sampling is constant
    /// work: two thread `getrusage` calls and two procfs reads capped at 4 KiB,
    /// never one syscall per SQL row or retrieval candidate.
    pub fn measure_retrieval<T>(
        &self,
        operation: impl FnOnce() -> T,
    ) -> (T, Option<u64>, Option<RetrievalResourceCounters>) {
        let start = self
            .sql_statement_counting_available
            .then(ThreadResourceSnapshot::capture)
            .flatten();
        let (output, sql_statements) = self.count_sql_statements(operation);
        let resources = start
            .and_then(|start| ThreadResourceSnapshot::capture().map(|end| start.delta(end)))
            .filter(RetrievalResourceCounters::is_available);
        (output, sql_statements, resources)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::types::ChunkId;
    use tempfile::{tempdir, TempDir};

    fn readonly_store_fixture() -> (TempDir, PathBuf, ChunkId) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("statement-count.db");
        let store = Store::new(&db_path).unwrap();
        let source = crate::store::tests::sample_source();
        let evidence = crate::store::tests::sample_evidence(&source.id.0);
        let chunks = crate::store::tests::sample_chunks(&source.id.0);
        store.add_source(&source).unwrap();
        store.bulk_insert_evidence(&evidence).unwrap();
        store.bulk_insert_chunks(&chunks).unwrap();
        store
            .link_chunk_evidence(&[(chunks[1].id.clone(), evidence[0].id.clone())])
            .unwrap();
        drop(store);
        (dir, db_path, chunks[1].id.clone())
    }

    #[test]
    fn file_backed_readonly_store_reports_exact_zero_one_two() {
        let (_dir, db_path, existing_id) = readonly_store_fixture();
        let store = Store::open_existing_readonly(&db_path).unwrap();

        assert_eq!(store.count_sql_statements(|| ()).1, Some(0));
        assert_eq!(
            store
                .count_sql_statements(|| store.get_chunk(&ChunkId("missing".into())).unwrap())
                .1,
            Some(1)
        );

        let (_, existing_count) = store.count_sql_statements(|| {
            assert!(store.get_chunk(&existing_id).unwrap().is_some());
        });
        assert_eq!(existing_count, Some(2));
    }

    #[test]
    fn same_thread_other_store_does_not_contaminate_target() {
        let (_dir, db_path, _existing_id) = readonly_store_fixture();
        let store_a = Store::open_existing_readonly(&db_path).unwrap();
        let store_b = Store::open_existing_readonly(&db_path).unwrap();
        let missing = ChunkId("missing".into());

        let (operation_result, count) =
            store_a.count_sql_statements(|| store_b.get_chunk(&missing));
        assert!(operation_result.unwrap().is_none());
        assert_eq!(count, Some(0));
        assert_eq!(
            store_a
                .count_sql_statements(|| store_a.get_chunk(&missing).unwrap())
                .1,
            Some(1)
        );
    }

    #[test]
    fn nested_target_stores_invalidate_both_windows() {
        let (_dir, db_path, _existing_id) = readonly_store_fixture();
        let store_a = Store::open_existing_readonly(&db_path).unwrap();
        let store_b = Store::open_existing_readonly(&db_path).unwrap();

        let (_, outer_count) = store_a.count_sql_statements(|| {
            let (_, inner_count) = store_b.count_sql_statements(|| {
                store_b.get_chunk(&ChunkId("missing".into())).unwrap();
            });
            assert_eq!(inner_count, None);
        });
        assert_eq!(outer_count, None);
    }

    #[test]
    fn panic_disables_callback_and_clears_window() {
        let (_dir, db_path, _existing_id) = readonly_store_fixture();
        let store_a = Store::open_existing_readonly(&db_path).unwrap();
        let store_b = Store::open_existing_readonly(&db_path).unwrap();

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store_a.count_sql_statements(|| panic!("counting window panic"));
        }));
        assert!(panic.is_err());
        let missing = ChunkId("missing".into());
        assert_eq!(
            store_b
                .count_sql_statements(|| store_a.get_chunk(&missing).unwrap())
                .1,
            Some(0)
        );
        assert_eq!(
            store_a
                .count_sql_statements(|| store_a.get_chunk(&missing).unwrap())
                .1,
            Some(1)
        );
    }

    #[test]
    fn writable_stores_do_not_advertise_statement_counting() {
        let (_dir, db_path, _existing_id) = readonly_store_fixture();
        let readonly = Store::open_existing_readonly(&db_path).unwrap();
        let in_memory = Store::in_memory().unwrap();
        assert_eq!(
            in_memory
                .count_sql_statements(|| {
                    in_memory
                        .get_chunk(&ChunkId("in-memory-missing".into()))
                        .unwrap()
                })
                .1,
            None
        );
        assert_eq!(
            readonly
                .count_sql_statements(|| {
                    in_memory
                        .get_chunk(&ChunkId("in-memory-missing".into()))
                        .unwrap()
                })
                .1,
            Some(0)
        );

        let dir = tempdir().unwrap();
        let writable = Store::new(&dir.path().join("writable.db")).unwrap();
        assert_eq!(
            writable
                .count_sql_statements(|| {
                    writable
                        .get_chunk(&ChunkId("writable-missing".into()))
                        .unwrap()
                })
                .1,
            None
        );
        assert_eq!(
            readonly
                .count_sql_statements(|| {
                    writable
                        .get_chunk(&ChunkId("writable-missing".into()))
                        .unwrap()
                })
                .1,
            Some(0)
        );
    }

    #[test]
    fn in_memory_store_reports_retrieval_resources_unavailable() {
        let store = Store::in_memory().unwrap();

        let (value, sql_statements, resources) = store.measure_retrieval(|| 42);

        assert_eq!(value, 42);
        assert_eq!(sql_statements, None);
        assert_eq!(resources, None);
    }

    #[test]
    fn isolates_concurrent_threads() {
        let (_dir, db_path, existing_id) = readonly_store_fixture();

        let barrier = Arc::new(Barrier::new(2));
        let missing_path = db_path.clone();
        let missing_barrier = Arc::clone(&barrier);
        let missing = std::thread::spawn(move || {
            let store = Store::open_existing_readonly(&missing_path).unwrap();
            missing_barrier.wait();
            store
                .count_sql_statements(|| store.get_chunk(&ChunkId("missing".into())).unwrap())
                .1
        });
        let existing_barrier = Arc::clone(&barrier);
        let existing = std::thread::spawn(move || {
            let store = Store::open_existing_readonly(&db_path).unwrap();
            existing_barrier.wait();
            store
                .count_sql_statements(|| store.get_chunk(&existing_id).unwrap())
                .1
        });

        assert_eq!(missing.join().unwrap(), Some(1));
        assert_eq!(existing.join().unwrap(), Some(2));
    }
}
