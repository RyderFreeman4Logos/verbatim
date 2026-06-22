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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphNodeId(pub String);

impl GraphNodeId {
    pub fn new(source_id: &SourceId, kind: GraphNodeKind, external_id: &str) -> Self {
        let digest =
            hex_sha256(format!("{}:{}:{external_id}", &source_id.0, kind.as_str()).as_bytes());
        Self(format!(
            "{}:graph:{}:{}",
            &source_id.0,
            kind.as_str(),
            &digest[..16]
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphEdgeId(pub String);

impl GraphEdgeId {
    pub fn new(
        source_id: &SourceId,
        edge_type: EdgeType,
        from_node_id: &GraphNodeId,
        to_node_id: &GraphNodeId,
        ordinal: Option<u32>,
    ) -> Self {
        let ordinal_key = ordinal.map(|value| value.to_string()).unwrap_or_default();
        let digest = hex_sha256(
            format!(
                "{}:{}:{}:{}:{ordinal_key}",
                &source_id.0,
                edge_type.as_str(),
                &from_node_id.0,
                &to_node_id.0
            )
            .as_bytes(),
        );
        Self(format!(
            "{}:graph-edge:{}:{}",
            &source_id.0,
            edge_type.as_str(),
            &digest[..16]
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GraphNodeKind {
    Source,
    Page,
    Section,
    Chunk,
    EvidenceUnit,
    ImageArtifact,
}

impl GraphNodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "Source",
            Self::Page => "Page",
            Self::Section => "Section",
            Self::Chunk => "Chunk",
            Self::EvidenceUnit => "EvidenceUnit",
            Self::ImageArtifact => "ImageArtifact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    Contains,
    DerivedFrom,
    Parent,
    Child,
    Previous,
    Next,
    SameSource,
    SamePage,
    SectionContains,
    PageContainsImage,
    ImageNearText,
    MarkdownLinksTo,
}

impl EdgeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::DerivedFrom => "derived_from",
            Self::Parent => "parent",
            Self::Child => "child",
            Self::Previous => "previous",
            Self::Next => "next",
            Self::SameSource => "same_source",
            Self::SamePage => "same_page",
            Self::SectionContains => "section_contains",
            Self::PageContainsImage => "page_contains_image",
            Self::ImageNearText => "image_near_text",
            Self::MarkdownLinksTo => "markdown_links_to",
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub source_id: SourceId,
    pub kind: GraphNodeKind,
    pub external_id: String,
    pub label: Option<String>,
    pub locator: Option<SourceLocator>,
    pub ordinal: Option<u32>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: GraphEdgeId,
    pub source_id: SourceId,
    pub edge_type: EdgeType,
    pub from_node_id: GraphNodeId,
    pub to_node_id: GraphNodeId,
    pub ordinal: Option<u32>,
    pub weight: Option<f64>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
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
    #[serde(default)]
    pub provenance: RetrievalProvenance,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalOrigin {
    #[default]
    Seed,
    GraphExpansion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphTraversalDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphExpansionStep {
    pub edge_type: EdgeType,
    pub from_node_id: GraphNodeId,
    pub to_node_id: GraphNodeId,
    pub direction: GraphTraversalDirection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalProvenance {
    #[serde(default)]
    pub origin: RetrievalOrigin,
    #[serde(default)]
    pub result_rank: usize,
    #[serde(default)]
    pub seed_rank: Option<usize>,
    #[serde(default)]
    pub seed_chunk_id: Option<ChunkId>,
    #[serde(default)]
    pub seed_source_id: Option<SourceId>,
    #[serde(default)]
    pub hop_distance: u32,
    #[serde(default)]
    pub graph_path: Vec<GraphExpansionStep>,
}

impl RetrievalProvenance {
    pub fn seed(result_rank: usize, chunk_id: ChunkId, source_id: SourceId) -> Self {
        Self {
            origin: RetrievalOrigin::Seed,
            result_rank,
            seed_rank: Some(result_rank),
            seed_chunk_id: Some(chunk_id),
            seed_source_id: Some(source_id),
            hop_distance: 0,
            graph_path: Vec::new(),
        }
    }

    pub fn graph_expansion(
        result_rank: usize,
        seed_rank: usize,
        seed_chunk_id: ChunkId,
        seed_source_id: SourceId,
        hop_distance: u32,
        graph_path: Vec<GraphExpansionStep>,
    ) -> Self {
        Self {
            origin: RetrievalOrigin::GraphExpansion,
            result_rank,
            seed_rank: Some(seed_rank),
            seed_chunk_id: Some(seed_chunk_id),
            seed_source_id: Some(seed_source_id),
            hop_distance,
            graph_path,
        }
    }
}

impl Default for RetrievalProvenance {
    fn default() -> Self {
        Self {
            origin: RetrievalOrigin::Seed,
            result_rank: 0,
            seed_rank: None,
            seed_chunk_id: None,
            seed_source_id: None,
            hop_distance: 0,
            graph_path: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalDebug {
    pub bm25_hits: Vec<RetrievalStageHit>,
    pub dense_hits: Vec<RetrievalStageHit>,
    pub rrf_fused_hits: Vec<RetrievalFusedHit>,
    pub graph_expanded_hits: Vec<RetrievalGraphExpansionDebug>,
    pub reranker: RetrievalRerankDebug,
    pub final_evidence_pack: Vec<RetrievalEvidencePackEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalStageHit {
    pub rank: usize,
    pub chunk_id: ChunkId,
    pub source_id: Option<SourceId>,
    pub score: f32,
    pub evidence_ids: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalFusedHit {
    pub rank: usize,
    pub chunk_id: ChunkId,
    pub source_id: Option<SourceId>,
    pub score: f32,
    pub dense_rank: Option<usize>,
    pub bm25_rank: Option<usize>,
    pub evidence_ids: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalGraphExpansionDebug {
    pub result_rank: usize,
    pub seed_rank: usize,
    pub seed_chunk_id: ChunkId,
    pub seed_source_id: SourceId,
    pub expanded_chunk_id: ChunkId,
    pub expanded_source_id: SourceId,
    pub score: f32,
    pub hop_distance: u32,
    pub path: Vec<GraphExpansionStep>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalRerankDebug {
    pub status: RetrievalRerankStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub scores: Vec<RetrievalRerankScore>,
}

impl RetrievalRerankDebug {
    pub fn disabled() -> Self {
        Self {
            status: RetrievalRerankStatus::Disabled,
            reason: None,
            scores: Vec::new(),
        }
    }

    pub fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: RetrievalRerankStatus::Skipped,
            reason: Some(reason.into()),
            scores: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalRerankStatus {
    Disabled,
    Skipped,
    Ran,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalRerankScore {
    pub rank: usize,
    pub chunk_id: ChunkId,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalEvidencePackEntry {
    pub label: String,
    pub result_rank: usize,
    pub chunk_id: ChunkId,
    pub score: f32,
    pub evidence_id: EvidenceId,
    pub source_id: SourceId,
    pub role: RetrievalEvidenceRole,
    pub kind: EvidenceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<EvidenceId>,
    pub locator: RetrievalLocatorDebug,
    pub provenance: RetrievalProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalEvidenceRole {
    OriginalText,
    ImageArtifact,
    ImageCaptionGenerated,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalLocatorDebug {
    pub display: String,
    pub structured: SourceLocator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationRef {
    pub label: String,
    pub evidence_id: EvidenceId,
    pub source_id: SourceId,
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

    #[test]
    fn retrieval_debug_serializes_without_raw_text_or_secrets() {
        let debug = RetrievalDebug {
            bm25_hits: vec![RetrievalStageHit {
                rank: 1,
                chunk_id: ChunkId("chunk-1".into()),
                source_id: Some(SourceId("src-1".into())),
                score: 4.2,
                evidence_ids: vec![EvidenceId("ev-1".into())],
            }],
            dense_hits: Vec::new(),
            rrf_fused_hits: vec![RetrievalFusedHit {
                rank: 1,
                chunk_id: ChunkId("chunk-1".into()),
                source_id: Some(SourceId("src-1".into())),
                score: 0.03,
                dense_rank: None,
                bm25_rank: Some(1),
                evidence_ids: vec![EvidenceId("ev-1".into())],
            }],
            graph_expanded_hits: vec![RetrievalGraphExpansionDebug {
                result_rank: 2,
                seed_rank: 1,
                seed_chunk_id: ChunkId("chunk-1".into()),
                seed_source_id: SourceId("src-1".into()),
                expanded_chunk_id: ChunkId("chunk-2".into()),
                expanded_source_id: SourceId("src-1".into()),
                score: 0.01,
                hop_distance: 1,
                path: vec![GraphExpansionStep {
                    edge_type: EdgeType::Next,
                    from_node_id: GraphNodeId("node-1".into()),
                    to_node_id: GraphNodeId("node-2".into()),
                    direction: GraphTraversalDirection::Outgoing,
                }],
                reason: "included_by_configured_graph_expansion".into(),
            }],
            reranker: RetrievalRerankDebug::skipped("disabled"),
            final_evidence_pack: vec![RetrievalEvidencePackEntry {
                label: "E1".into(),
                result_rank: 1,
                chunk_id: ChunkId("chunk-1".into()),
                score: 0.03,
                evidence_id: EvidenceId("ev-1".into()),
                source_id: SourceId("src-1".into()),
                role: RetrievalEvidenceRole::OriginalText,
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: RetrievalLocatorDebug {
                    display: "/tmp/doc.txt L1".into(),
                    structured: SourceLocator::Document {
                        path_or_url: "/tmp/doc.txt".into(),
                        line_start: 1,
                        line_end: None,
                    },
                },
                provenance: RetrievalProvenance::seed(
                    1,
                    ChunkId("chunk-1".into()),
                    SourceId("src-1".into()),
                ),
            }],
        };

        let encoded = serde_json::to_string(&debug).unwrap();
        assert!(encoded.contains("bm25_hits"));
        assert!(encoded.contains("graph_expanded_hits"));
        assert!(encoded.contains("final_evidence_pack"));
        assert!(encoded.contains("disabled"));
        assert!(!encoded.contains("api_key"));
        assert!(!encoded.contains("secret full raw source text"));

        let decoded: RetrievalDebug = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, debug);
    }
}
