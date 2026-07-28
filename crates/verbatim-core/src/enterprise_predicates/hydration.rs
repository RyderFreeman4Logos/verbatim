//! Hydration revalidation contract: defense-in-depth validation of every
//! returned candidate ID during authoritative hydration.
//!
//! Candidate generation may return IDs that the authoritative store later
//! rejects (tombstoned, lifecycle-filtered, ACL-revoked). This contract defines
//! the bounded revalidation that must run on every returned ID before it is
//! surfaced to the caller.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    EnterprisePredicateDiagnosticCode, EnterprisePredicateError, EnterprisePredicateResult,
};

/// Maximum number of candidate IDs that may be revalidated in one bounded batch.
pub const MAX_REVALIDATION_BATCH: usize = 1_024;

/// Opaque validated candidate identifier. The string content is never leaked in
/// `Debug`/`Display`; only the closed revalidation outcome is reported.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct CandidateIdentifier(String);

impl<'de> Deserialize<'de> for CandidateIdentifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

const MAX_IDENTIFIER_BYTES: usize = 512;

impl CandidateIdentifier {
    /// Creates a bounded, non-empty candidate identifier.
    pub fn new(value: impl Into<String>) -> EnterprisePredicateResult<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
            return Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::InvalidPredicateValue,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the identifier (crate-private; not exposed in diagnostics).
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CandidateIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CandidateIdentifier([REDACTED])")
    }
}

/// Outcome of revalidating one candidate during authoritative hydration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevalidationOutcome {
    /// The candidate is authorized, lifecycle-visible, and not tombstoned.
    Accepted,
    /// The candidate was tombstoned after candidate generation.
    Tombstoned,
    /// The candidate failed the lifecycle predicate during hydration.
    LifecycleRejected,
    /// The candidate's ACL grant was revoked during hydration.
    AclRevoked,
}

impl RevalidationOutcome {
    /// Whether this outcome authorizes the candidate for surfacing.
    pub const fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// A bounded batch of candidate identifiers to revalidate.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RevalidationBatch {
    identifiers: Vec<CandidateIdentifier>,
}

impl<'de> Deserialize<'de> for RevalidationBatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let identifiers = Vec::<CandidateIdentifier>::deserialize(deserializer)?;
        Self::new(identifiers).map_err(serde::de::Error::custom)
    }
}

impl RevalidationBatch {
    /// Builds a bounded revalidation batch.
    pub fn new(identifiers: Vec<CandidateIdentifier>) -> EnterprisePredicateResult<Self> {
        if identifiers.len() > MAX_REVALIDATION_BATCH {
            return Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::InvalidHydrationBudget,
            ));
        }
        Ok(Self { identifiers })
    }

    /// The identifiers in this batch.
    pub fn identifiers(&self) -> &[CandidateIdentifier] {
        &self.identifiers
    }

    /// Number of identifiers in the batch.
    pub fn len(&self) -> usize {
        self.identifiers.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.identifiers.is_empty()
    }
}

impl fmt::Debug for RevalidationBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevalidationBatch")
            .field("count", &self.identifiers.len())
            .finish()
    }
}

/// The contract for defense-in-depth revalidation during authoritative hydration.
///
/// Implementations must validate every returned candidate ID against the
/// authoritative ACL, lifecycle, and tombstone state, bound to the same
/// [`super::generation::GenerationBinding`] as candidate generation. Any
/// non-accepted candidate is dropped; the rejected outcome is returned so the
/// caller can record that revalidation intervened, without leaking the ID.
pub trait HydrationRevalidation {
    /// Revalidates one bounded batch of candidate identifiers.
    ///
    /// Returns one outcome per input identifier, in order. Implementations must
    /// not surface unauthorized IDs, counts, or distances in diagnostics.
    fn revalidate(
        &self,
        batch: &RevalidationBatch,
    ) -> EnterprisePredicateResult<Vec<RevalidationOutcome>>;

    /// Revalidates a single identifier as a convenience.
    ///
    /// Returns `Ok(())` when accepted, or a fail-closed
    /// [`EnterprisePredicateError`] with
    /// [`EnterprisePredicateDiagnosticCode::HydrationRevalidationFailed`]
    /// otherwise.
    fn revalidate_one(&self, identifier: &CandidateIdentifier) -> EnterprisePredicateResult<()> {
        let batch = RevalidationBatch::new(vec![identifier.clone()])?;
        let outcomes = self.revalidate(&batch)?;
        match outcomes.as_slice() {
            [outcome] if outcome.is_accepted() => Ok(()),
            _ => Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::HydrationRevalidationFailed,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysAccepted;

    impl HydrationRevalidation for AlwaysAccepted {
        fn revalidate(
            &self,
            batch: &RevalidationBatch,
        ) -> EnterprisePredicateResult<Vec<RevalidationOutcome>> {
            Ok(batch
                .identifiers()
                .iter()
                .map(|_| RevalidationOutcome::Accepted)
                .collect())
        }
    }

    struct AlwaysTombstoned;

    impl HydrationRevalidation for AlwaysTombstoned {
        fn revalidate(
            &self,
            batch: &RevalidationBatch,
        ) -> EnterprisePredicateResult<Vec<RevalidationOutcome>> {
            Ok(batch
                .identifiers()
                .iter()
                .map(|_| RevalidationOutcome::Tombstoned)
                .collect())
        }
    }

    fn id(n: usize) -> CandidateIdentifier {
        CandidateIdentifier::new(format!("id-{n}")).unwrap()
    }

    #[test]
    fn candidate_identifier_redacts_debug() {
        let identifier = id(1);
        let debug = format!("{:?}", identifier);
        assert_eq!(debug, "CandidateIdentifier([REDACTED])");
        assert!(!debug.contains("id-1"));
    }

    #[test]
    fn empty_identifier_rejected() {
        let result = CandidateIdentifier::new("");
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidPredicateValue
        );
    }

    #[test]
    fn oversize_identifier_rejected() {
        let result = CandidateIdentifier::new("x".repeat(MAX_IDENTIFIER_BYTES + 1));
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidPredicateValue
        );
    }

    #[test]
    fn batch_reports_count_not_ids() {
        let batch = RevalidationBatch::new(vec![id(1), id(2)]).unwrap();
        let debug = format!("{:?}", batch);
        assert!(debug.contains("count: 2"));
        assert!(!debug.contains("id-1"));
        assert!(!debug.contains("id-2"));
    }

    #[test]
    fn oversize_batch_rejected() {
        let identifiers: Vec<CandidateIdentifier> = (0..=MAX_REVALIDATION_BATCH).map(id).collect();
        let result = RevalidationBatch::new(identifiers);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidHydrationBudget
        );
    }

    #[test]
    fn always_accepted_revalidates_one_ok() {
        let revalidator = AlwaysAccepted;
        revalidator.revalidate_one(&id(1)).unwrap();
    }

    #[test]
    fn always_tombstoned_revalidates_one_fail_closed() {
        let revalidator = AlwaysTombstoned;
        let result = revalidator.revalidate_one(&id(1));
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::HydrationRevalidationFailed
        );
    }

    #[test]
    fn revalidation_batch_outcomes_preserve_order() {
        struct Alternating;
        impl HydrationRevalidation for Alternating {
            fn revalidate(
                &self,
                batch: &RevalidationBatch,
            ) -> EnterprisePredicateResult<Vec<RevalidationOutcome>> {
                Ok(batch
                    .identifiers()
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        if i % 2 == 0 {
                            RevalidationOutcome::Accepted
                        } else {
                            RevalidationOutcome::AclRevoked
                        }
                    })
                    .collect())
            }
        }
        let batch = RevalidationBatch::new(vec![id(0), id(1), id(2), id(3)]).unwrap();
        let outcomes = Alternating.revalidate(&batch).unwrap();
        assert_eq!(
            outcomes,
            vec![
                RevalidationOutcome::Accepted,
                RevalidationOutcome::AclRevoked,
                RevalidationOutcome::Accepted,
                RevalidationOutcome::AclRevoked,
            ]
        );
    }

    #[test]
    fn outcomes_classify_correctly() {
        assert!(RevalidationOutcome::Accepted.is_accepted());
        assert!(!RevalidationOutcome::Tombstoned.is_accepted());
        assert!(!RevalidationOutcome::LifecycleRejected.is_accepted());
        assert!(!RevalidationOutcome::AclRevoked.is_accepted());
    }
}
