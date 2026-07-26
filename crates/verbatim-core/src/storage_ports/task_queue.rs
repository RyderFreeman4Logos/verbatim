//! TaskQueue port — durable async task enqueue / claim / status.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::task::{TaskId, TaskKind, TaskStatus, TaskSummary};

use super::common::{
    DurationMillis, PageRequest, PageResponse, StorageAuthContext, StorageCapability, StorageResult,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEnqueueRequest {
    pub auth: StorageAuthContext,
    pub kind: TaskKind,
    /// Bounded request metadata (never raw prompts).
    pub request: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEnqueueResponse {
    pub task_id: TaskId,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClaimRequest {
    pub auth: StorageAuthContext,
    pub kind: TaskKind,
    /// Maximum tasks to claim in one call.
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<DurationMillis>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskClaimResponse {
    pub tasks: Vec<TaskSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGetRequest {
    pub auth: StorageAuthContext,
    pub task_id: TaskId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskGetResponse {
    pub task: TaskSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskFinishRequest {
    pub auth: StorageAuthContext,
    pub task_id: TaskId,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFinishResponse {
    pub task_id: TaskId,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListRequest {
    pub auth: StorageAuthContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<TaskKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    pub page: PageRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskListResponse {
    pub page: PageResponse<TaskSummary>,
}

/// Durable task queue port. Queue claim and terminalization stay server-side.
#[async_trait]
pub trait TaskQueue: StorageCapability + Send + Sync {
    async fn enqueue(&self, request: TaskEnqueueRequest) -> StorageResult<TaskEnqueueResponse>;

    async fn claim(&self, request: TaskClaimRequest) -> StorageResult<TaskClaimResponse>;

    async fn get_task(&self, request: TaskGetRequest) -> StorageResult<TaskGetResponse>;

    async fn finish(&self, request: TaskFinishRequest) -> StorageResult<TaskFinishResponse>;

    async fn list_tasks(&self, request: TaskListRequest) -> StorageResult<TaskListResponse>;
}
