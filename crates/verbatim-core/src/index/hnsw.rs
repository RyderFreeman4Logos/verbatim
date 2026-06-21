use std::path::Path;

use anyhow::{Context, Result};
use instant_distance::{Builder, HnswMap, Point, Search};
use serde::{Deserialize, Serialize};

use crate::types::ChunkId;

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
    points: Vec<(ChunkId, VecPoint)>,
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
        self.points.push((chunk_id.clone(), VecPoint(vector)));
        self.map = None;
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
        for (k, v) in &self.points {
            values.push(v.clone());
            keys.push(k.clone());
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

    pub fn save(&self, path: &Path) -> Result<()> {
        let data = serde_json::to_vec(&self.points).context("serialize HNSW data")?;
        std::fs::write(path, data).context("write HNSW index")?;
        Ok(())
    }

    pub fn load(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let data = std::fs::read(path).context("read HNSW index")?;
        self.points = serde_json::from_slice(&data).context("deserialize HNSW data")?;
        self.build()?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

impl Default for HnswIndex {
    fn default() -> Self {
        Self::new()
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
    fn save_load_roundtrip() {
        let mut index = HnswIndex::new();
        for i in 0..5u64 {
            index.add(&ChunkId(format!("c-{i}")), seeded_vec(4, i));
        }
        index.build().unwrap();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        index.save(tmp.path()).unwrap();

        let mut loaded = HnswIndex::new();
        loaded.load(tmp.path()).unwrap();

        assert_eq!(loaded.len(), 5);
        let query = seeded_vec(4, 2);
        let results = loaded.search(&query, 1);
        assert_eq!(results[0].0 .0, "c-2");
    }
}
