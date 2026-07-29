//! Completeness semantics for hybrid fusion.
//!
//! Ordinary ANN/BM25 fusion is approximate Top-K. Exact/exhaustive workflows
//! must enumerate a declared scope, track coverage, and report inability to
//! establish completeness. A high fusion score may never be turned into an
//! unsupported exhaustive claim.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{HybridFusionDiagnosticCode, HybridFusionError, HybridFusionResult};

/// Stable identity for a declared exhaustive scope. Opaque to the contract:
/// it is a presentation-free label such as a generation id or snapshot cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExhaustiveScopeId(String);

impl ExhaustiveScopeId {
    /// Builds a scope id, rejecting empty/whitespace input.
    pub fn new(value: String) -> HybridFusionResult<Self> {
        if value.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::CompletenessScopeEmpty,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExhaustiveScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Coverage accounting for an enumerated exhaustive scope.
///
/// `enumerated` is the number of items the exhaustive workflow visited;
/// `matched` is how many of those satisfied the retrieval predicate. The
/// invariant `matched <= enumerated` is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageAccount {
    enumerated: u64,
    matched: u64,
}

impl CoverageAccount {
    /// Builds a coverage account, rejecting inverted counts.
    pub fn new(enumerated: u64, matched: u64) -> HybridFusionResult<Self> {
        if enumerated == 0 {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::CompletenessScopeEmpty,
            ));
        }
        if matched > enumerated {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::CompletenessCoverageInvalid,
            ));
        }
        Ok(Self {
            enumerated,
            matched,
        })
    }

    pub const fn enumerated(self) -> u64 {
        self.enumerated
    }

    pub const fn matched(self) -> u64 {
        self.matched
    }

    /// Returns the fraction of enumerated items that matched, or `None` when
    /// the scope was empty. Empty scopes are rejected at construction, so a
    /// valid account always returns `Some`.
    pub const fn coverage_ratio(self) -> Option<f64> {
        // enumerated > 0 is an invariant; the Option is kept for API safety.
        if self.enumerated == 0 {
            None
        } else {
            Some(self.matched as f64 / self.enumerated as f64)
        }
    }
}

/// The completeness state of a fusion output.
///
/// `ApproximateTopK` is the default for ANN/BM25 fusion. `ExactScopeEnumerated`
/// is only permitted when an exhaustive scope has been declared and its
/// coverage tracked. `CoverageIncomplete` records that coverage could not be
/// established and no exhaustive claim may be made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletenessState {
    /// Approximate Top-K fusion. No exhaustive claim is permitted.
    #[default]
    ApproximateTopK,
    /// Exact over the enumerated authorized scope only. Carries the scope id
    /// and coverage account. Never globally exact.
    ExactScopeEnumerated {
        scope_id: ExhaustiveScopeId,
        coverage: CoverageAccount,
    },
    /// Coverage could not be established. No exhaustive claim is permitted.
    CoverageIncomplete { scope_id: ExhaustiveScopeId },
}

impl CompletenessState {
    /// Returns `true` only for an exact-scope-enumerated state.
    pub const fn is_exact_scope(&self) -> bool {
        matches!(self, Self::ExactScopeEnumerated { .. })
    }

    /// Returns `true` if this state is globally exact — always `false`.
    /// A high fusion score never becomes an unsupported exhaustive claim.
    pub const fn is_global_exact(&self) -> bool {
        false
    }

    /// Returns `true` when this state permits an exhaustive claim. Only
    /// `ExactScopeEnumerated` qualifies.
    pub const fn may_claim_exhaustive(&self) -> bool {
        matches!(self, Self::ExactScopeEnumerated { .. })
    }

    /// Returns the scope id when this state carries one.
    pub fn scope_id(&self) -> Option<&ExhaustiveScopeId> {
        match self {
            Self::ExactScopeEnumerated { scope_id, .. } | Self::CoverageIncomplete { scope_id } => {
                Some(scope_id)
            }
            Self::ApproximateTopK => None,
        }
    }

    /// Validates that an approximate retriever did not assert exhaustive scope.
    ///
    /// `approximate` retrievers (dense ANN, BM25 Top-K) may never carry an
    /// `ExactScopeEnumerated` state; they must remain `ApproximateTopK` or
    /// degrade to `CoverageIncomplete`.
    pub fn validate_against_approximate(&self, approximate: bool) -> HybridFusionResult<()> {
        if approximate && self.is_exact_scope() {
            return Err(HybridFusionError::CompletenessViolation {
                state: self.clone(),
                code: HybridFusionDiagnosticCode::CompletenessApproximateCannotClaimExhaustive,
            });
        }
        Ok(())
    }
}
