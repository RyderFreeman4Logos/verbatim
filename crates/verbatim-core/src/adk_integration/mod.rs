//! ADK-Rust integration domain-boundary contract (issue #365).
//!
//! This pure walking skeleton records selected-crate policy, explicit domain
//! boundaries, exact-version pinning, and diagnostic-only failures. It has no
//! live ADK-Rust dependency, provider, storage, daemon, or workflow runtime.
//! See `docs/architecture/adk-rust-integration.md`.

mod boundary;
mod catalog;
mod contract;
mod error;
mod version;

pub use boundary::{AdkBoundaryUse, DomainBoundaryRule};
pub use catalog::{
    decode_adk_crate_catalog_json, encode_adk_crate_catalog_json, AdkBoundaryConstraint,
    AdkCrateCatalog, AdkCrateDisposition, AdkCrateName, AdkCratePolicy,
    PlatformSecurityConformance,
};
pub use contract::{AdkIntegrationContract, AdkIntegrationPolicy};
pub use error::{AdkIntegrationDiagnosticCode, AdkIntegrationError, AdkIntegrationResult};
pub use version::{
    decode_version_policy_json, encode_version_policy_json, AdkVersion, VersionPolicy,
};

/// Contract schema version for ADK-Rust integration policy documents.
pub const ADK_INTEGRATION_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../adk_integration_tests.rs"]
mod tests;
