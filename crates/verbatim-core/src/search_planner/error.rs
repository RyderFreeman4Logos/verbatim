//! Closed diagnostic-only failures for the search-planner contract.

use std::error::Error;
use std::fmt;

/// Result alias for SearchBudget and retrieval-planner contract operations.
pub type SearchPlannerResult<T> = Result<T, SearchPlannerError>;

/// Closed diagnostic taxonomy for the planner contract.
///
/// No variant retains caller-controlled identifiers, payload text, estimates, or
/// backend responses. This keeps `Debug` and `Display` safe for public diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchPlannerDiagnosticCode {
    /// A hard request or stage budget was zero or exceeded.
    BudgetExceeded,
    /// Summing independent budget dimensions overflowed.
    BudgetOverflow,
    /// A plan attempted to widen its caller's budget.
    PlanBudgetWidened,
    /// A fallback had no valid shared budget remaining.
    FallbackBudgetExhausted,
    /// The authorized cardinality estimate is stale.
    StaleCardinalityEstimate,
    /// The authorized cardinality estimate is known wrong or insufficiently trusted.
    UntrustedCardinalityEstimate,
    /// A selectivity crossover calibration is invalid.
    InvalidCrossoverThreshold,
    /// A backend capability declaration is internally invalid.
    InvalidCapability,
    /// A required backend capability is not available.
    CapabilityUnsupported,
    /// A caller budget exceeds a backend's safe request limit.
    CapabilityLimitExceeded,
    /// A generation binding was absent or invalid.
    GenerationBindingInvalid,
    /// A strict predicate cannot be evaluated by the chosen path.
    StrictPredicateUnsupported,
    /// An explicit exhaustive request cannot fit inside its hard budget.
    ExhaustiveBudgetExceeded,
    /// A sealed retrieval plan violated an internal invariant.
    InvalidPlan,
    /// Reported actual work exceeded the sealed plan budget.
    ActualWorkExceeded,
}

impl SearchPlannerDiagnosticCode {
    /// Returns the stable, machine-readable diagnostic code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "budget_exceeded",
            Self::BudgetOverflow => "budget_overflow",
            Self::PlanBudgetWidened => "plan_budget_widened",
            Self::FallbackBudgetExhausted => "fallback_budget_exhausted",
            Self::StaleCardinalityEstimate => "stale_cardinality_estimate",
            Self::UntrustedCardinalityEstimate => "untrusted_cardinality_estimate",
            Self::InvalidCrossoverThreshold => "invalid_crossover_threshold",
            Self::InvalidCapability => "invalid_capability",
            Self::CapabilityUnsupported => "capability_unsupported",
            Self::CapabilityLimitExceeded => "capability_limit_exceeded",
            Self::GenerationBindingInvalid => "generation_binding_invalid",
            Self::StrictPredicateUnsupported => "strict_predicate_unsupported",
            Self::ExhaustiveBudgetExceeded => "exhaustive_budget_exceeded",
            Self::InvalidPlan => "invalid_plan",
            Self::ActualWorkExceeded => "actual_work_exceeded",
        }
    }
}

/// A planner failure containing only a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SearchPlannerError {
    code: SearchPlannerDiagnosticCode,
}

impl SearchPlannerError {
    /// Constructs a closed error from an internal diagnostic code.
    pub(crate) const fn new(code: SearchPlannerDiagnosticCode) -> Self {
        Self { code }
    }

    /// Returns the closed diagnostic code without any caller-controlled detail.
    pub const fn diagnostic_code(self) -> SearchPlannerDiagnosticCode {
        self.code
    }
}

impl fmt::Debug for SearchPlannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "SearchPlannerError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for SearchPlannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "search-planner.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for SearchPlannerError {}
