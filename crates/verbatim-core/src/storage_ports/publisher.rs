//! IndexPublisher port — atomic index publication and manifests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::EmbeddingProfileId;

use super::common::{
    PublicationManifest, StorageAuthContext, StorageCapability, StorageGeneration, StorageResult,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexPublishRequest {
    pub auth: StorageAuthContext,
    pub manifest: PublicationManifest,
    /// Expected current generation for compare-and-swap publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_current: Option<StorageGeneration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexPublishResponse {
    pub generation: StorageGeneration,
    pub manifest: PublicationManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCurrentRequest {
    pub auth: StorageAuthContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<EmbeddingProfileId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCurrentResponse {
    pub generation: StorageGeneration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<PublicationManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGetManifestRequest {
    pub auth: StorageAuthContext,
    pub generation: StorageGeneration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<EmbeddingProfileId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGetManifestResponse {
    pub manifest: PublicationManifest,
}

/// Atomic index publication port. Publication is compare-and-swap on
/// generation; readers fence with [`StorageGeneration`].
#[async_trait]
pub trait IndexPublisher: StorageCapability + Send + Sync {
    async fn publish(&self, request: IndexPublishRequest) -> StorageResult<IndexPublishResponse>;

    async fn current(&self, request: IndexCurrentRequest) -> StorageResult<IndexCurrentResponse>;

    async fn get_manifest(
        &self,
        request: IndexGetManifestRequest,
    ) -> StorageResult<IndexGetManifestResponse>;
}
