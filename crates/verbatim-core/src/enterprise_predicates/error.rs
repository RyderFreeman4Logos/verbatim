//! Closed, diagnostic-only failures for the enterprise predicate contract.
//!
//! No variant retains caller-controlled input (tenant, ACL principal, source
//! id, predicate value). Public `Debug` and `Display` rendering is safe to
//! expose in operational diagnostics and metrics/logs.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for enterprise predicate contract operations.
pub type EnterprisePredicateResult<T> = Result<T, EnterprisePredicateError>;

/// Closed diagnostic taxonomy. No variant carries caller-controlled data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnterprisePredicateDiagnosticCode {
    /// A typed predicate value failed bounded validation.
    InvalidPredicateValue,
    /// The predicate payload exceeded the bounded filter count.
    PredicatePayloadTooLarge,
    /// A strict predicate is unsupported by the selected index path.
    UnsupportedStrictPredicate,
    /// The authorized candidate set is empty; no traversal is permitted.
    ZeroAuthorizedCandidates,
    /// The policy or publication generation binding is missing or invalid.
    GenerationBindingInvalid,
    /// A returned candidate failed authoritative hydration revalidation.
    HydrationRevalidationFailed,
    /// Selectivity classification thresholds are malformed or unordered.
    InvalidSelectivityThreshold,
    /// The candidate hydration budget is zero or malformed.
    InvalidHydrationBudget,
}

impl EnterprisePredicateDiagnosticCode {
    /// Every closed diagnostic code, useful for exhaustive contract tests.
    pub const ALL: [Self; 8] = [
        Self::InvalidPredicateValue,
        Self::PredicatePayloadTooLarge,
        Self::UnsupportedStrictPredicate,
        Self::ZeroAuthorizedCandidates,
        Self::GenerationBindingInvalid,
        Self::HydrationRevalidationFailed,
        Self::InvalidSelectivityThreshold,
        Self::InvalidHydrationBudget,
    ];

    /// Stable machine-readable diagnostic code without caller-controlled data.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPredicateValue => "invalid_predicate_value",
            Self::PredicatePayloadTooLarge => "predicate_payload_too_large",
            Self::UnsupportedStrictPredicate => "unsupported_strict_predicate",
            Self::ZeroAuthorizedCandidates => "zero_authorized_candidates",
            Self::GenerationBindingInvalid => "generation_binding_invalid",
            Self::HydrationRevalidationFailed => "hydration_revalidation_failed",
            Self::InvalidSelectivityThreshold => "invalid_selectivity_threshold",
            Self::InvalidHydrationBudget => "invalid_hydration_budget",
        }
    }
}

/// A contract failure containing only a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum EnterprisePredicateError {
    Contract {
        code: EnterprisePredicateDiagnosticCode,
    },
}

impl EnterprisePredicateError {
    pub const fn contract(code: EnterprisePredicateDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> EnterprisePredicateDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for EnterprisePredicateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "EnterprisePredicateError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for EnterprisePredicateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "enterprise-predicates.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for EnterprisePredicateError {}
