//! CatalogStore port — authoritative collection/source metadata.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::collection::{CollectionMember, CollectionRecord, CollectionRoot, CollectionStatus};
use crate::types::{Source, SourceId};

use super::common::{
    PageRequest, PageResponse, StorageAuthContext, StorageCapability, StorageGeneration,
    StorageResult,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogListSourcesRequest {
    pub auth: StorageAuthContext,
    pub page: PageRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogListSourcesResponse {
    pub page: PageResponse<Source>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogGetSourceRequest {
    pub auth: StorageAuthContext,
    pub source_id: SourceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogGetSourceResponse {
    pub source: Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogUpsertSourceRequest {
    pub auth: StorageAuthContext,
    pub source: Source,
    /// Expected generation for optimistic concurrency; omit for unconditional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<StorageGeneration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogUpsertSourceResponse {
    pub source_id: SourceId,
    pub generation: StorageGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogListCollectionsRequest {
    pub auth: StorageAuthContext,
    pub page: PageRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogListCollectionsResponse {
    pub page: PageResponse<CollectionRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogGetCollectionRequest {
    pub auth: StorageAuthContext,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogGetCollectionResponse {
    pub collection: CollectionRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CollectionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogCreateCollectionRequest {
    pub auth: StorageAuthContext,
    pub name: String,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    #[serde(default)]
    pub watch_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_index_enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogCreateCollectionResponse {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogDeleteCollectionRequest {
    pub auth: StorageAuthContext,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogDeleteCollectionResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogListRootsRequest {
    pub auth: StorageAuthContext,
    pub collection_name: String,
    pub page: PageRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogListRootsResponse {
    pub page: PageResponse<CollectionRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogListMembersRequest {
    pub auth: StorageAuthContext,
    pub collection_name: String,
    pub page: PageRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogListMembersResponse {
    pub page: PageResponse<CollectionMember>,
}

/// Authoritative catalog / collection / source metadata port.
///
/// Lifecycle: durable source-of-truth. Transaction boundaries stay inside the
/// adapter; callers issue typed operations only.
#[async_trait]
pub trait CatalogStore: StorageCapability + Send + Sync {
    async fn list_sources(
        &self,
        request: CatalogListSourcesRequest,
    ) -> StorageResult<CatalogListSourcesResponse>;

    async fn get_source(
        &self,
        request: CatalogGetSourceRequest,
    ) -> StorageResult<CatalogGetSourceResponse>;

    async fn upsert_source(
        &self,
        request: CatalogUpsertSourceRequest,
    ) -> StorageResult<CatalogUpsertSourceResponse>;

    async fn list_collections(
        &self,
        request: CatalogListCollectionsRequest,
    ) -> StorageResult<CatalogListCollectionsResponse>;

    async fn get_collection(
        &self,
        request: CatalogGetCollectionRequest,
    ) -> StorageResult<CatalogGetCollectionResponse>;

    async fn create_collection(
        &self,
        request: CatalogCreateCollectionRequest,
    ) -> StorageResult<CatalogCreateCollectionResponse>;

    async fn delete_collection(
        &self,
        request: CatalogDeleteCollectionRequest,
    ) -> StorageResult<CatalogDeleteCollectionResponse>;

    async fn list_roots(
        &self,
        request: CatalogListRootsRequest,
    ) -> StorageResult<CatalogListRootsResponse>;

    async fn list_members(
        &self,
        request: CatalogListMembersRequest,
    ) -> StorageResult<CatalogListMembersResponse>;
}
