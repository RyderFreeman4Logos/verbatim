//! Shared storage-port types: schema, auth, pagination, errors, capability discovery.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::auth::{Principal, Role};

/// Wire schema version for port documents and capability descriptors.
/// Unknown versions fail closed on decode.
pub const STORAGE_PORTS_SCHEMA_VERSION: u32 = 1;

/// Authorization and correlation context carried on every port call.
///
/// Ports never accept raw bearer tokens or filesystem principals — callers
/// authenticate first and pass the resolved [`Principal`] plus optional scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageAuthContext {
    /// Wire schema version. Must equal [`STORAGE_PORTS_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Authenticated caller identity (principal kind only; no secrets).
    pub principal: StoragePrincipal,
    /// Optional collection/ACL scope the caller is operating under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl_scope: Option<String>,
    /// Optional end-to-end request correlation id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl StorageAuthContext {
    pub fn new(principal: StoragePrincipal) -> Self {
        Self {
            schema_version: STORAGE_PORTS_SCHEMA_VERSION,
            principal,
            acl_scope: None,
            request_id: None,
        }
    }

    pub fn with_acl_scope(mut self, scope: impl Into<String>) -> Self {
        self.acl_scope = Some(scope.into());
        self
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    pub fn from_principal(principal: &Principal) -> Self {
        Self::new(StoragePrincipal::from_principal(principal))
    }

    pub fn validate(&self) -> StorageResult<()> {
        validate_schema_version(self.schema_version)?;
        if let Some(scope) = &self.acl_scope {
            if scope.trim().is_empty() {
                return Err(StorageError::invalid_request("acl_scope must not be empty"));
            }
        }
        if let Some(request_id) = &self.request_id {
            if request_id.trim().is_empty() {
                return Err(StorageError::invalid_request(
                    "request_id must not be empty when set",
                ));
            }
        }
        Ok(())
    }
}

/// Wire-safe principal descriptor (no tokens, no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StoragePrincipal {
    LocalAnonymous,
    Token { role: String },
}

impl StoragePrincipal {
    pub fn from_principal(principal: &Principal) -> Self {
        match principal {
            Principal::LocalAnonymous => Self::LocalAnonymous,
            Principal::Token { role } => Self::Token {
                role: role_wire_name(*role).to_string(),
            },
        }
    }
}

fn role_wire_name(role: Role) -> &'static str {
    match role {
        Role::Reader => "reader",
        Role::Editor => "editor",
        Role::Admin => "admin",
    }
}

/// Opaque, backend-neutral page cursor. Never a raw SQL offset string contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PageCursor(pub String);

impl PageCursor {
    pub fn new(value: impl Into<String>) -> StorageResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "page cursor must not be empty",
            ));
        }
        Ok(Self(value))
    }
}

/// Pagination controls shared by list/search ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    /// Maximum items to return. Must be > 0.
    pub limit: u32,
    /// Opaque cursor for the next page, when continuing a prior response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
}

impl PageRequest {
    pub fn new(limit: u32) -> StorageResult<Self> {
        if limit == 0 {
            return Err(StorageError::invalid_request("page limit must be > 0"));
        }
        Ok(Self {
            limit,
            cursor: None,
        })
    }

    pub fn with_cursor(mut self, cursor: PageCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.limit == 0 {
            return Err(StorageError::invalid_request("page limit must be > 0"));
        }
        Ok(())
    }
}

/// Paginated response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageResponse<T> {
    pub items: Vec<T>,
    /// Cursor for the next page, when more results may exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
    /// Optional total count when cheap to compute; never required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_hint: Option<u64>,
}

impl<T> PageResponse<T> {
    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: None,
            total_hint: Some(0),
        }
    }

    pub fn single_page(items: Vec<T>) -> Self {
        let total_hint = Some(items.len() as u64);
        Self {
            items,
            next_cursor: None,
            total_hint,
        }
    }
}

/// Monotonic publication / index generation fence.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct StorageGeneration(pub u64);

impl StorageGeneration {
    pub const INITIAL: Self = Self(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for StorageGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Wire-friendly duration (milliseconds). Avoids serde Duration quirks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationMillis(pub u64);

impl DurationMillis {
    pub fn as_duration(self) -> Duration {
        Duration::from_millis(self.0)
    }

    pub fn from_duration(duration: Duration) -> Self {
        Self(duration.as_millis() as u64)
    }
}

/// Typed storage port errors. Adapters must map backend failures into these
/// classes; ports never surface SQL, rusqlite, or filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "class")]
pub enum StorageError {
    /// Operation exceeded the caller or adapter timeout budget.
    Timeout {
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Concurrent write / optimistic-lock conflict.
    Conflict {
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Caller generation is behind the authoritative publication generation.
    StaleGeneration {
        expected: StorageGeneration,
        actual: StorageGeneration,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Capability or operation is not implemented by this adapter.
    Unsupported {
        capability: StorageCapabilityKind,
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Requested entity does not exist.
    NotFound { resource: String, id: String },
    /// Caller is not authorized for the requested operation/scope.
    Unauthorized {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Request failed structural/semantic validation before IO.
    InvalidRequest { detail: String },
    /// Backend temporarily unavailable (circuit open, restarting, …).
    Unavailable {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl StorageError {
    pub fn timeout(operation: impl Into<String>) -> Self {
        Self::Timeout {
            operation: operation.into(),
            detail: None,
        }
    }

    pub fn conflict(resource: impl Into<String>) -> Self {
        Self::Conflict {
            resource: resource.into(),
            detail: None,
        }
    }

    pub fn stale_generation(expected: StorageGeneration, actual: StorageGeneration) -> Self {
        Self::StaleGeneration {
            expected,
            actual,
            detail: None,
        }
    }

    pub fn unsupported(capability: StorageCapabilityKind, operation: impl Into<String>) -> Self {
        Self::Unsupported {
            capability,
            operation: operation.into(),
            detail: None,
        }
    }

    pub fn not_found(resource: impl Into<String>, id: impl Into<String>) -> Self {
        Self::NotFound {
            resource: resource.into(),
            id: id.into(),
        }
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::Unauthorized {
            detail: Some(detail.into()),
        }
    }

    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::InvalidRequest {
            detail: detail.into(),
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::Unavailable {
            detail: Some(detail.into()),
        }
    }

    /// Stable class name for metrics / redacted logs.
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "timeout",
            Self::Conflict { .. } => "conflict",
            Self::StaleGeneration { .. } => "stale_generation",
            Self::Unsupported { .. } => "unsupported",
            Self::NotFound { .. } => "not_found",
            Self::Unauthorized { .. } => "unauthorized",
            Self::InvalidRequest { .. } => "invalid_request",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout { operation, detail } => {
                write!(f, "storage timeout during {operation}")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Conflict { resource, detail } => {
                write!(f, "storage conflict on {resource}")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::StaleGeneration {
                expected,
                actual,
                detail,
            } => {
                write!(
                    f,
                    "stale storage generation: expected {expected}, actual {actual}"
                )?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Unsupported {
                capability,
                operation,
                detail,
            } => {
                write!(
                    f,
                    "unsupported storage capability {capability:?} for operation {operation}"
                )?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::NotFound { resource, id } => {
                write!(f, "storage resource {resource} not found: {id}")
            }
            Self::Unauthorized { detail } => {
                write!(f, "storage unauthorized")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::InvalidRequest { detail } => write!(f, "invalid storage request: {detail}"),
            Self::Unavailable { detail } => {
                write!(f, "storage unavailable")?;
                if let Some(detail) = detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for StorageError {}

/// Result alias for all storage port operations.
pub type StorageResult<T> = Result<T, StorageError>;

pub(crate) fn validate_schema_version(schema_version: u32) -> StorageResult<()> {
    if schema_version != STORAGE_PORTS_SCHEMA_VERSION {
        return Err(StorageError::invalid_request(format!(
            "unsupported storage ports schema version {schema_version}; expected {STORAGE_PORTS_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

/// Named capability classes exposed by storage adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageCapabilityKind {
    CatalogStore,
    EvidenceStore,
    BlobStore,
    TaskQueue,
    LexicalSearch,
    VectorSearch,
    GraphSearch,
    IndexPublisher,
}

impl StorageCapabilityKind {
    pub const ALL: [Self; 8] = [
        Self::CatalogStore,
        Self::EvidenceStore,
        Self::BlobStore,
        Self::TaskQueue,
        Self::LexicalSearch,
        Self::VectorSearch,
        Self::GraphSearch,
        Self::IndexPublisher,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CatalogStore => "catalog_store",
            Self::EvidenceStore => "evidence_store",
            Self::BlobStore => "blob_store",
            Self::TaskQueue => "task_queue",
            Self::LexicalSearch => "lexical_search",
            Self::VectorSearch => "vector_search",
            Self::GraphSearch => "graph_search",
            Self::IndexPublisher => "index_publisher",
        }
    }

    /// Whether this capability is authoritative source-of-truth data.
    pub fn is_authoritative(self) -> bool {
        matches!(
            self,
            Self::CatalogStore | Self::EvidenceStore | Self::BlobStore | Self::TaskQueue
        )
    }

    /// Whether this capability is derived / rebuildable from authoritative data.
    pub fn is_derived(self) -> bool {
        matches!(
            self,
            Self::LexicalSearch | Self::VectorSearch | Self::GraphSearch | Self::IndexPublisher
        )
    }
}

/// Capability descriptor returned by discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageCapabilityDescriptor {
    pub schema_version: u32,
    pub capabilities: BTreeSet<StorageCapabilityKind>,
    /// Optional human-readable backend label (e.g. "in_process_sqlite").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_label: Option<String>,
}

impl StorageCapabilityDescriptor {
    pub fn new(capabilities: impl IntoIterator<Item = StorageCapabilityKind>) -> Self {
        Self {
            schema_version: STORAGE_PORTS_SCHEMA_VERSION,
            capabilities: capabilities.into_iter().collect(),
            backend_label: None,
        }
    }

    pub fn with_backend_label(mut self, label: impl Into<String>) -> Self {
        self.backend_label = Some(label.into());
        self
    }

    pub fn supports(&self, capability: StorageCapabilityKind) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn validate(&self) -> StorageResult<()> {
        validate_schema_version(self.schema_version)?;
        if let Some(label) = &self.backend_label {
            if label.trim().is_empty() {
                return Err(StorageError::invalid_request(
                    "backend_label must not be empty when set",
                ));
            }
        }
        Ok(())
    }
}

/// Capability discovery surface. Every storage facade must implement this so
/// unsupported operations fail as typed [`StorageError::Unsupported`] rather
/// than panicking or silently no-opping.
pub trait StorageCapability: Send + Sync {
    fn capability_descriptor(&self) -> StorageCapabilityDescriptor;

    fn supports(&self, capability: StorageCapabilityKind) -> bool {
        self.capability_descriptor().supports(capability)
    }

    fn require(&self, capability: StorageCapabilityKind, operation: &str) -> StorageResult<()> {
        if self.supports(capability) {
            Ok(())
        } else {
            Err(StorageError::unsupported(capability, operation))
        }
    }
}

/// Publication manifest describing an atomically published index generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationManifest {
    pub schema_version: u32,
    pub generation: StorageGeneration,
    pub profile_id: Option<crate::types::EmbeddingProfileId>,
    /// Content checksum of the published index snapshot (hex SHA-256).
    pub checksum: String,
    /// Wall-clock publish timestamp (RFC3339 / unix-ms string; adapter-defined).
    pub published_at: String,
    /// Optional logical labels (e.g. "lexical", "vector", "graph").
    #[serde(default)]
    pub components: Vec<String>,
}

impl PublicationManifest {
    pub fn new(
        generation: StorageGeneration,
        checksum: impl Into<String>,
        published_at: impl Into<String>,
    ) -> StorageResult<Self> {
        let checksum = checksum.into();
        let published_at = published_at.into();
        if checksum.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "publication checksum must not be empty",
            ));
        }
        if published_at.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "publication published_at must not be empty",
            ));
        }
        Ok(Self {
            schema_version: STORAGE_PORTS_SCHEMA_VERSION,
            generation,
            profile_id: None,
            checksum,
            published_at,
            components: Vec::new(),
        })
    }

    pub fn with_profile(mut self, profile_id: crate::types::EmbeddingProfileId) -> Self {
        self.profile_id = Some(profile_id);
        self
    }

    pub fn with_components(mut self, components: impl IntoIterator<Item = String>) -> Self {
        self.components = components.into_iter().collect();
        self
    }

    pub fn validate(&self) -> StorageResult<()> {
        validate_schema_version(self.schema_version)?;
        if self.checksum.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "publication checksum must not be empty",
            ));
        }
        if self.published_at.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "publication published_at must not be empty",
            ));
        }
        Ok(())
    }
}

pub fn decode_auth_context_json(bytes: &[u8]) -> StorageResult<StorageAuthContext> {
    let value: StorageAuthContext = serde_json::from_slice(bytes)
        .map_err(|err| StorageError::invalid_request(format!("auth context decode: {err}")))?;
    value.validate()?;
    Ok(value)
}

pub fn decode_capability_descriptor_json(
    bytes: &[u8],
) -> StorageResult<StorageCapabilityDescriptor> {
    let value: StorageCapabilityDescriptor = serde_json::from_slice(bytes).map_err(|err| {
        StorageError::invalid_request(format!("capability descriptor decode: {err}"))
    })?;
    value.validate()?;
    Ok(value)
}

pub fn decode_publication_manifest_json(bytes: &[u8]) -> StorageResult<PublicationManifest> {
    let value: PublicationManifest = serde_json::from_slice(bytes).map_err(|err| {
        StorageError::invalid_request(format!("publication manifest decode: {err}"))
    })?;
    value.validate()?;
    Ok(value)
}
