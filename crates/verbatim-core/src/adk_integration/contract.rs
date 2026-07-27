//! Contract trait enforcing ADK-Rust policy before integration wiring exists.

use crate::adk_integration::{
    AdkBoundaryUse, AdkCrateCatalog, AdkIntegrationError, AdkIntegrationResult, DomainBoundaryRule,
    VersionPolicy,
};

/// The fail-closed integration contract for selected ADK-Rust capabilities.
pub trait AdkIntegrationContract {
    /// Ensures every selected crate retains its approved disposition and constraints.
    fn validate_disposition(&self, catalog: &AdkCrateCatalog) -> AdkIntegrationResult<()>;

    /// Rejects an attempted ADK crossing into a protected Verbatim domain.
    fn check_boundary(&self, attempted_use: AdkBoundaryUse) -> AdkIntegrationResult<()>;

    /// Accepts only an exact, stable ADK-Rust 1.x version.
    fn pin_version(&self, exact_version: &str) -> AdkIntegrationResult<VersionPolicy>;
}

/// Stateless implementation of the ADK-Rust integration contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdkIntegrationPolicy;

impl AdkIntegrationContract for AdkIntegrationPolicy {
    fn validate_disposition(&self, catalog: &AdkCrateCatalog) -> AdkIntegrationResult<()> {
        catalog.validate()
    }

    fn check_boundary(&self, attempted_use: AdkBoundaryUse) -> AdkIntegrationResult<()> {
        for rule in DomainBoundaryRule::ALL {
            if rule.forbidden_use() == attempted_use {
                return Err(AdkIntegrationError::validation(rule.diagnostic_code()));
            }
        }
        Ok(())
    }

    fn pin_version(&self, exact_version: &str) -> AdkIntegrationResult<VersionPolicy> {
        VersionPolicy::new(exact_version)
    }
}
