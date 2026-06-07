use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub String);

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
