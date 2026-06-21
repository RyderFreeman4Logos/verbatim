use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};

use crate::store::Store;
use crate::traits::{LexicalDocument, LexicalIndex};
use crate::types::{Chunk, ChunkId, SourceId};

pub struct SqliteFtsIndex<'a> {
    store: &'a Store,
}

impl<'a> SqliteFtsIndex<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
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

    fn rebuild_from_store(&self, store: &Store) -> Result<()> {
        let documents: Vec<_> = store
            .list_child_chunks()?
            .into_iter()
            .map(|chunk| lexical_document_for_chunk(&chunk))
            .collect();

        let tx = self
            .store
            .connection()
            .unchecked_transaction()
            .context("begin FTS rebuild")?;
        tx.execute("DELETE FROM chunk_fts", [])
            .context("clear FTS rows")?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO chunk_fts(rowid, chunk_id, source_id, text, heading)
                     SELECT rowid, ?1, ?2, ?3, ?4 FROM chunks WHERE id = ?1 AND chunk_type = 'Child'",
                )
                .context("prepare FTS rebuild insert")?;
            for document in &documents {
                let inserted = stmt
                    .execute(params![
                        &document.chunk_id.0,
                        &document.source_id.0,
                        &document.text,
                        &document.heading,
                    ])
                    .with_context(|| format!("insert FTS row for {}", document.chunk_id.0))?;
                if inserted != 1 {
                    bail!(
                        "cannot rebuild lexical document for missing child chunk: {}",
                        document.chunk_id.0
                    );
                }
            }
        }
        tx.commit().context("commit FTS rebuild")?;
        Ok(())
    }
}

pub fn lexical_document_for_chunk(chunk: &Chunk) -> LexicalDocument {
    LexicalDocument {
        chunk_id: chunk.id.clone(),
        source_id: chunk.source_id.clone(),
        text: chunk_search_text(chunk),
        heading: chunk.heading_path.join(" > "),
    }
}

fn chunk_search_text(chunk: &Chunk) -> String {
    chunk
        .context_text
        .as_ref()
        .filter(|text| !text.is_empty())
        .map(|ctx| format!("{ctx} {}", chunk.text))
        .unwrap_or_else(|| chunk.text.clone())
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
        Chunk, ChunkType, EvidenceId, EvidenceUnit, Source, SourceLocator, SourceStatus,
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
            text: text.into(),
            context_text: None,
            token_count: 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: vec!["Heading".into()],
            evidence_unit_ids: vec![evidence_id.clone()],
        }
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
}
