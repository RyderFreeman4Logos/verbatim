//! Immutable generation replicas and explicit update/delta durability boundaries.

use serde::{Deserialize, Serialize};

use super::{
    DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult, RequestIdentity,
};

/// Immutable service replica location, validated without storing network credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ReplicaEndpoint(String);

impl<'de> Deserialize<'de> for ReplicaEndpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl ReplicaEndpoint {
    pub fn new(value: impl Into<String>) -> DiskAnn3ServiceResult<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidReplicaSet,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Valid serving storage modes. Mutable NFS/SMB layouts are explicitly invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaStorage {
    LocalNvme,
    SharedMutableNfs,
    SharedMutableSmb,
}

/// Immutable published replica set. There is no mutable shared index mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableReplicaSet {
    identity: RequestIdentity,
    replicas: Vec<ReplicaEndpoint>,
    storage: ReplicaStorage,
}

impl<'de> Deserialize<'de> for ImmutableReplicaSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            identity: RequestIdentity,
            replicas: Vec<ReplicaEndpoint>,
            storage: ReplicaStorage,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::with_storage(wire.identity, wire.replicas, wire.storage)
            .map_err(serde::de::Error::custom)
    }
}

impl ImmutableReplicaSet {
    pub fn new(
        identity: RequestIdentity,
        replicas: Vec<ReplicaEndpoint>,
    ) -> DiskAnn3ServiceResult<Self> {
        Self::with_storage(identity, replicas, ReplicaStorage::LocalNvme)
    }

    pub fn with_storage(
        identity: RequestIdentity,
        replicas: Vec<ReplicaEndpoint>,
        storage: ReplicaStorage,
    ) -> DiskAnn3ServiceResult<Self> {
        if replicas.is_empty() || replicas.len() > 64 {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidReplicaSet,
            ));
        }
        if storage != ReplicaStorage::LocalNvme {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidDeploymentStorage,
            ));
        }
        Ok(Self {
            identity,
            replicas,
            storage,
        })
    }

    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub fn replicas(&self) -> &[ReplicaEndpoint] {
        &self.replicas
    }
    pub const fn storage(&self) -> ReplicaStorage {
        self.storage
    }
}

/// Serving-set transition. Exactly one active identity avoids mixed generations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveGenerationSet {
    active: ImmutableReplicaSet,
}

impl ActiveGenerationSet {
    pub fn new(sets: Vec<ImmutableReplicaSet>) -> DiskAnn3ServiceResult<Self> {
        if sets.len() != 1 {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::IncompatibleActiveGeneration,
            ));
        }
        Ok(Self {
            active: sets.into_iter().next().expect("length checked"),
        })
    }

    pub const fn active(&self) -> &ImmutableReplicaSet {
        &self.active
    }
}

/// Explicit delta/recovery boundary; DiskANN library state cannot satisfy this alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaRecoveryContract {
    AuthoritativeDurableLog,
    AnnLibraryOnly,
}

impl DeltaRecoveryContract {
    pub fn new(contract: Self) -> DiskAnn3ServiceResult<Self> {
        if contract == Self::AnnLibraryOnly {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::DurabilityContractRequired,
            ));
        }
        Ok(contract)
    }

    pub fn ann_library_only() -> DiskAnn3ServiceResult<Self> {
        Self::new(Self::AnnLibraryOnly)
    }
}
