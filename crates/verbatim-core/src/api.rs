//! HTTP API payloads shared by the daemon and thin CLI.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::ConfigReloadMetadata;
use crate::task::{TaskEvent, TaskSpan, TaskSummary};
use crate::types::{
    BBox, ImageArtifact, RetrievalDebug, RetrievalProvenance, SourceIngestDiagnostics,
    SourceLocator,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceResponse {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceResponse {
    pub id: String,
    pub path: String,
    pub status: String,
    pub hash: String,
    pub parser_used: Option<String>,
    pub last_ingested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<SourceIngestDiagnostics>,
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
pub struct ReindexRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default)]
    pub vectors_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReindexResponse {
    pub reindexed: usize,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default)]
    pub vectors_only: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default)]
    pub show_retrieval: bool,
    #[serde(default)]
    pub context_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskResponse {
    pub answer: String,
    #[serde(default)]
    pub citations: Vec<CitationResponse>,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<RetrievalDebug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<RetrieveResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(default)]
    pub fast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dense_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bm25_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_top_n: Option<usize>,
    #[serde(default)]
    pub include_debug: bool,
    #[serde(default)]
    pub include_locator: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrieveResponse {
    pub task_id: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub embedding_profile_id: String,
    pub limit: usize,
    pub page_size: usize,
    pub page: usize,
    pub total_results: usize,
    pub returned_results: usize,
    pub controls: RetrieveControlsResponse,
    #[serde(default)]
    pub timings: Vec<RetrieveTimingResponse>,
    #[serde(default)]
    pub results: Vec<RetrieveResultResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<RetrievalDebug>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveControlsResponse {
    pub fast: bool,
    pub rerank_enabled: bool,
    pub dense_top_k: usize,
    pub bm25_top_k: usize,
    pub rrf_k: usize,
    pub rerank_top_n: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveTimingResponse {
    pub phase: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrieveResultResponse {
    pub index: usize,
    pub rank: usize,
    pub label: String,
    pub evidence_id: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub chunk_id: String,
    pub kind: String,
    pub role: String,
    pub score: f32,
    pub locator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_locator: Option<SourceLocator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<RetrievalProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    pub snippet: String,
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
    pub structured_locator: SourceLocator,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_failure: Option<serde_json::Value>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigResponse {
    pub config: Value,
    pub reload: ConfigReloadMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_request_accepts_context_only_flag() {
        let request: AskRequest = serde_json::from_value(serde_json::json!({
            "question": "What is cited?",
            "context_only": true
        }))
        .unwrap();

        assert!(request.context_only);
        assert!(!request.show_retrieval);
    }

    #[test]
    fn retrieve_request_defaults_to_context_only_compact_output() {
        let request: RetrieveRequest =
            serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();

        assert_eq!(request.question, "What is cited?");
        assert!(request.source_id.is_none());
        assert!(request.limit.is_none());
        assert!(request.page_size.is_none());
        assert!(!request.fast);
        assert!(request.rerank.is_none());
        assert!(!request.include_debug);
        assert!(!request.include_locator);
    }

    #[test]
    fn retrieve_result_omits_structured_locator_until_requested() {
        let response = RetrieveResponse {
            task_id: "task-1".into(),
            query: "What is cited?".into(),
            source_id: None,
            embedding_profile_id: "default".into(),
            limit: 12,
            page_size: 1,
            page: 1,
            total_results: 1,
            returned_results: 1,
            controls: RetrieveControlsResponse {
                fast: false,
                rerank_enabled: false,
                dense_top_k: 80,
                bm25_top_k: 50,
                rrf_k: 60,
                rerank_top_n: 12,
            },
            timings: vec![RetrieveTimingResponse {
                phase: "retrieval".into(),
                duration_ms: 7,
            }],
            results: vec![RetrieveResultResponse {
                index: 0,
                rank: 1,
                label: "E1".into(),
                evidence_id: "ev-1".into(),
                source_id: "src-1".into(),
                source_path: Some("/tmp/doc.md".into()),
                chunk_id: "chunk-1".into(),
                kind: "text".into(),
                role: "original_text".into(),
                score: 0.03,
                locator: "/tmp/doc.md L1".into(),
                structured_locator: None,
                provenance: None,
                derived_from: None,
                snippet: "compact cited text".into(),
            }],
            debug: None,
        };

        let encoded = serde_json::to_string(&response).unwrap();

        assert!(encoded.contains("\"locator\""));
        assert!(!encoded.contains("structured_locator"));
        assert!(!encoded.contains("provenance"));
        assert!(!encoded.contains("debug"));
    }
}
