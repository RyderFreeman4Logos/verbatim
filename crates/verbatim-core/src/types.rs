use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub String);

impl SourceId {
    pub fn from_path(path: &Path) -> Self {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let path_text = canonical.to_string_lossy();
        let mut hasher = Sha256::new();
        hasher.update(path_text.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(sanitize_source_stem)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "source".to_string());
        Self(format!("{}-{}", stem, &digest[..16]))
    }
}

fn sanitize_source_stem(stem: &str) -> String {
    stem.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => c,
            '-' | '_' => c,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkId(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SourceLocator {
    Pdf {
        page: u32,
        paragraph: u32,
        bbox: Option<BBox>,
    },
    Document {
        path_or_url: String,
        line_start: u32,
        line_end: Option<u32>,
    },
}

impl fmt::Display for SourceLocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pdf {
                page, paragraph, ..
            } => write!(f, "PDF p.{page}, para {paragraph}"),
            Self::Document {
                path_or_url,
                line_start,
                line_end: Some(end),
            } => write!(f, "{path_or_url} L{line_start}-{end}"),
            Self::Document {
                path_or_url,
                line_start,
                line_end: None,
            } => write!(f, "{path_or_url} L{line_start}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceStatus {
    Pending,
    Indexed,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub path: PathBuf,
    pub hash: String,
    pub status: SourceStatus,
    pub parser_used: Option<String>,
    pub last_ingested_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceUnit {
    pub id: EvidenceId,
    pub source_id: SourceId,
    pub locator: SourceLocator,
    pub text: String,
    pub text_hash: String,
    pub heading_path: Vec<String>,
    pub position: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkType {
    Child,
    Parent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: ChunkId,
    pub source_id: SourceId,
    pub text: String,
    pub context_text: Option<String>,
    pub token_count: u32,
    pub chunk_type: ChunkType,
    pub parent_chunk_id: Option<ChunkId>,
    pub heading_path: Vec<String>,
    pub evidence_unit_ids: Vec<EvidenceId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalResult {
    pub chunk_id: ChunkId,
    pub score: f32,
    pub chunk: Chunk,
    pub evidence_units: Vec<EvidenceUnit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationRef {
    pub evidence_id: EvidenceId,
    pub locator: SourceLocator,
    pub text_preview: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ids_include_path_hash_to_avoid_stem_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let left_dir = tmp.path().join("left");
        let right_dir = tmp.path().join("right");
        std::fs::create_dir_all(&left_dir).unwrap();
        std::fs::create_dir_all(&right_dir).unwrap();
        let left = left_dir.join("notes.md");
        let right = right_dir.join("notes.md");
        std::fs::write(&left, "left").unwrap();
        std::fs::write(&right, "right").unwrap();

        let left_id = SourceId::from_path(&left);
        let right_id = SourceId::from_path(&right);

        assert_ne!(left_id, right_id);
        assert!(left_id.0.starts_with("notes-"));
        assert!(right_id.0.starts_with("notes-"));
    }
}
