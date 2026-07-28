//! Redaction contract: sensitive predicate values must never leak into
//! metrics, logs, `Debug`, or `Display`.
//!
//! This module centralises the redaction guarantee for the enterprise predicate
//! contract. Every type carrying a tenant, ACL principal, source id, collection
//! id, or predicate value overrides `Debug`/`Display` to render a closed
//! `[REDACTED]` placeholder.

use std::fmt;

use super::hydration::RevalidationBatch;
use super::predicate::EnterprisePredicateConjunction;

/// Marker for redacted debug output. Implementors render only closed fields.
pub trait RedactedDebug: fmt::Debug {
    /// Formats a redacted, safe-for-diagnostics summary.
    fn redacted_summary(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result;
}

/// Asserts at compile time that a type's `Debug` is redaction-safe by virtue of
/// carrying only closed fields. This is a documentation marker; the actual
/// redaction is enforced by the manual `impl fmt::Debug` on each type.
pub struct RedactionAttested;

impl RedactionAttested {
    /// Stable label for operational attestations.
    pub const LABEL: &'static str = "enterprise-predicates-redaction-v1";
}

/// A closed redaction report suitable for metrics/logs. It never contains a
/// tenant, ACL principal, source id, collection id, predicate value, or
/// candidate identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RedactionReport {
    predicate_count: usize,
    has_authorization: bool,
    batch_count: usize,
}

impl RedactionReport {
    /// Builds a redaction report from a conjunction without leaking values.
    pub fn from_conjunction(conjunction: &EnterprisePredicateConjunction) -> Self {
        Self {
            predicate_count: conjunction.len(),
            has_authorization: conjunction.has_authorization(),
            batch_count: 0,
        }
    }

    /// Builds a redaction report from a revalidation batch without leaking IDs.
    pub fn from_batch(batch: &RevalidationBatch) -> Self {
        Self {
            predicate_count: 0,
            has_authorization: false,
            batch_count: batch.len(),
        }
    }

    /// Number of predicates in the source conjunction (zero if none).
    pub const fn predicate_count(&self) -> usize {
        self.predicate_count
    }

    /// Whether the source conjunction had an authorization predicate.
    pub const fn has_authorization(&self) -> bool {
        self.has_authorization
    }

    /// Number of candidate identifiers in the source batch (zero if none).
    pub const fn batch_count(&self) -> usize {
        self.batch_count
    }
}

impl fmt::Display for RedactionReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "RedactionReport(predicates={}, auth={}, batch={})",
            self.predicate_count, self.has_authorization, self.batch_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enterprise_predicates::predicate::EnterprisePredicate;
    use crate::enterprise_predicates::CandidateIdentifier;

    #[test]
    fn report_from_conjunction_does_not_leak_values() {
        let conjunction = EnterprisePredicateConjunction::new(vec![
            EnterprisePredicate::source("secret-source").unwrap(),
            EnterprisePredicate::tenant("secret-tenant").unwrap(),
        ])
        .unwrap();
        let report = RedactionReport::from_conjunction(&conjunction);
        let display = format!("{}", report);
        let debug = format!("{:?}", report);
        assert!(display.contains("predicates=2"));
        assert!(display.contains("auth=true"));
        assert!(!display.contains("secret-source"));
        assert!(!display.contains("secret-tenant"));
        assert!(!debug.contains("secret-source"));
        assert!(!debug.contains("secret-tenant"));
    }

    #[test]
    fn report_from_batch_does_not_leak_ids() {
        let batch = RevalidationBatch::new(vec![
            CandidateIdentifier::new("secret-id-1").unwrap(),
            CandidateIdentifier::new("secret-id-2").unwrap(),
            CandidateIdentifier::new("secret-id-3").unwrap(),
        ])
        .unwrap();
        let report = RedactionReport::from_batch(&batch);
        let display = format!("{}", report);
        let debug = format!("{:?}", report);
        assert!(display.contains("batch=3"));
        assert!(!display.contains("secret-id-1"));
        assert!(!debug.contains("secret-id-2"));
    }

    #[test]
    fn redaction_attested_label_is_stable() {
        assert_eq!(
            RedactionAttested::LABEL,
            "enterprise-predicates-redaction-v1"
        );
    }
}
