use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EvidenceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChunkId(pub String);

pub const DEFAULT_EMBEDDING_PROFILE_ID: &str = "default";
pub const MAX_EMBEDDING_PROFILE_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingProfileIdError {
    value: String,
}

impl EmbeddingProfileIdError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl fmt::Display for EmbeddingProfileIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "embedding profile id must be 1-{MAX_EMBEDDING_PROFILE_ID_LEN} characters of ASCII letters, digits, '.', '_', or '-' and must not be '.' or '..': {}",
            self.value
        )
    }
}

impl Error for EmbeddingProfileIdError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EmbeddingProfileId(String);

impl EmbeddingProfileId {
    pub fn new(value: impl Into<String>) -> Result<Self, EmbeddingProfileIdError> {
        let value = value.into();
        if is_valid_embedding_profile_id(&value) {
            Ok(Self(value))
        } else {
            Err(EmbeddingProfileIdError::new(value))
        }
    }

    pub fn default_profile() -> Self {
        Self(DEFAULT_EMBEDDING_PROFILE_ID.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl Default for EmbeddingProfileId {
    fn default() -> Self {
        Self::default_profile()
    }
}

impl fmt::Display for EmbeddingProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for EmbeddingProfileId {
    type Error = EmbeddingProfileIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for EmbeddingProfileId {
    type Error = EmbeddingProfileIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for EmbeddingProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn is_valid_embedding_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_EMBEDDING_PROFILE_ID_LEN
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceEmbeddingStatus {
    Pending,
    Embedded,
    Failed,
    Stale,
}

impl SourceEmbeddingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Embedded => "Embedded",
            Self::Failed => "Failed",
            Self::Stale => "Stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    GeneratedEntity,
    GeneratedClaim,
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
            Self::GeneratedEntity => "GeneratedEntity",
            Self::GeneratedClaim => "GeneratedClaim",
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
    GeneratedDependsOn,
    GeneratedImplements,
    GeneratedMentions,
    GeneratedConflictsWith,
    GeneratedSupports,
    GeneratedOther,
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
            Self::GeneratedDependsOn => "generated_depends_on",
            Self::GeneratedImplements => "generated_implements",
            Self::GeneratedMentions => "generated_mentions",
            Self::GeneratedConflictsWith => "generated_conflicts_with",
            Self::GeneratedSupports => "generated_supports",
            Self::GeneratedOther => "generated_other",
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

/// Markdown block category used by structured locators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownBlockKind {
    Paragraph,
    BlockQuote,
    ListItem,
    CodeBlock,
}

impl MarkdownBlockKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::BlockQuote => "block_quote",
            Self::ListItem => "list_item",
            Self::CodeBlock => "code_block",
        }
    }
}

/// One heading in a Markdown heading hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownHeadingLocator {
    /// Heading level from one to six.
    pub level: u32,
    /// Heading text after inline Markdown is rendered to plain text.
    pub text: String,
    /// Deterministic document-local slug. Duplicate headings receive numeric suffixes.
    pub slug: String,
    /// One-based source line containing the heading marker.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SourceLocator {
    Pdf {
        page: u32,
        paragraph: u32,
        bbox: Option<BBox>,
    },
    PdfOcr {
        page: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_label: Option<String>,
        line_index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        word_index: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bbox: Option<BBox>,
        ocr: Box<OcrLocatorMetadata>,
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
    Markdown {
        path: String,
        line_start: u32,
        line_end: u32,
        byte_start: u64,
        byte_end: u64,
        block_kind: MarkdownBlockKind,
        block_index: u32,
        block_hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading_level: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading_slug: Option<String>,
        #[serde(default)]
        heading_path: Vec<MarkdownHeadingLocator>,
    },
}

impl fmt::Display for SourceLocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pdf {
                page, paragraph, ..
            } => write!(f, "PDF p.{page}, para {paragraph}"),
            Self::PdfOcr {
                page,
                line_index,
                word_index: Some(word_index),
                ocr,
                ..
            } => write!(
                f,
                "PDF p.{page}, OCR line {line_index}, word {word_index}, conf={}",
                format_confidence(ocr.confidence)
            ),
            Self::PdfOcr {
                page,
                line_index,
                ocr,
                ..
            } => write!(
                f,
                "PDF p.{page}, OCR line {line_index}, conf={}",
                format_confidence(ocr.confidence)
            ),
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
            Self::Markdown {
                path,
                line_start,
                line_end,
                block_kind,
                heading_slug,
                ..
            } => {
                if line_start == line_end {
                    write!(f, "{path} L{line_start} markdown:{}", block_kind.as_str())?;
                } else {
                    write!(
                        f,
                        "{path} L{line_start}-{line_end} markdown:{}",
                        block_kind.as_str()
                    )?;
                }
                if let Some(slug) = heading_slug {
                    write!(f, " #{slug}")?;
                }
                Ok(())
            }
        }
    }
}

fn format_confidence(confidence: Option<f32>) -> String {
    confidence
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "unknown".to_string())
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
    Ocr,
    Image,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrProfile {
    pub provider: String,
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    pub language: String,
    pub profile: String,
}

impl OcrProfile {
    pub fn profile_hash(&self) -> String {
        let encoded = serde_json::to_vec(self).unwrap_or_default();
        hex_sha256(&encoded)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrLocatorMetadata {
    pub profile: OcrProfile,
    pub profile_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub text_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfPageScanSummary {
    pub page: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_label: Option<String>,
    pub text_char_count: usize,
    pub text_density: f32,
    pub image_count: usize,
    pub has_meaningful_text: bool,
    pub image_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfScanSummary {
    pub page_count: usize,
    pub text_char_count: usize,
    pub text_density: f32,
    pub image_only_page_count: usize,
    pub ocr_recommended: bool,
    #[serde(default)]
    pub pages: Vec<PdfPageScanSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrSourceStatus {
    NotRequired,
    Disabled,
    Recommended,
    Applied,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceOcrDiagnostics {
    pub enabled: bool,
    pub status: OcrSourceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_profile: Option<OcrProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_profile_hash: Option<String>,
    pub evidence_count: usize,
    #[serde(default)]
    pub evidence_profile_hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceIngestDiagnostics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<PdfScanSummary>,
    pub ocr: SourceOcrDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Deterministic identity for this chunk's content and structural context.
    pub chunk_hash: String,
    /// Hash of the exact embedding input for the active embedding profile, when embedded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_input_hash: Option<String>,
    pub text: String,
    pub context_text: Option<String>,
    pub token_count: u32,
    pub chunk_type: ChunkType,
    pub parent_chunk_id: Option<ChunkId>,
    pub heading_path: Vec<String>,
    pub evidence_unit_ids: Vec<EvidenceId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingCacheStats {
    pub cache_hits: usize,
    pub cache_misses: usize,
    pub embedded_chunks: usize,
    pub reused_chunks: usize,
    pub changed_chunks: usize,
}

impl EmbeddingCacheStats {
    pub fn add(&mut self, other: &Self) {
        self.cache_hits += other.cache_hits;
        self.cache_misses += other.cache_misses;
        self.embedded_chunks += other.embedded_chunks;
        self.reused_chunks += other.reused_chunks;
        self.changed_chunks += other.changed_chunks;
    }

    pub fn is_empty(&self) -> bool {
        self.cache_hits == 0
            && self.cache_misses == 0
            && self.embedded_chunks == 0
            && self.reused_chunks == 0
            && self.changed_chunks == 0
    }
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
    #[serde(default)]
    pub dense_vector_path: RetrievalDenseVectorPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_embedding_latency_ms: Option<u64>,
    pub bm25_hits: Vec<RetrievalStageHit>,
    pub dense_hits: Vec<RetrievalStageHit>,
    pub rrf_fused_hits: Vec<RetrievalFusedHit>,
    pub graph_expanded_hits: Vec<RetrievalGraphExpansionDebug>,
    pub reranker: RetrievalRerankDebug,
    pub final_evidence_pack: Vec<RetrievalEvidencePackEntry>,
}

/// Local dense vector residency policy for the daemon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorIndexResidency {
    /// Keep vectors in SQLite and scan stored vectors at query time.
    #[default]
    LowMemory,
    /// Load the published local HNSW index into daemon memory.
    ResidentHnsw,
}

/// Actual local dense vector path used for one retrieval debug result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalDenseVectorPath {
    /// Dense retrieval was disabled, so BM25 supplied the candidates.
    Bm25Only,
    /// Dense retrieval scanned SQLite-stored vectors without resident HNSW.
    #[default]
    LowMemorySqliteScan,
    /// Dense retrieval used the resident local HNSW index.
    ResidentHnsw,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_n: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<RetrievalRerankCapabilityDebug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RetrievalRerankRequestDebug>,
    pub scores: Vec<RetrievalRerankScore>,
}

impl RetrievalRerankDebug {
    pub fn disabled() -> Self {
        Self {
            status: RetrievalRerankStatus::Disabled,
            reason: None,
            provider: None,
            model: None,
            top_n: None,
            candidate_count: None,
            latency_ms: None,
            capability: None,
            request: None,
            scores: Vec::new(),
        }
    }

    pub fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: RetrievalRerankStatus::Skipped,
            reason: Some(bounded_debug_text(&reason.into())),
            provider: None,
            model: None,
            top_n: None,
            candidate_count: None,
            latency_ms: None,
            capability: None,
            request: None,
            scores: Vec::new(),
        }
    }

    pub fn succeeded(
        provider: impl Into<String>,
        model: impl Into<String>,
        top_n: usize,
        candidate_count: usize,
        scores: Vec<RetrievalRerankScore>,
    ) -> Self {
        Self {
            status: RetrievalRerankStatus::Succeeded,
            reason: None,
            provider: Some(bounded_debug_text(&provider.into())),
            model: Some(bounded_debug_text(&model.into())),
            top_n: Some(top_n),
            candidate_count: Some(candidate_count),
            latency_ms: None,
            capability: None,
            request: None,
            scores,
        }
    }

    pub fn fallback(
        provider: impl Into<String>,
        model: impl Into<String>,
        top_n: usize,
        candidate_count: usize,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            status: RetrievalRerankStatus::Fallback,
            reason: Some(bounded_debug_text(&reason.into())),
            provider: Some(bounded_debug_text(&provider.into())),
            model: Some(bounded_debug_text(&model.into())),
            top_n: Some(top_n),
            candidate_count: Some(candidate_count),
            latency_ms: None,
            capability: None,
            request: None,
            scores: Vec::new(),
        }
    }

    pub fn with_latency_ms(mut self, latency_ms: u64) -> Self {
        self.latency_ms = Some(latency_ms);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalRerankCapabilityDebug {
    pub state: RetrievalRerankCapabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_candidates: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_documents: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_document_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_payload_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub retried_after_context_limit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalRerankCapabilityState {
    Cached,
    Refreshed,
    Unavailable,
    RefreshFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalRerankRequestDebug {
    pub candidate_count: usize,
    pub document_char_limit: usize,
    pub top_n: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalRerankStatus {
    Disabled,
    Skipped,
    Succeeded,
    Fallback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalRerankScore {
    pub rank: usize,
    pub chunk_id: ChunkId,
    pub score: f32,
}

fn bounded_debug_text(input: &str) -> String {
    const MAX_DEBUG_TEXT_CHARS: usize = 96;
    let mut output = String::new();
    for ch in input.chars().take(MAX_DEBUG_TEXT_CHARS) {
        output.push(ch);
    }
    output
}

fn is_false(value: &bool) -> bool {
    !*value
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
    OcrText,
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
    fn mvp_regression_source_ids_include_path_hash_to_avoid_stem_collisions() {
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
            dense_vector_path: RetrievalDenseVectorPath::ResidentHnsw,
            query_embedding_latency_ms: None,
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

    #[test]
    fn rerank_debug_metadata_serializes_without_request_text_or_secrets() {
        let mut debug =
            RetrievalRerankDebug::fallback("vllm", "rerank-model", 1, 1, "http_status_400");
        debug.capability = Some(RetrievalRerankCapabilityDebug {
            state: RetrievalRerankCapabilityState::Refreshed,
            max_context_tokens: Some(512),
            max_candidates: Some(2),
            max_documents: Some(2),
            max_document_chars: Some(1024),
            max_payload_chars: Some(4096),
            reason: Some("capability_absent".into()),
            retried_after_context_limit: true,
        });
        debug.request = Some(RetrievalRerankRequestDebug {
            candidate_count: 1,
            document_char_limit: 768,
            top_n: 1,
        });

        let encoded = serde_json::to_string(&debug).unwrap();

        assert!(encoded.contains("refreshed"));
        assert!(encoded.contains("max_document_chars"));
        assert!(encoded.contains("document_char_limit"));
        assert!(!encoded.contains("Authorization"));
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("api_key"));
        assert!(!encoded.contains("secret query token=fixture-query"));
        assert!(!encoded.contains("secret document body"));
    }
}
