//! BlobStore port — binary artifact storage without path leakage.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::{EvidenceId, ImageId};

use super::common::{StorageAuthContext, StorageCapability, StorageError, StorageResult};

/// Content-addressed blob identity. Adapters may map this to object storage or
/// a content-addressed local store; ports never expose filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlobId(pub String);

impl BlobId {
    pub fn new(value: impl Into<String>) -> StorageResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StorageError::invalid_request("blob id must not be empty"));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPutRequest {
    pub auth: StorageAuthContext,
    pub blob_id: BlobId,
    pub content_type: String,
    pub bytes: Vec<u8>,
    /// Optional logical linkage (image/evidence) without path leakage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<ImageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPutResponse {
    pub blob_id: BlobId,
    pub content_hash: String,
    pub byte_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobGetRequest {
    pub auth: StorageAuthContext,
    pub blob_id: BlobId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobGetResponse {
    pub blob_id: BlobId,
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobDeleteRequest {
    pub auth: StorageAuthContext,
    pub blob_id: BlobId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobHeadRequest {
    pub auth: StorageAuthContext,
    pub blob_id: BlobId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobHeadResponse {
    pub blob_id: BlobId,
    pub content_type: String,
    pub content_hash: String,
    pub byte_len: u64,
}

/// Authoritative binary blob storage. Paths and local filenames stay inside
/// adapters.
#[async_trait]
pub trait BlobStore: StorageCapability + Send + Sync {
    async fn put_blob(&self, request: BlobPutRequest) -> StorageResult<BlobPutResponse>;

    async fn get_blob(&self, request: BlobGetRequest) -> StorageResult<BlobGetResponse>;

    async fn head_blob(&self, request: BlobHeadRequest) -> StorageResult<BlobHeadResponse>;

    async fn delete_blob(&self, request: BlobDeleteRequest) -> StorageResult<BlobDeleteResponse>;
}
