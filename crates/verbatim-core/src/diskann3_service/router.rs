//! Generation-aware, pre-I/O metadata shard routing with bounded fan-out.

use super::{
    DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult, RequestIdentity,
    SearchRequest,
};

const MAX_METADATA_VALUES: usize = 64;

/// Health advertised by a shard replica before routing. No live health probe exists here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardHealth {
    Ready,
    Unavailable,
    CircuitOpen,
}

/// Metadata sufficient only to exclude non-eligible shards before vector I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardRouteMetadata {
    tenant: String,
    sources: Vec<String>,
    collections: Vec<String>,
    acls: Vec<String>,
}

impl ShardRouteMetadata {
    pub fn new(
        tenant: impl Into<String>,
        sources: Vec<String>,
        collections: Vec<String>,
        acls: Vec<String>,
    ) -> DiskAnn3ServiceResult<Self> {
        let metadata = Self {
            tenant: tenant.into(),
            sources,
            collections,
            acls,
        };
        if metadata.tenant.is_empty()
            || metadata.sources.is_empty()
            || metadata.collections.is_empty()
            || metadata.acls.is_empty()
            || !metadata.values_are_bounded()
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidShardMetadata,
            ));
        }
        Ok(metadata)
    }

    fn values_are_bounded(&self) -> bool {
        [&self.sources, &self.collections, &self.acls]
            .iter()
            .all(|values| {
                values.len() <= MAX_METADATA_VALUES
                    && values
                        .iter()
                        .all(|value| !value.is_empty() && value.len() <= 256)
            })
    }

    fn matches(&self, request: &SearchRequest) -> bool {
        self.tenant == request.predicate().tenant()
            && intersects(&self.sources, request.predicate().sources())
            && intersects(&self.collections, request.predicate().collections())
            && request
                .predicate()
                .required_acls()
                .iter()
                .all(|acl| self.acls.contains(acl))
    }
}

/// One immutable generation-bound shard descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardDescriptor {
    id: u32,
    identity: RequestIdentity,
    metadata: ShardRouteMetadata,
    health: ShardHealth,
    required: bool,
}

impl ShardDescriptor {
    pub fn new(
        id: u32,
        identity: RequestIdentity,
        metadata: ShardRouteMetadata,
        health: ShardHealth,
        required: bool,
    ) -> DiskAnn3ServiceResult<Self> {
        if id == 0 {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidShardMetadata,
            ));
        }
        Ok(Self {
            id,
            identity,
            metadata,
            health,
            required,
        })
    }

    pub const fn id(&self) -> u32 {
        self.id
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
}

/// A manifest contains only one immutable identity/generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardManifest {
    identity: RequestIdentity,
    shards: Vec<ShardDescriptor>,
}

impl ShardManifest {
    pub fn new(
        identity: RequestIdentity,
        shards: Vec<ShardDescriptor>,
    ) -> DiskAnn3ServiceResult<Self> {
        if shards.is_empty() || shards.iter().any(|shard| shard.identity != identity) {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::GenerationMismatch,
            ));
        }
        Ok(Self { identity, shards })
    }

    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
}

/// Hard maximum on the candidate shard count reached by one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardRouterConfig {
    max_fan_out: u32,
}

impl ShardRouterConfig {
    pub fn new(max_fan_out: u32) -> DiskAnn3ServiceResult<Self> {
        if max_fan_out == 0 {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::FanOutExceeded,
            ));
        }
        Ok(Self { max_fan_out })
    }

    pub const fn max_fan_out(&self) -> u32 {
        self.max_fan_out
    }
}

/// Routed shard set; partial is explicit rather than an unmarked empty result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardRoute {
    ready_shards: Vec<u32>,
    unavailable_shards: Vec<u32>,
}

impl ShardRoute {
    pub fn is_partial(&self) -> bool {
        !self.unavailable_shards.is_empty()
    }
    pub fn ready_shards(&self) -> &[u32] {
        &self.ready_shards
    }
    pub fn unavailable_shards(&self) -> &[u32] {
        &self.unavailable_shards
    }
}

/// Deterministic pure router. It performs only metadata selection, never vector I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardRouter {
    config: ShardRouterConfig,
}

impl ShardRouter {
    pub const fn new(config: ShardRouterConfig) -> Self {
        Self { config }
    }

    pub fn route(
        &self,
        request: &SearchRequest,
        manifest: &ShardManifest,
    ) -> DiskAnn3ServiceResult<ShardRoute> {
        request.authorization().authorizes(request.predicate())?;
        if request.identity() != manifest.identity() {
            let code = if request.identity().generation() != manifest.identity().generation() {
                DiskAnn3ServiceDiagnosticCode::StaleGeneration
            } else {
                DiskAnn3ServiceDiagnosticCode::GenerationMismatch
            };
            return Err(DiskAnn3ServiceError::contract(code));
        }
        let selected: Vec<&ShardDescriptor> = manifest
            .shards
            .iter()
            .filter(|shard| shard.metadata.matches(request))
            .collect();
        if selected.len() as u32 > self.config.max_fan_out {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::FanOutExceeded,
            ));
        }
        if selected.is_empty() {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::PartialShardUnavailable,
            ));
        }
        let mut ready_shards = Vec::new();
        let mut unavailable_shards = Vec::new();
        for shard in selected {
            match shard.health {
                ShardHealth::Ready => ready_shards.push(shard.id),
                ShardHealth::Unavailable | ShardHealth::CircuitOpen if shard.required => {
                    unavailable_shards.push(shard.id)
                }
                ShardHealth::Unavailable | ShardHealth::CircuitOpen => {}
            }
        }
        Ok(ShardRoute {
            ready_shards,
            unavailable_shards,
        })
    }
}

fn intersects(left: &[String], right: &[String]) -> bool {
    left.iter().any(|value| right.contains(value))
}
