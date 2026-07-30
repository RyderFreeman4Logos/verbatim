//! Typed VECTOR-REF-002 LanceDB IVF_RQ / IVF_PQ reference-backend adapter contract.
//!
//! This walking skeleton intentionally contains no `lancedb` dependency, live client,
//! table handles, or network I/O. Future crate-owned adapters implement this sealed surface.

mod budget;
mod capability;
mod contract;
mod error;
mod filter;
mod identity;
mod index_profile;
mod lexical_caveat;
mod lifecycle;
mod probe;
mod quality;
mod schema;
mod search_policy;

pub use budget::LanceDbOperationBudget;
pub use capability::{LanceDbCapabilities, LanceDbCapabilityFields};
pub use contract::LanceDbVectorSearch;
pub use error::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};
pub use filter::{
    FilterClause, FilterStrictness, LanceDbFilterContract, LanceDbScalarIndexKind,
    LanceDbScalarIndexPlan, LanceDbScalarIndexRequirement,
};
pub use identity::{LanceDbCollectionIdentity, LanceDbHitRef, TableName};
pub use index_profile::LanceDbIndexProfile;
pub use lexical_caveat::{LanceDbLexicalPolicy, LexicalConformanceFlag, LexicalOwnership};
pub use lifecycle::{LanceDbLifecycleOperation, LanceDbLifecycleState, LanceDbLifecycleTransition};
pub use probe::AdaptiveProbePlan;
pub use quality::{CandidateLossReport, LanceDbQualityPlan};
pub use schema::{LanceDbCollectionSchema, LanceDbSchemaFields};
pub use search_policy::{BackendSelection, LanceDbSearchPolicy, LanceDbSearchRequest};

/// Contract schema version for the LanceDB reference adapter boundary.
pub const LANCEDB_REFERENCE_BACKEND_ADAPTER_SCHEMA_VERSION: u32 = 1;
