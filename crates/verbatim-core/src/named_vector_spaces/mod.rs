//! Typed VECTOR-SSD-009 named vector spaces, multimodal routing, and
//! late-interaction SSD contract.
//!
//! This is a walking skeleton only: it deliberately contains no live DiskANN3,
//! Qdrant, or LanceDB multivector client, no filesystem I/O, and no fusion
//! implementation. It specifies backend-neutral native-dimensional spaces,
//! generation-bound mappings, bounded routing, explicit capabilities, and
//! publication/lifecycle invariants. See `docs/architecture/named-vector-spaces.md`.

mod error;
mod identity;
mod late_interaction;
mod lifecycle;
mod mapping;
mod query;
mod spec;

pub use error::{NamedVectorSpaceDiagnosticCode, NamedVectorSpaceError, NamedVectorSpaceResult};
pub use identity::{
    CandidateIndexProfileId, ModelIdentity, NamedVectorSpaceId, ObjectId, PublicationGeneration,
};
pub use late_interaction::{
    ExactInteraction, LateInteractionCandidateStage, LateInteractionQualityMeasurements,
    VectorRange,
};
pub use lifecycle::{
    DerivedRepresentationOperation, NamedVectorPublicationManifest, SpacePublicationState,
    SpaceRetentionRequest, StagedSpaceArtifact,
};
pub use mapping::{ObjectSpaceMapping, StorageComplexityContract, VectorLocation};
pub use query::{
    BackendCapabilities, CandidateInclusionReason, CompiledNamedVectorPlan, DegradedProfile,
    FusionProfileIdentity, NamedVectorClause, NamedVectorQueryPlan, QueryVectorShape, SearchBudget,
    SpaceAvailability, SpaceCandidate,
};
pub use spec::{
    EmbeddingModality, NamedVectorSpaceSpec, NamedVectorSpaceSpecFields, Normalization,
    QueryOperation, StorageEncoding, VectorMetric,
};

/// Version for durable VECTOR-SSD-009 contract documents.
pub const NAMED_VECTOR_SPACES_CONTRACT_SCHEMA_VERSION: u32 = 1;
