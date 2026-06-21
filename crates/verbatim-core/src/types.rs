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
pub struct ImageId(pub String);

impl ImageId {
    pub fn for_pdf_image(
        source_id: &SourceId,
        page: u32,
        bbox: Option<&BBox>,
        image_hash: &str,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(source_id.0.as_bytes());
        hasher.update(page.to_be_bytes());
        hasher.update(canonical_bbox_key(bbox).as_bytes());
        hasher.update(image_hash.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        Self(format!("{}:img:{}", source_id.0, &digest[..16]))
    }
}

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
    PdfImage {
        page: u32,
        image_index: u32,
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
            Self::PdfImage {
                page,
                image_index,
                bbox: Some(bbox),
            } => write!(
                f,
                "PDF p.{page}, image {image_index}, bbox={}",
                format_bbox(bbox)
            ),
            Self::PdfImage {
                page,
                image_index,
                bbox: None,
            } => write!(f, "PDF p.{page}, image {image_index}"),
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

fn canonical_bbox_key(bbox: Option<&BBox>) -> String {
    bbox.map(|bbox| {
        format!(
            "{:.3},{:.3},{:.3},{:.3}",
            normalize_zero(bbox.x0),
            normalize_zero(bbox.y0),
            normalize_zero(bbox.x1),
            normalize_zero(bbox.y1)
        )
    })
    .unwrap_or_else(|| "none".to_string())
}

fn format_bbox(bbox: &BBox) -> String {
    format!(
        "[{:.2},{:.2},{:.2},{:.2}]",
        normalize_zero(bbox.x0),
        normalize_zero(bbox.y0),
        normalize_zero(bbox.x1),
        normalize_zero(bbox.y1)
    )
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceKind {
    Text,
    Image,
    Generated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceUnit {
    pub id: EvidenceId,
    pub source_id: SourceId,
    pub kind: EvidenceKind,
    #[serde(default)]
    pub derived_from: Option<EvidenceId>,
    pub locator: SourceLocator,
    pub text: String,
    pub text_hash: String,
    pub heading_path: Vec<String>,
    pub position: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedImageArtifact {
    pub page: u32,
    pub image_index: u32,
    pub bbox: Option<BBox>,
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub extension: String,
    pub width: u32,
    pub height: u32,
    pub nearby_text_before: Option<String>,
    pub nearby_text_after: Option<String>,
}

impl ParsedImageArtifact {
    pub fn content_hash(&self) -> String {
        hex_sha256(&self.bytes)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageArtifact {
    pub image_id: ImageId,
    pub source_id: SourceId,
    pub evidence_id: EvidenceId,
    pub relative_path: PathBuf,
    pub content_hash: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub page: u32,
    pub image_index: u32,
    pub bbox: Option<BBox>,
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
    pub label: String,
    pub evidence_id: EvidenceId,
    pub kind: EvidenceKind,
    #[serde(default)]
    pub derived_from: Option<EvidenceId>,
    pub locator: SourceLocator,
    pub text_preview: String,
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
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

    #[test]
    fn pdf_image_locators_include_image_index_and_bbox() {
        let locator = SourceLocator::PdfImage {
            page: 84,
            image_index: 2,
            bbox: Some(BBox {
                x0: 1.0,
                y0: 2.0,
                x1: 3.0,
                y1: 4.0,
            }),
        };

        assert_eq!(
            locator.to_string(),
            "PDF p.84, image 2, bbox=[1.00,2.00,3.00,4.00]"
        );
    }

    #[test]
    fn pdf_image_ids_are_stable_for_same_hash_and_bbox() {
        let source_id = SourceId("src".into());
        let bbox = BBox {
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };

        let first = ImageId::for_pdf_image(&source_id, 1, Some(&bbox), "hash");
        let second = ImageId::for_pdf_image(&source_id, 1, Some(&bbox), "hash");
        let different = ImageId::for_pdf_image(&source_id, 2, Some(&bbox), "hash");

        assert_eq!(first, second);
        assert_ne!(first, different);
    }
}
