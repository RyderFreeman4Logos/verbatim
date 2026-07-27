//! Backend roles and explicit legacy gating.

use serde::{Deserialize, Serialize};

use super::{VectorSearchDiagnosticCode, VectorSearchError, VectorSearchResult};

/// A vector backend's architectural role, not a migration preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendRole {
    Primary,
    Reference,
    Legacy,
}

impl BackendRole {
    pub const ALL: [Self; 3] = [Self::Primary, Self::Reference, Self::Legacy];
}

/// Recognized vector backends in the DiskANN3 architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorBackend {
    DiskAnn3,
    Qdrant,
    LanceDb,
    SQLite,
    HnswLegacy,
}

impl VectorBackend {
    pub const ALL: [Self; 5] = [
        Self::DiskAnn3,
        Self::Qdrant,
        Self::LanceDb,
        Self::SQLite,
        Self::HnswLegacy,
    ];

    pub const fn role(self) -> BackendRole {
        match self {
            Self::DiskAnn3 => BackendRole::Primary,
            Self::Qdrant | Self::LanceDb => BackendRole::Reference,
            Self::SQLite | Self::HnswLegacy => BackendRole::Legacy,
        }
    }

    pub const fn is_legacy(self) -> bool {
        matches!(self.role(), BackendRole::Legacy)
    }
}

/// A backend selection proves that a legacy path had explicit operator opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct BackendSelection {
    backend: VectorBackend,
    legacy_opt_in: bool,
}

#[derive(Deserialize)]
struct SerializedBackendSelection {
    backend: VectorBackend,
    legacy_opt_in: bool,
}

impl<'de> Deserialize<'de> for BackendSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let serialized = SerializedBackendSelection::deserialize(deserializer)?;
        Self::new(serialized.backend, serialized.legacy_opt_in).map_err(serde::de::Error::custom)
    }
}

impl BackendSelection {
    pub fn new(backend: VectorBackend, legacy_opt_in: bool) -> VectorSearchResult<Self> {
        let selection = Self {
            backend,
            legacy_opt_in,
        };
        selection.validate()?;
        Ok(selection)
    }

    pub const fn backend(self) -> VectorBackend {
        self.backend
    }

    pub const fn role(self) -> BackendRole {
        self.backend.role()
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        if self.backend.is_legacy() && !self.legacy_opt_in {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::LegacyBackendOptInRequired,
            ));
        }
        Ok(())
    }
}
