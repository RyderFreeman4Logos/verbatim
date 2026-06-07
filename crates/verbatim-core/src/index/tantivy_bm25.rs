use std::path::Path;

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, Value, STORED, TEXT};
use tantivy::{doc, Index, IndexWriter, ReloadPolicy};

use crate::types::ChunkId;

pub struct Bm25Index {
    index: Index,
    chunk_id_field: tantivy::schema::Field,
    text_field: tantivy::schema::Field,
    heading_field: tantivy::schema::Field,
}

impl Bm25Index {
    pub fn open_or_create(path: &Path) -> Result<Self> {
        let schema = build_schema();
        let chunk_id_field = schema.get_field("chunk_id").unwrap();
        let text_field = schema.get_field("text").unwrap();
        let heading_field = schema.get_field("heading").unwrap();

        std::fs::create_dir_all(path).context("create tantivy dir")?;
        let dir = MmapDirectory::open(path).context("open mmap directory")?;
        let index = if Index::exists(&dir)? {
            Index::open(dir).context("open tantivy index")?
        } else {
            Index::create(dir, schema, tantivy::IndexSettings::default())
                .context("create tantivy index")?
        };

        Ok(Self {
            index,
            chunk_id_field,
            text_field,
            heading_field,
        })
    }

    pub fn create_in_ram() -> Result<Self> {
        let schema = build_schema();
        let chunk_id_field = schema.get_field("chunk_id").unwrap();
        let text_field = schema.get_field("text").unwrap();
        let heading_field = schema.get_field("heading").unwrap();
        let index = Index::create_in_ram(schema);

        Ok(Self {
            index,
            chunk_id_field,
            text_field,
            heading_field,
        })
    }

    pub fn add_documents(&self, docs: &[(ChunkId, String, String)]) -> Result<()> {
        let mut writer: IndexWriter = self
            .index
            .writer(50_000_000)
            .context("create index writer")?;

        for (chunk_id, text, heading) in docs {
            writer.add_document(doc!(
                self.chunk_id_field => chunk_id.0.as_str(),
                self.text_field => text.as_str(),
                self.heading_field => heading.as_str(),
            ))?;
        }

        writer.commit().context("commit tantivy index")?;
        Ok(())
    }

    pub fn clear_and_rebuild(&self, docs: &[(ChunkId, String, String)]) -> Result<()> {
        let mut writer: IndexWriter = self
            .index
            .writer(50_000_000)
            .context("create index writer")?;
        writer.delete_all_documents()?;
        writer.commit()?;

        for (chunk_id, text, heading) in docs {
            writer.add_document(doc!(
                self.chunk_id_field => chunk_id.0.as_str(),
                self.text_field => text.as_str(),
                self.heading_field => heading.as_str(),
            ))?;
        }

        writer.commit().context("rebuild tantivy index")?;
        Ok(())
    }

    pub fn search(&self, query_str: &str, top_k: usize) -> Result<Vec<(ChunkId, f32)>> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("create reader")?;
        let searcher = reader.searcher();

        let query_parser =
            QueryParser::for_index(&self.index, vec![self.text_field, self.heading_field]);
        let query = query_parser
            .parse_query(query_str)
            .context("parse BM25 query")?;

        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(top_k).order_by_score())
            .context("BM25 search")?;

        let mut results = Vec::new();
        for (score, doc_addr) in top_docs {
            let doc = searcher
                .doc::<tantivy::TantivyDocument>(doc_addr)
                .context("retrieve doc")?;
            if let Some(chunk_id_val) = doc.get_first(self.chunk_id_field) {
                if let Some(id_str) = chunk_id_val.as_str() {
                    results.push((ChunkId(id_str.to_string()), score));
                }
            }
        }

        Ok(results)
    }
}

fn build_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field("chunk_id", STORED);
    builder.add_text_field("text", TEXT);
    builder.add_text_field("heading", TEXT);
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_search() {
        let index = Bm25Index::create_in_ram().unwrap();
        let docs = vec![
            (
                ChunkId("c1".into()),
                "The quick brown fox jumps over the lazy dog".into(),
                "Animals".into(),
            ),
            (
                ChunkId("c2".into()),
                "Machine learning algorithms process large datasets".into(),
                "Technology".into(),
            ),
            (
                ChunkId("c3".into()),
                "The fox ran through the forest quickly".into(),
                "Animals".into(),
            ),
        ];
        index.add_documents(&docs).unwrap();

        let results = index.search("fox", 5).unwrap();
        assert!(!results.is_empty());
        let ids: Vec<&str> = results.iter().map(|(id, _)| id.0.as_str()).collect();
        assert!(ids.contains(&"c1") || ids.contains(&"c3"));
    }

    #[test]
    fn empty_index_search() {
        let index = Bm25Index::create_in_ram().unwrap();
        let results = index.search("anything", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn clear_and_rebuild() {
        let index = Bm25Index::create_in_ram().unwrap();
        let docs1 = vec![(ChunkId("old".into()), "old data here".into(), "".into())];
        index.add_documents(&docs1).unwrap();

        let docs2 = vec![(
            ChunkId("new".into()),
            "completely new content".into(),
            "".into(),
        )];
        index.clear_and_rebuild(&docs2).unwrap();

        let results = index.search("old", 5).unwrap();
        assert!(results.is_empty());

        let results = index.search("new", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0 .0, "new");
    }
}
