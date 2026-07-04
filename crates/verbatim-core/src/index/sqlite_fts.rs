use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::time::{Duration, Instant};

use crate::store::Store;
use crate::traits::{LexicalDocument, LexicalIndex};
use crate::types::{Chunk, ChunkId, SourceId};

pub struct SqliteFtsIndex<'a> {
    store: &'a Store,
}

const FTS_PROJECTION_VERSION: &str = "2";
const FTS_PROJECTION_VERSION_KEY: &str = "sqlite_fts_projection_version";
const FTS_DIRTY_KEY: &str = "sqlite_fts_dirty";
const FTS_TRIGGER_TEXT_PROJECTION: &str = "CASE
    WHEN c.context_text IS NULL OR c.context_text = '' THEN c.text
    ELSE c.context_text || ' ' || c.text
END";
const FTS_TRIGGER_HEADING_PROJECTION: &str = "COALESCE(c.heading_path_json, '')";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsMaintenanceStatus {
    Skipped,
    Rebuilt,
    Repaired,
}

impl FtsMaintenanceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Rebuilt => "rebuilt",
            Self::Repaired => "repaired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsMaintenanceReason {
    Current,
    ManualRebuild,
    MissingProjectionVersion,
    ProjectionVersionMismatch,
    DirtyMarker,
    RowCountMismatch,
    MissingRows,
    OrphanRows,
    IntegrityCheckFailed,
}

impl FtsMaintenanceReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::ManualRebuild => "manual_rebuild",
            Self::MissingProjectionVersion => "missing_projection_version",
            Self::ProjectionVersionMismatch => "projection_version_mismatch",
            Self::DirtyMarker => "dirty_marker",
            Self::RowCountMismatch => "row_count_mismatch",
            Self::MissingRows => "missing_rows",
            Self::OrphanRows => "orphan_rows",
            Self::IntegrityCheckFailed => "integrity_check_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FtsMaintenanceCounts {
    pub child_rows: u64,
    pub fts_rows: u64,
    pub missing_rows: u64,
    pub orphan_rows: u64,
}

impl FtsMaintenanceCounts {
    fn is_empty_and_aligned(self) -> bool {
        self.child_rows == 0
            && self.fts_rows == 0
            && self.missing_rows == 0
            && self.orphan_rows == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FtsMaintenanceOutcome {
    pub status: FtsMaintenanceStatus,
    pub reason: FtsMaintenanceReason,
    pub counts: FtsMaintenanceCounts,
    pub duration: Duration,
}

impl Default for FtsMaintenanceOutcome {
    fn default() -> Self {
        Self {
            status: FtsMaintenanceStatus::Skipped,
            reason: FtsMaintenanceReason::Current,
            counts: FtsMaintenanceCounts::default(),
            duration: Duration::ZERO,
        }
    }
}

impl<'a> SqliteFtsIndex<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn maintain_startup(&self) -> Result<FtsMaintenanceOutcome> {
        let started = Instant::now();
        let conn = self.store.connection();
        validate_fts_schema(conn).context("validate SQLite FTS schema")?;

        let counts = fts_maintenance_counts(conn).context("inspect SQLite FTS row alignment")?;
        let reason = startup_rebuild_reason(conn, counts)?;

        match reason {
            None => {
                validate_fts_integrity(conn).context("validate SQLite FTS integrity")?;
                Ok(FtsMaintenanceOutcome {
                    status: FtsMaintenanceStatus::Skipped,
                    reason: FtsMaintenanceReason::Current,
                    counts,
                    duration: started.elapsed(),
                })
            }
            Some(reason) if counts.is_empty_and_aligned() => {
                mark_fts_current(conn, started, reason)
                    .context("mark empty SQLite FTS index current")
            }
            Some(reason) => {
                let status = rebuild_status_for_reason(reason);
                self.rebuild_from_store_for_reason(status, reason, started)
                    .with_context(|| {
                        format!(
                            "repair SQLite FTS startup maintenance after {}",
                            reason.as_str()
                        )
                    })
            }
        }
    }

    fn rebuild_from_store_for_reason(
        &self,
        status: FtsMaintenanceStatus,
        reason: FtsMaintenanceReason,
        started: Instant,
    ) -> Result<FtsMaintenanceOutcome> {
        let conn = self.store.connection();
        validate_fts_schema(conn).context("validate SQLite FTS schema before rebuild")?;
        set_index_meta(conn, FTS_DIRTY_KEY, reason.as_str()).context("mark SQLite FTS dirty")?;

        let tx = conn
            .unchecked_transaction()
            .context("begin SQLite FTS rebuild")?;
        tx.execute("DELETE FROM chunk_fts", [])
            .context("clear SQLite FTS rows")?;
        tx.execute(
            &format!(
                "INSERT INTO chunk_fts(rowid, chunk_id, source_id, text, heading)
                 SELECT c.rowid, c.id, c.source_id, {FTS_TRIGGER_TEXT_PROJECTION}, {FTS_TRIGGER_HEADING_PROJECTION}
                 FROM chunks c
                 WHERE c.chunk_type = 'Child'
                 ORDER BY c.source_id, c.id"
            ),
            [],
        )
        .context("stream chunks into SQLite FTS")?;
        set_index_meta_tx(&tx, FTS_PROJECTION_VERSION_KEY, FTS_PROJECTION_VERSION)
            .context("record SQLite FTS projection version")?;
        delete_index_meta_tx(&tx, FTS_DIRTY_KEY).context("clear SQLite FTS dirty marker")?;
        tx.commit().context("commit SQLite FTS rebuild")?;

        let counts = fts_maintenance_counts(conn).context("inspect SQLite FTS after rebuild")?;
        Ok(FtsMaintenanceOutcome {
            status,
            reason,
            counts,
            duration: started.elapsed(),
        })
    }
}

impl LexicalIndex for SqliteFtsIndex<'_> {
    fn upsert(&self, document: &LexicalDocument) -> Result<()> {
        let conn = self.store.connection();
        let rowid: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM chunks WHERE id = ?1 AND chunk_type = 'Child'",
                params![&document.chunk_id.0],
                |row| row.get(0),
            )
            .optional()
            .context("lookup chunk rowid for FTS upsert")?;
        let rowid = rowid.with_context(|| {
            format!(
                "cannot upsert lexical document for missing child chunk: {}",
                document.chunk_id.0
            )
        })?;

        conn.execute("DELETE FROM chunk_fts WHERE rowid = ?1", params![rowid])
            .context("delete previous FTS row")?;
        conn.execute(
            "INSERT INTO chunk_fts(rowid, chunk_id, source_id, text, heading) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                rowid,
                &document.chunk_id.0,
                &document.source_id.0,
                &document.text,
                &document.heading,
            ],
        )
        .context("insert FTS row")?;
        Ok(())
    }

    fn delete_source(&self, source_id: &SourceId) -> Result<()> {
        self.store
            .connection()
            .execute(
                "DELETE FROM chunk_fts WHERE source_id = ?1",
                params![&source_id.0],
            )
            .context("delete source from FTS")?;
        Ok(())
    }

    fn search(&self, query: &str, top_k: usize) -> Result<Vec<(ChunkId, f32)>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let Some(fts_query) = normalize_fts_query(query) else {
            return Ok(Vec::new());
        };

        let mut stmt = self
            .store
            .connection()
            .prepare(
                "SELECT chunk_id, bm25(chunk_fts) AS rank
                 FROM chunk_fts
                 WHERE chunk_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .context("prepare FTS search")?;
        let rows = stmt
            .query_map(params![fts_query, top_k as i64], |row| {
                let chunk_id: String = row.get(0)?;
                let rank: f64 = row.get(1)?;
                let score = 1.0 / (1.0 + rank.abs() as f32);
                Ok((ChunkId(chunk_id), score))
            })
            .context("execute FTS search")?;

        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn rebuild_from_store(&self, _store: &Store) -> Result<()> {
        self.rebuild_from_store_for_reason(
            FtsMaintenanceStatus::Rebuilt,
            FtsMaintenanceReason::ManualRebuild,
            Instant::now(),
        )?;
        Ok(())
    }
}

pub fn lexical_document_for_chunk(chunk: &Chunk) -> Result<LexicalDocument> {
    Ok(LexicalDocument {
        chunk_id: chunk.id.clone(),
        source_id: chunk.source_id.clone(),
        text: chunk_search_text(chunk),
        heading: chunk_heading_projection(chunk)?,
    })
}

fn chunk_search_text(chunk: &Chunk) -> String {
    chunk
        .context_text
        .as_ref()
        .filter(|text| !text.is_empty())
        .map(|ctx| format!("{ctx} {}", chunk.text))
        .unwrap_or_else(|| chunk.text.clone())
}

fn chunk_heading_projection(chunk: &Chunk) -> Result<String> {
    serde_json::to_string(&chunk.heading_path).context("serialize chunk heading path for FTS")
}

fn startup_rebuild_reason(
    conn: &Connection,
    counts: FtsMaintenanceCounts,
) -> Result<Option<FtsMaintenanceReason>> {
    if counts.missing_rows > 0 {
        return Ok(Some(FtsMaintenanceReason::MissingRows));
    }
    if counts.orphan_rows > 0 {
        return Ok(Some(FtsMaintenanceReason::OrphanRows));
    }
    if counts.child_rows != counts.fts_rows {
        return Ok(Some(FtsMaintenanceReason::RowCountMismatch));
    }
    if read_index_meta(conn, FTS_DIRTY_KEY)?.is_some() {
        return Ok(Some(FtsMaintenanceReason::DirtyMarker));
    }

    match read_index_meta(conn, FTS_PROJECTION_VERSION_KEY)? {
        None => Ok(Some(FtsMaintenanceReason::MissingProjectionVersion)),
        Some(version) if version == FTS_PROJECTION_VERSION => Ok(None),
        Some(_) => Ok(Some(FtsMaintenanceReason::ProjectionVersionMismatch)),
    }
}

fn rebuild_status_for_reason(reason: FtsMaintenanceReason) -> FtsMaintenanceStatus {
    match reason {
        FtsMaintenanceReason::MissingRows
        | FtsMaintenanceReason::OrphanRows
        | FtsMaintenanceReason::RowCountMismatch
        | FtsMaintenanceReason::DirtyMarker
        | FtsMaintenanceReason::IntegrityCheckFailed => FtsMaintenanceStatus::Repaired,
        FtsMaintenanceReason::Current
        | FtsMaintenanceReason::ManualRebuild
        | FtsMaintenanceReason::MissingProjectionVersion
        | FtsMaintenanceReason::ProjectionVersionMismatch => FtsMaintenanceStatus::Rebuilt,
    }
}

fn mark_fts_current(
    conn: &Connection,
    started: Instant,
    reason: FtsMaintenanceReason,
) -> Result<FtsMaintenanceOutcome> {
    let tx = conn
        .unchecked_transaction()
        .context("begin SQLite FTS metadata repair")?;
    set_index_meta_tx(&tx, FTS_PROJECTION_VERSION_KEY, FTS_PROJECTION_VERSION)
        .context("record SQLite FTS projection version")?;
    delete_index_meta_tx(&tx, FTS_DIRTY_KEY).context("clear SQLite FTS dirty marker")?;
    tx.commit().context("commit SQLite FTS metadata repair")?;
    let counts =
        fts_maintenance_counts(conn).context("inspect SQLite FTS after metadata repair")?;
    Ok(FtsMaintenanceOutcome {
        status: FtsMaintenanceStatus::Repaired,
        reason,
        counts,
        duration: started.elapsed(),
    })
}

fn validate_fts_schema(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(chunk_fts)")
        .context("prepare SQLite FTS schema inspection")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .context("inspect SQLite FTS schema")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read SQLite FTS schema columns")?;
    let expected = ["chunk_id", "source_id", "text", "heading"];
    if !columns
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
    {
        bail!(
            "SQLite FTS schema mismatch for chunk_fts: expected {:?}, got {:?}",
            expected,
            columns
        );
    }
    Ok(())
}

fn validate_fts_integrity(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO chunk_fts(chunk_fts) VALUES('integrity-check')",
        [],
    )
    .context("run SQLite FTS5 integrity-check")?;
    Ok(())
}

fn fts_maintenance_counts(conn: &Connection) -> Result<FtsMaintenanceCounts> {
    Ok(FtsMaintenanceCounts {
        child_rows: count_sql(
            conn,
            "SELECT COUNT(*) FROM chunks WHERE chunk_type = 'Child'",
            "count child chunks",
        )?,
        fts_rows: count_sql(
            conn,
            "SELECT COUNT(*) FROM chunk_fts",
            "count SQLite FTS rows",
        )?,
        missing_rows: count_sql(
            conn,
            "SELECT COUNT(*)
             FROM chunks c
             WHERE c.chunk_type = 'Child'
               AND NOT EXISTS (
                   SELECT 1
                   FROM chunk_fts f
                   WHERE f.rowid = c.rowid
                     AND f.chunk_id = c.id
                     AND f.source_id = c.source_id
               )",
            "count missing SQLite FTS rows",
        )?,
        orphan_rows: count_sql(
            conn,
            "SELECT COUNT(*)
             FROM chunk_fts f
             WHERE NOT EXISTS (
                 SELECT 1
                 FROM chunks c
                 WHERE c.rowid = f.rowid
                   AND c.chunk_type = 'Child'
                   AND c.id = f.chunk_id
                   AND c.source_id = f.source_id
             )",
            "count orphan SQLite FTS rows",
        )?,
    })
}

fn count_sql(conn: &Connection, sql: &str, context: &'static str) -> Result<u64> {
    let count: i64 = conn.query_row(sql, [], |row| row.get(0)).context(context)?;
    count
        .try_into()
        .with_context(|| format!("{context}: negative count {count}"))
}

fn read_index_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM index_meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .with_context(|| format!("read index_meta key {key}"))
}

fn set_index_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO index_meta(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .with_context(|| format!("set index_meta key {key}"))?;
    Ok(())
}

fn set_index_meta_tx(tx: &Transaction<'_>, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO index_meta(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .with_context(|| format!("set index_meta key {key}"))?;
    Ok(())
}

fn delete_index_meta_tx(tx: &Transaction<'_>, key: &str) -> Result<()> {
    tx.execute("DELETE FROM index_meta WHERE key = ?1", params![key])
        .with_context(|| format!("delete index_meta key {key}"))?;
    Ok(())
}

fn normalize_fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect();

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::types::{
        Chunk, ChunkType, EvidenceId, EvidenceKind, EvidenceUnit, Source, SourceLocator,
        SourceStatus,
    };
    use std::path::PathBuf;

    fn source(id: &str) -> Source {
        Source {
            id: SourceId(id.into()),
            path: PathBuf::from(format!("/tmp/{id}.txt")),
            hash: format!("hash-{id}"),
            status: SourceStatus::Indexed,
            parser_used: Some("plaintext".into()),
            last_ingested_at: None,
        }
    }

    fn evidence(source_id: &SourceId, id: &str) -> EvidenceUnit {
        EvidenceUnit {
            id: EvidenceId(id.into()),
            source_id: source_id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: source_id.0.clone(),
                line_start: 1,
                line_end: None,
            },
            text: "text".into(),
            text_hash: format!("hash-{id}"),
            heading_path: Vec::new(),
            position: 0,
        }
    }

    fn child(source_id: &SourceId, id: &str, evidence_id: &EvidenceId, text: &str) -> Chunk {
        Chunk {
            id: ChunkId(id.into()),
            source_id: source_id.clone(),
            chunk_hash: format!("hash-{id}"),
            embedding_input_hash: None,
            text: text.into(),
            context_text: None,
            token_count: 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Heading".into()],
            evidence_unit_ids: vec![evidence_id.clone()],
        }
    }

    fn fts_projection(store: &Store, chunk_id: &str) -> (String, String) {
        store
            .connection()
            .query_row(
                "SELECT text, heading FROM chunk_fts WHERE chunk_id = ?1",
                params![chunk_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn clear_fts(store: &Store) {
        store
            .connection()
            .execute("DELETE FROM chunk_fts", [])
            .unwrap();
    }

    fn insert_child(store: &Store, source: &Source, chunk_id: &str, text: &str) -> Chunk {
        let evidence = evidence(&source.id, &format!("ev-{chunk_id}"));
        let chunk = child(&source.id, chunk_id, &evidence.id, text);
        store.add_source(source).unwrap();
        store.bulk_insert_evidence(&[evidence]).unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        chunk
    }

    #[test]
    fn fts_triggers_index_inserted_chunks() {
        let store = Store::in_memory().unwrap();
        let source = source("src-1");
        insert_child(&store, &source, "chunk-1", "alpha fox retrieval");
        let index = SqliteFtsIndex::new(&store);

        let results = index.search("fox?", 5).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0 .0, "chunk-1");
    }

    #[test]
    fn delete_source_removes_lexical_hits() {
        let store = Store::in_memory().unwrap();
        let first = source("src-1");
        let second = source("src-2");
        insert_child(&store, &first, "chunk-1", "alpha deleted");
        insert_child(&store, &second, "chunk-2", "alpha retained");
        let index = SqliteFtsIndex::new(&store);

        index.delete_source(&first.id).unwrap();
        let results = index.search("alpha", 5).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0 .0, "chunk-2");
    }

    #[test]
    fn rebuild_from_store_restores_lexical_hits() {
        let store = Store::in_memory().unwrap();
        let source = source("src-1");
        insert_child(&store, &source, "chunk-1", "rebuildable alpha");
        let index = SqliteFtsIndex::new(&store);
        index.delete_source(&source.id).unwrap();
        assert!(index.search("alpha", 5).unwrap().is_empty());

        index.rebuild_from_store(&store).unwrap();

        let results = index.search("alpha", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0 .0, "chunk-1");
    }

    #[test]
    fn fts_projection_matches_trigger_upsert_and_rebuild() {
        let store = Store::in_memory().unwrap();
        let source = source("src-1");
        let evidence = evidence(&source.id, "ev-chunk-1");
        let mut chunk = child(&source.id, "chunk-1", &evidence.id, "body alpha");
        chunk.context_text = Some("context beta".into());
        chunk.heading_path = vec!["Chapter".into(), "Section".into()];
        store.add_source(&source).unwrap();
        store.bulk_insert_evidence(&[evidence]).unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        let trigger_projection = fts_projection(&store, "chunk-1");
        assert_eq!(trigger_projection.0, "context beta body alpha");
        assert_eq!(
            trigger_projection.1,
            serde_json::to_string(&chunk.heading_path).unwrap()
        );
        let index = SqliteFtsIndex::new(&store);

        clear_fts(&store);
        let document = lexical_document_for_chunk(&chunk).unwrap();
        index.upsert(&document).unwrap();
        assert_eq!(fts_projection(&store, "chunk-1"), trigger_projection);

        clear_fts(&store);
        index.rebuild_from_store(&store).unwrap();
        assert_eq!(fts_projection(&store, "chunk-1"), trigger_projection);
    }

    #[test]
    fn fts_startup_maintenance_rebuilds_old_db_then_skips_current() {
        let store = Store::in_memory().unwrap();
        let source = source("src-1");
        insert_child(&store, &source, "chunk-1", "alpha old database");
        let index = SqliteFtsIndex::new(&store);

        let first = index.maintain_startup().unwrap();
        assert_eq!(first.status, FtsMaintenanceStatus::Rebuilt);
        assert_eq!(first.reason, FtsMaintenanceReason::MissingProjectionVersion);
        assert_eq!(first.counts.child_rows, 1);
        assert_eq!(first.counts.fts_rows, 1);
        assert_eq!(index.search("alpha", 5).unwrap()[0].0 .0, "chunk-1");

        let second = index.maintain_startup().unwrap();
        assert_eq!(second.status, FtsMaintenanceStatus::Skipped);
        assert_eq!(second.reason, FtsMaintenanceReason::Current);
        assert_eq!(second.counts.child_rows, 1);
        assert_eq!(second.counts.fts_rows, 1);
    }

    #[test]
    fn fts_startup_maintenance_repairs_empty_metadata_without_rebuild() {
        let store = Store::in_memory().unwrap();
        let index = SqliteFtsIndex::new(&store);

        let first = index.maintain_startup().unwrap();
        assert_eq!(first.status, FtsMaintenanceStatus::Repaired);
        assert_eq!(first.reason, FtsMaintenanceReason::MissingProjectionVersion);
        assert_eq!(first.counts, FtsMaintenanceCounts::default());

        let second = index.maintain_startup().unwrap();
        assert_eq!(second.status, FtsMaintenanceStatus::Skipped);
        assert_eq!(second.reason, FtsMaintenanceReason::Current);
    }

    #[test]
    fn fts_repair_restores_missing_rows() {
        let store = Store::in_memory().unwrap();
        let source = source("src-1");
        insert_child(&store, &source, "chunk-1", "repairable alpha");
        let index = SqliteFtsIndex::new(&store);
        index.maintain_startup().unwrap();
        clear_fts(&store);
        assert!(index.search("alpha", 5).unwrap().is_empty());

        let outcome = index.maintain_startup().unwrap();

        assert_eq!(outcome.status, FtsMaintenanceStatus::Repaired);
        assert_eq!(outcome.reason, FtsMaintenanceReason::MissingRows);
        assert_eq!(outcome.counts.child_rows, 1);
        assert_eq!(outcome.counts.fts_rows, 1);
        assert_eq!(index.search("alpha", 5).unwrap()[0].0 .0, "chunk-1");
    }

    #[test]
    fn fts_repair_removes_orphan_rows() {
        let store = Store::in_memory().unwrap();
        let source = source("src-1");
        insert_child(&store, &source, "chunk-1", "retained alpha");
        let index = SqliteFtsIndex::new(&store);
        index.maintain_startup().unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO chunk_fts(rowid, chunk_id, source_id, text, heading)
                 VALUES (9999, 'orphan-chunk', 'orphan-source', 'orphan beta', '[]')",
                [],
            )
            .unwrap();
        assert_eq!(index.search("orphan", 5).unwrap()[0].0 .0, "orphan-chunk");

        let outcome = index.maintain_startup().unwrap();

        assert_eq!(outcome.status, FtsMaintenanceStatus::Repaired);
        assert_eq!(outcome.reason, FtsMaintenanceReason::OrphanRows);
        assert!(index.search("orphan", 5).unwrap().is_empty());
        assert_eq!(index.search("alpha", 5).unwrap()[0].0 .0, "chunk-1");
    }

    #[test]
    fn fts_startup_maintenance_rebuilds_version_mismatch_and_dirty_marker() {
        let store = Store::in_memory().unwrap();
        let source = source("src-1");
        insert_child(&store, &source, "chunk-1", "version alpha");
        let index = SqliteFtsIndex::new(&store);
        index.maintain_startup().unwrap();
        set_index_meta(
            store.connection(),
            FTS_PROJECTION_VERSION_KEY,
            "old-version",
        )
        .unwrap();

        let version_outcome = index.maintain_startup().unwrap();
        assert_eq!(version_outcome.status, FtsMaintenanceStatus::Rebuilt);
        assert_eq!(
            version_outcome.reason,
            FtsMaintenanceReason::ProjectionVersionMismatch
        );

        set_index_meta(store.connection(), FTS_DIRTY_KEY, "test").unwrap();
        let dirty_outcome = index.maintain_startup().unwrap();
        assert_eq!(dirty_outcome.status, FtsMaintenanceStatus::Repaired);
        assert_eq!(dirty_outcome.reason, FtsMaintenanceReason::DirtyMarker);
    }

    #[test]
    fn fts_startup_maintenance_reports_schema_mismatch() {
        let store = Store::in_memory().unwrap();
        store
            .connection()
            .execute_batch(
                "
                DROP TABLE chunk_fts;
                CREATE VIRTUAL TABLE chunk_fts USING fts5(body);
                ",
            )
            .unwrap();
        let index = SqliteFtsIndex::new(&store);

        let error = index.maintain_startup().unwrap_err();

        assert!(format!("{error:#}").contains("SQLite FTS schema mismatch"));
    }
}
