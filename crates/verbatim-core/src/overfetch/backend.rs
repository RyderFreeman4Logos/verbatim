//! Primary-first backend selection and conditional fallback policy.

use serde::{Deserialize, Deserializer, Serialize};

use super::{OverfetchError, OverfetchResult};

/// Recognized dense-retrieval backends at the orchestration boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalBackend {
    DiskAnn3,
    Qdrant,
    LanceDb,
    LocalDense,
}

/// Closed typed conditions under which a primary backend may fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedBackendFailure {
    Unavailable,
    DeadlineExceeded,
    ProtocolViolation,
}

/// Outcome of a primary backend attempt used to decide whether fallback is legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "failure")]
pub enum PrimaryBackendOutcome {
    Satisfied,
    DeclaredInsufficientResults,
    TypedFailure(TypedBackendFailure),
}

/// A selected primary backend and an optional declared fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PrimaryBackendSelection {
    primary: RetrievalBackend,
    fallback: Option<RetrievalBackend>,
}

#[derive(Deserialize)]
struct PrimaryBackendSelectionFields {
    primary: RetrievalBackend,
    fallback: Option<RetrievalBackend>,
}

impl<'de> Deserialize<'de> for PrimaryBackendSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = PrimaryBackendSelectionFields::deserialize(deserializer)?;
        Self::new(fields.primary, fields.fallback).map_err(serde::de::Error::custom)
    }
}

impl PrimaryBackendSelection {
    pub fn new(
        primary: RetrievalBackend,
        fallback: Option<RetrievalBackend>,
    ) -> OverfetchResult<Self> {
        let selection = Self { primary, fallback };
        selection.validate()?;
        Ok(selection)
    }

    pub const fn primary(&self) -> RetrievalBackend {
        self.primary
    }

    pub const fn fallback(&self) -> Option<RetrievalBackend> {
        self.fallback
    }

    pub fn validate(&self) -> OverfetchResult<()> {
        if self.fallback == Some(self.primary) {
            return Err(OverfetchError::PrimaryBackendRequired);
        }
        Ok(())
    }

    /// Rejects any attempt that does not start with the selected primary.
    pub fn validate_first_attempt(&self, attempted: RetrievalBackend) -> OverfetchResult<()> {
        self.validate()?;
        if attempted != self.primary {
            return Err(OverfetchError::PrimaryBackendRequired);
        }
        Ok(())
    }

    /// Returns a fallback only after a typed failure or declared insufficiency.
    pub fn fallback_after(
        &self,
        outcome: PrimaryBackendOutcome,
    ) -> OverfetchResult<Option<RetrievalBackend>> {
        self.validate()?;
        match outcome {
            PrimaryBackendOutcome::Satisfied => Ok(None),
            PrimaryBackendOutcome::DeclaredInsufficientResults
            | PrimaryBackendOutcome::TypedFailure(_) => self
                .fallback
                .map(Some)
                .ok_or(OverfetchError::PrimaryBackendRequired),
        }
    }
}
