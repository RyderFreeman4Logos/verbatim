use std::path::Path;

use anyhow::{Context, Result};
use instant_distance::{Builder, HnswMap, Point, Search};
use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::traits::{VectorDocument, VectorIndex};
use crate::types::{ChunkId, EmbeddingProfileId, SourceId};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct VecPoint(Vec<f32>);

impl Point for VecPoint {
    fn distance(&self, other: &Self) -> f32 {
        self.0
            .iter()
            .zip(other.0.iter())
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f32>()
            .sqrt()
    }
}

pub struct HnswIndex {
    points: Vec<VectorDocument>,
    map: Option<HnswMap<VecPoint, ChunkId>>,
}

impl HnswIndex {
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            map: None,
        }
    }

    pub fn add(&mut self, chunk_id: &ChunkId, vector: Vec<f32>) {
        self.upsert(VectorDocument {
            chunk_id: chunk_id.clone(),
            source_id: SourceId(String::new()),
            vector,
        });
    }

    pub fn add_for_source(&mut self, chunk_id: &ChunkId, source_id: &SourceId, vector: Vec<f32>) {
        self.upsert(VectorDocument {
            chunk_id: chunk_id.clone(),
            source_id: source_id.clone(),
            vector,
        });
    }

    pub fn clear(&mut self) {
        self.points.clear();
        self.map = None;
    }

    pub fn build(&mut self) -> Result<()> {
        if self.points.is_empty() {
            self.map = None;
            return Ok(());
        }

        let mut values = Vec::with_capacity(self.points.len());
        let mut keys = Vec::with_capacity(self.points.len());
        for document in &self.points {
            values.push(VecPoint(document.vector.clone()));
            keys.push(document.chunk_id.clone());
        }

        let map = Builder::default().build(values, keys);
        self.map = Some(map);
        Ok(())
    }

    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)> {
        let map = match &self.map {
            Some(m) => m,
            None => return Vec::new(),
        };

        let query_point = VecPoint(query.to_vec());
        let mut search = Search::default();
        let results = map.search(&query_point, &mut search);

        results
            .take(top_k)
            .map(|item| {
                let chunk_id = item.value.clone();
                let distance = item.distance;
                let score = 1.0 / (1.0 + distance);
                (chunk_id, score)
            })
            .collect()
    }

    /// Magic bytes prefixing bincode-serialized HNSW data for format detection.
    const BINCODE_MAGIC: &[u8] = b"VRBH"; // Verbatim Binary Hnsw

    pub fn save(&self, path: &Path) -> Result<()> {
        let payload = bincode::serialize(&self.points).context("serialize HNSW data (bincode)")?;
        let mut data = Vec::with_capacity(Self::BINCODE_MAGIC.len() + payload.len());
        data.extend_from_slice(Self::BINCODE_MAGIC);
        data.extend_from_slice(&payload);
        std::fs::write(path, data).context("write HNSW index")?;
        Ok(())
    }

    pub fn load(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let data = std::fs::read(path).context("read HNSW index")?;
        self.points = if data.starts_with(Self::BINCODE_MAGIC) {
            // New bincode format.
            bincode::deserialize(&data[Self::BINCODE_MAGIC.len()..])
                .context("deserialize HNSW data (bincode)")?
        } else {
            // Legacy JSON format — backward compatibility.
            serde_json::from_slice(&data).context("deserialize HNSW data (legacy JSON)")?
        };
        self.build()?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn rebuild_from_store_for_profile(
        &mut self,
        store: &Store,
        profile_id: &EmbeddingProfileId,
    ) -> Result<()> {
        self.points = store.list_vector_documents_for_profile(profile_id)?;
        self.build()
    }

    /// Bulk-replace all points by moving a Vec, avoiding per-document retain+push.
    ///
    /// This is O(n) instead of the O(n²) that calling `upsert` in a loop would cost,
    /// and avoids cloning the vectors since ownership is moved.
    pub fn replace_all(&mut self, documents: Vec<VectorDocument>) {
        self.points = documents;
        self.map = None;
    }

    /// Returns a slice of all stored vector documents.
    pub fn points(&self) -> &[VectorDocument] {
        &self.points
    }
}

impl Default for HnswIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorIndex for HnswIndex {
    fn upsert(&mut self, document: VectorDocument) {
        self.points
            .retain(|point| point.chunk_id != document.chunk_id);
        self.points.push(document);
        self.map = None;
    }

    fn delete_source(&mut self, source_id: &SourceId) -> Result<()> {
        self.points.retain(|point| point.source_id != *source_id);
        self.build()
    }

    fn search(&self, query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)> {
        HnswIndex::search(self, query, top_k)
    }

    fn rebuild_from_store(&mut self, store: &Store) -> Result<()> {
        self.rebuild_from_store_for_profile(store, &EmbeddingProfileId::default_profile())
    }

    fn len(&self) -> usize {
        HnswIndex::len(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_vec(dim: usize, seed: u64) -> Vec<f32> {
        (0..dim)
            .map(|i| (seed as f32 * 0.1 + i as f32 * 0.01).sin())
            .collect()
    }

    #[test]
    fn build_and_search() {
        let mut index = HnswIndex::new();
        for i in 0..10u64 {
            index.add(&ChunkId(format!("chunk-{i}")), seeded_vec(4, i));
        }
        index.build().unwrap();

        let query = seeded_vec(4, 3);
        let results = index.search(&query, 3);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].0 .0, "chunk-3");
    }

    #[test]
    fn empty_index_search() {
        let index = HnswIndex::new();
        let results = index.search(&[0.0, 0.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn clear_removes_previous_points() {
        let mut index = HnswIndex::new();
        index.add(&ChunkId("old".into()), seeded_vec(4, 1));
        index.build().unwrap();

        index.clear();
        index.build().unwrap();

        assert!(index.is_empty());
        assert!(index.search(&seeded_vec(4, 1), 5).is_empty());
    }

    #[test]
    fn delete_source_removes_points_from_dense_results() {
        let mut index = HnswIndex::new();
        let first = SourceId("src-1".into());
        let second = SourceId("src-2".into());
        index.add_for_source(&ChunkId("old".into()), &first, seeded_vec(4, 1));
        index.add_for_source(&ChunkId("kept".into()), &second, seeded_vec(4, 2));
        index.build().unwrap();

        index.delete_source(&first).unwrap();

        let results = index.search(&seeded_vec(4, 1), 5);
        assert_eq!(index.len(), 1);
        assert_eq!(results[0].0 .0, "kept");
    }

    #[test]
    fn save_load_roundtrip() {
        let mut index = HnswIndex::new();
        for i in 0..5u64 {
            index.add(&ChunkId(format!("c-{i}")), seeded_vec(4, i));
        }
        index.build().unwrap();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        index.save(tmp.path()).unwrap();

        // Verify the file starts with bincode magic.
        let raw = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&raw[..4], HnswIndex::BINCODE_MAGIC);

        let mut loaded = HnswIndex::new();
        loaded.load(tmp.path()).unwrap();

        assert_eq!(loaded.len(), 5);
        let query = seeded_vec(4, 2);
        let results = loaded.search(&query, 1);
        assert_eq!(results[0].0 .0, "c-2");
    }

    #[test]
    fn load_legacy_json_format() {
        let mut index = HnswIndex::new();
        for i in 0..3u64 {
            index.add(&ChunkId(format!("legacy-{i}")), seeded_vec(4, i));
        }

        // Write in legacy JSON format (no magic prefix).
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let json = serde_json::to_vec(&index.points).unwrap();
        std::fs::write(tmp.path(), json).unwrap();

        let mut loaded = HnswIndex::new();
        loaded.load(tmp.path()).unwrap();

        assert_eq!(loaded.len(), 3);
        let query = seeded_vec(4, 1);
        let results = loaded.search(&query, 1);
        assert_eq!(results[0].0 .0, "legacy-1");
    }
}
