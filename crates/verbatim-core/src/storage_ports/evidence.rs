//! EvidenceStore port — authoritative evidence and chunk units.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::{Chunk, ChunkId, EvidenceId, EvidenceUnit};

use super::common::{
    PageRequest, PageResponse, StorageAuthContext, StorageCapability, StorageGeneration,
    StorageResult,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<crate::types::SourceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<EvidenceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<ChunkId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceListRequest {
    pub auth: StorageAuthContext,
    pub filter: EvidenceFilter,
    pub page: PageRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceListResponse {
    pub page: PageResponse<EvidenceUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceGetRequest {
    pub auth: StorageAuthContext,
    pub evidence_id: EvidenceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceGetResponse {
    pub evidence: EvidenceUnit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencePutRequest {
    pub auth: StorageAuthContext,
    pub units: Vec<EvidenceUnit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<StorageGeneration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePutResponse {
    pub written: u64,
    pub generation: StorageGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkListRequest {
    pub auth: StorageAuthContext,
    pub filter: EvidenceFilter,
    pub page: PageRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkListResponse {
    pub page: PageResponse<Chunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkGetRequest {
    pub auth: StorageAuthContext,
    pub chunk_id: ChunkId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkGetResponse {
    pub chunk: Chunk,
}

/// Authoritative evidence and chunk storage port with pagination/filtering.
#[async_trait]
pub trait EvidenceStore: StorageCapability + Send + Sync {
    async fn list_evidence(
        &self,
        request: EvidenceListRequest,
    ) -> StorageResult<EvidenceListResponse>;

    async fn get_evidence(&self, request: EvidenceGetRequest)
        -> StorageResult<EvidenceGetResponse>;

    async fn put_evidence(&self, request: EvidencePutRequest)
        -> StorageResult<EvidencePutResponse>;

    async fn list_chunks(&self, request: ChunkListRequest) -> StorageResult<ChunkListResponse>;

    async fn get_chunk(&self, request: ChunkGetRequest) -> StorageResult<ChunkGetResponse>;
}
