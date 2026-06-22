//! HTTP API payloads shared by the daemon and thin CLI.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task::{TaskEvent, TaskSpan, TaskSummary};
use crate::types::{BBox, ImageArtifact, RetrievalDebug};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceResponse {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceResponse {
    pub id: String,
    pub path: String,
    pub status: String,
    pub hash: String,
    pub parser_used: Option<String>,
    pub last_ingested_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckStaleResponse {
    pub stale: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestResponse {
    pub ingested: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCreatedResponse {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskIngestRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSummaryResponse {
    pub task: TaskSummary,
    #[serde(default)]
    pub spans: Vec<TaskSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEventsResponse {
    #[serde(default)]
    pub events: Vec<TaskEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskWaitEvent {
    pub task: TaskSummary,
    #[serde(default)]
    pub events: Vec<TaskEvent>,
    #[serde(default)]
    pub spans: Vec<TaskSpan>,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default)]
    pub show_retrieval: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskResponse {
    pub answer: String,
    #[serde(default)]
    pub citations: Vec<CitationResponse>,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<RetrievalDebug>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationResponse {
    pub label: String,
    pub evidence_id: String,
    pub kind: String,
    pub derived_from: Option<String>,
    pub locator: String,
    pub text_preview: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceResponse {
    pub id: String,
    pub source_id: String,
    pub kind: String,
    pub derived_from: Option<String>,
    pub locator: String,
    pub text: String,
    pub heading_path: Vec<String>,
    pub position: u32,
    pub image_artifact: Option<ImageArtifactResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageArtifactResponse {
    pub image_id: String,
    pub path: String,
    pub content_hash: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub page: u32,
    pub image_index: u32,
    pub bbox: Option<BBox>,
}

impl From<ImageArtifact> for ImageArtifactResponse {
    fn from(artifact: ImageArtifact) -> Self {
        Self {
            image_id: artifact.image_id.0,
            path: artifact.relative_path.display().to_string(),
            content_hash: artifact.content_hash,
            mime_type: artifact.mime_type,
            width: artifact.width,
            height: artifact.height,
            page: artifact.page,
            image_index: artifact.image_index,
            bbox: artifact.bbox,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskTokenEvent {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskCitationEvent {
    #[serde(default)]
    pub citations: Vec<CitationResponse>,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskErrorEvent {
    pub status: Option<u16>,
    pub error: String,
}

pub type ConfigResponse = Value;
