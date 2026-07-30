//! Generation-bound staging, optimization, validation, publication, and recovery hooks.

use serde::{Deserialize, Serialize};

use super::{
    LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult,
    LanceDbCollectionIdentity,
};

/// Lifecycle operation names required from a future `IndexPublisher` implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanceDbLifecycleOperation {
    Append,
    OptimizeReindex,
    Validate,
    Promote,
    Rollback,
    Delete,
    Compact,
    CrashRecover,
}

/// Lifecycle states named to align with `generation_publication` and `IndexPublisher`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanceDbLifecycleState {
    Staged,
    Optimized,
    Validated,
    Promoted,
    RolledBack,
    Deleted,
    Compacted,
    CrashRecovered,
}

impl LanceDbLifecycleState {
    /// Operational hooks remain generation-bound even when they do not publish a new generation.
    pub const fn is_generation_bound_hook(self) -> bool {
        matches!(self, Self::Deleted | Self::Compacted | Self::CrashRecovered)
    }
}

/// One valid lifecycle edge for a single immutable generation identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbLifecycleTransition {
    identity: LanceDbCollectionIdentity,
    from: LanceDbLifecycleState,
    to: LanceDbLifecycleState,
}

impl<'de> Deserialize<'de> for LanceDbLifecycleTransition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            identity: LanceDbCollectionIdentity,
            from: LanceDbLifecycleState,
            to: LanceDbLifecycleState,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.identity, wire.from, wire.to).map_err(serde::de::Error::custom)
    }
}

impl LanceDbLifecycleTransition {
    pub fn new(
        identity: LanceDbCollectionIdentity,
        from: LanceDbLifecycleState,
        to: LanceDbLifecycleState,
    ) -> LanceDbBackendResult<Self> {
        if !matches!(
            (from, to),
            (
                LanceDbLifecycleState::Staged,
                LanceDbLifecycleState::Optimized
            ) | (
                LanceDbLifecycleState::Optimized,
                LanceDbLifecycleState::Validated
            ) | (
                LanceDbLifecycleState::Validated,
                LanceDbLifecycleState::Promoted
            ) | (
                LanceDbLifecycleState::Validated,
                LanceDbLifecycleState::RolledBack
            ) | (
                LanceDbLifecycleState::Promoted,
                LanceDbLifecycleState::Compacted
            ) | (
                LanceDbLifecycleState::Promoted,
                LanceDbLifecycleState::Deleted
            ) | (
                LanceDbLifecycleState::Promoted,
                LanceDbLifecycleState::CrashRecovered
            ) | (
                LanceDbLifecycleState::CrashRecovered,
                LanceDbLifecycleState::Validated
            )
        ) {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidLifecycleTransition,
            ));
        }
        Ok(Self { identity, from, to })
    }

    pub const fn identity(&self) -> &LanceDbCollectionIdentity {
        &self.identity
    }

    pub const fn from(&self) -> LanceDbLifecycleState {
        self.from
    }

    pub const fn to(&self) -> LanceDbLifecycleState {
        self.to
    }
}
