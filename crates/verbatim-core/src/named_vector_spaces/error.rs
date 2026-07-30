//! Closed, redacted diagnostics for the named-vector-space contract.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

pub type NamedVectorSpaceResult<T> = Result<T, NamedVectorSpaceError>;

/// Closed diagnostic taxonomy. Caller-controlled identifiers and backend details
/// are deliberately not retained by public errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedVectorSpaceDiagnosticCode {
    InvalidIdentifier,
    InvalidGeneration,
    InvalidNativeDimension,
    InvalidSpaceSpecification,
    UnsupportedQueryOperation,
    InvalidVectorMapping,
    MappingGenerationMismatch,
    DuplicateVectorLocation,
    InvalidStorageComplexity,
    ArithmeticOverflow,
    InvalidSearchBudget,
    InvalidQueryPlan,
    IncompatibleQueryShape,
    UnsupportedBackendCapability,
    MissingVectorSpace,
    StaleVectorSpace,
    WrongGeneration,
    InvalidLateInteractionLayout,
    InvalidLateInteractionMeasurement,
    InvalidPublicationManifest,
    InvalidLifecycleOperation,
    ReferencedEvidenceRetention,
}

impl NamedVectorSpaceDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIdentifier => "invalid_identifier",
            Self::InvalidGeneration => "invalid_generation",
            Self::InvalidNativeDimension => "invalid_native_dimension",
            Self::InvalidSpaceSpecification => "invalid_space_specification",
            Self::UnsupportedQueryOperation => "unsupported_query_operation",
            Self::InvalidVectorMapping => "invalid_vector_mapping",
            Self::MappingGenerationMismatch => "mapping_generation_mismatch",
            Self::DuplicateVectorLocation => "duplicate_vector_location",
            Self::InvalidStorageComplexity => "invalid_storage_complexity",
            Self::ArithmeticOverflow => "arithmetic_overflow",
            Self::InvalidSearchBudget => "invalid_search_budget",
            Self::InvalidQueryPlan => "invalid_query_plan",
            Self::IncompatibleQueryShape => "incompatible_query_shape",
            Self::UnsupportedBackendCapability => "unsupported_backend_capability",
            Self::MissingVectorSpace => "missing_vector_space",
            Self::StaleVectorSpace => "stale_vector_space",
            Self::WrongGeneration => "wrong_generation",
            Self::InvalidLateInteractionLayout => "invalid_late_interaction_layout",
            Self::InvalidLateInteractionMeasurement => "invalid_late_interaction_measurement",
            Self::InvalidPublicationManifest => "invalid_publication_manifest",
            Self::InvalidLifecycleOperation => "invalid_lifecycle_operation",
            Self::ReferencedEvidenceRetention => "referenced_evidence_retention",
        }
    }
}

/// Error carrying only a stable diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NamedVectorSpaceError(NamedVectorSpaceDiagnosticCode);

impl NamedVectorSpaceError {
    pub const fn contract(code: NamedVectorSpaceDiagnosticCode) -> Self {
        Self(code)
    }

    pub const fn diagnostic_code(self) -> NamedVectorSpaceDiagnosticCode {
        self.0
    }
}

impl fmt::Debug for NamedVectorSpaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "NamedVectorSpaceError({})", self.0.as_str())
    }
}

impl fmt::Display for NamedVectorSpaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "named-vector-spaces.{}", self.0.as_str())
    }
}

impl Error for NamedVectorSpaceError {}
