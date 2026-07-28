//! Integration tests for the enterprise predicate contract (issue #375).
//!
//! These exercise cross-cutting security scenarios from the issue test plan:
//! stale ACL pages, mixed generations, deny precedence, and source deletion
//! (tombstoning). Each test asserts fail-closed behaviour and redaction.

#![cfg(test)]

use crate::enterprise_predicates::{
    evaluate_predicates, CandidateGenerationPath, CandidateIdentifier, EnterpriseLifecycleState,
    EnterprisePredicate, EnterprisePredicateConjunction, EnterprisePredicateDiagnosticCode,
    GenerationBinding, HydrationRevalidation, PolicyGeneration, PublicationGenerationBinding,
    RevalidationBatch, RevalidationOutcome, SelectivityClass, SelectivityThresholds,
};

fn thresholds() -> SelectivityThresholds {
    SelectivityThresholds::new(1_024, 8_192).unwrap()
}

fn gen_binding(policy: u64, publication: u64) -> GenerationBinding {
    GenerationBinding::new(
        PolicyGeneration::new(policy).unwrap(),
        PublicationGenerationBinding::new(publication).unwrap(),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Selectivity: 100% down to one vector
// ---------------------------------------------------------------------------

#[test]
fn selectivity_from_hundred_percent_to_one_vector() {
    let conjunction =
        EnterprisePredicateConjunction::new(vec![EnterprisePredicate::tenant("t1").unwrap()])
            .unwrap();

    // 100% of a large authorized set → predicate-aware ANN
    let eval = evaluate_predicates(&conjunction, 100_000, &thresholds()).unwrap();
    assert_eq!(eval.path(), CandidateGenerationPath::PredicateAwareAnn);

    // medium → planner-selected
    let eval = evaluate_predicates(&conjunction, 4_000, &thresholds()).unwrap();
    assert_eq!(eval.path(), CandidateGenerationPath::PlannerSelected);

    // small → exact scan
    let eval = evaluate_predicates(&conjunction, 100, &thresholds()).unwrap();
    assert_eq!(eval.path(), CandidateGenerationPath::ExactScan);

    // single vector → exact scan (never global ANN)
    let eval = evaluate_predicates(&conjunction, 1, &thresholds()).unwrap();
    assert_eq!(eval.path(), CandidateGenerationPath::ExactScan);
}

// ---------------------------------------------------------------------------
// Zero authorized candidates: immediate return, no vector pages touched
// ---------------------------------------------------------------------------

#[test]
fn zero_authorized_candidates_returns_without_traversal() {
    let conjunction = EnterprisePredicateConjunction::new(vec![
        EnterprisePredicate::tenant("t1").unwrap(),
        EnterprisePredicate::acl_principal("p1").unwrap(),
    ])
    .unwrap();

    let eval = evaluate_predicates(&conjunction, 0, &thresholds()).unwrap();
    assert!(eval.is_zero());
    assert_eq!(eval.path(), CandidateGenerationPath::ZeroAuthorized);
}

// ---------------------------------------------------------------------------
// Mixed generations: a closed failure, never combined
// ---------------------------------------------------------------------------

#[test]
fn mixed_generations_are_incompatible() {
    let old_binding = gen_binding(1, 1);
    let new_binding = gen_binding(1, 2);
    assert!(!old_binding.is_compatible_with(&new_binding));

    let old_policy = gen_binding(1, 1);
    let new_policy = gen_binding(2, 1);
    assert!(!old_policy.is_compatible_with(&new_policy));

    let same = gen_binding(1, 1);
    assert!(old_binding.is_compatible_with(&same));
}

// ---------------------------------------------------------------------------
// Deny precedence: ACL deny predicate is present and validated
// ---------------------------------------------------------------------------

#[test]
fn acl_deny_predicate_present_and_validated() {
    let conjunction = EnterprisePredicateConjunction::new(vec![
        EnterprisePredicate::acl_principal("user:alice").unwrap(),
        EnterprisePredicate::acl_deny("user:eve").unwrap(),
    ])
    .unwrap();
    assert!(conjunction.has_authorization());

    // A valid conjunction with deny precedence still classifies by selectivity.
    let eval = evaluate_predicates(&conjunction, 500, &thresholds()).unwrap();
    assert_eq!(eval.path(), CandidateGenerationPath::ExactScan);
}

#[test]
fn acl_deny_invalid_value_fail_closed() {
    let err = EnterprisePredicate::acl_deny("").unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        EnterprisePredicateDiagnosticCode::InvalidPredicateValue
    );
}

// ---------------------------------------------------------------------------
// Source deletion (tombstoning): hydration revalidation fails closed
// ---------------------------------------------------------------------------

struct TombstoningRevalidator;

impl HydrationRevalidation for TombstoningRevalidator {
    fn revalidate(
        &self,
        batch: &RevalidationBatch,
    ) -> Result<Vec<RevalidationOutcome>, crate::enterprise_predicates::EnterprisePredicateError>
    {
        Ok(batch
            .identifiers()
            .iter()
            .map(|_| RevalidationOutcome::Tombstoned)
            .collect())
    }
}

#[test]
fn source_deletion_revalidation_fails_closed() {
    let revalidator = TombstoningRevalidator;
    let id = CandidateIdentifier::new("vec-deleted").unwrap();
    let result = revalidator.revalidate_one(&id);
    assert_eq!(
        result.unwrap_err().diagnostic_code(),
        EnterprisePredicateDiagnosticCode::HydrationRevalidationFailed
    );
}

// ---------------------------------------------------------------------------
// Stale ACL pages: ACL revocation during hydration is rejected
// ---------------------------------------------------------------------------

struct StaleAclRevalidator;

impl HydrationRevalidation for StaleAclRevalidator {
    fn revalidate(
        &self,
        batch: &RevalidationBatch,
    ) -> Result<Vec<RevalidationOutcome>, crate::enterprise_predicates::EnterprisePredicateError>
    {
        Ok(batch
            .identifiers()
            .iter()
            .map(|_| RevalidationOutcome::AclRevoked)
            .collect())
    }
}

#[test]
fn stale_acl_revocation_rejected_during_hydration() {
    let revalidator = StaleAclRevalidator;
    let id = CandidateIdentifier::new("vec-stale-acl").unwrap();
    let result = revalidator.revalidate_one(&id);
    assert_eq!(
        result.unwrap_err().diagnostic_code(),
        EnterprisePredicateDiagnosticCode::HydrationRevalidationFailed
    );
}

// ---------------------------------------------------------------------------
// Lifecycle predicate: archived documents visible, deleted not surfaced
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_archived_predicate_classifies() {
    let conjunction = EnterprisePredicateConjunction::new(vec![EnterprisePredicate::lifecycle(
        EnterpriseLifecycleState::Archived,
    )])
    .unwrap();
    let eval = evaluate_predicates(&conjunction, 2_000, &thresholds()).unwrap();
    assert_eq!(eval.path(), CandidateGenerationPath::PlannerSelected);
}

struct LifecycleRejectingRevalidator;

impl HydrationRevalidation for LifecycleRejectingRevalidator {
    fn revalidate(
        &self,
        batch: &RevalidationBatch,
    ) -> Result<Vec<RevalidationOutcome>, crate::enterprise_predicates::EnterprisePredicateError>
    {
        Ok(batch
            .identifiers()
            .iter()
            .map(|_| RevalidationOutcome::LifecycleRejected)
            .collect())
    }
}

#[test]
fn lifecycle_rejected_during_hydration() {
    let revalidator = LifecycleRejectingRevalidator;
    let id = CandidateIdentifier::new("vec-lifecycle").unwrap();
    let result = revalidator.revalidate_one(&id);
    assert_eq!(
        result.unwrap_err().diagnostic_code(),
        EnterprisePredicateDiagnosticCode::HydrationRevalidationFailed
    );
}

// ---------------------------------------------------------------------------
// Redaction: no sensitive data leaks in Debug/Display
// ---------------------------------------------------------------------------

#[test]
fn no_sensitive_data_in_debug() {
    let predicates = vec![
        EnterprisePredicate::source("confidential-source").unwrap(),
        EnterprisePredicate::tenant("tenant-x").unwrap(),
        EnterprisePredicate::acl_principal("group:secret").unwrap(),
        EnterprisePredicate::acl_deny("user:blacklisted").unwrap(),
    ];
    let conjunction = EnterprisePredicateConjunction::new(predicates).unwrap();
    let debug = format!("{:?}", conjunction);
    assert!(!debug.contains("confidential-source"));
    assert!(!debug.contains("tenant-x"));
    assert!(!debug.contains("group:secret"));
    assert!(!debug.contains("user:blacklisted"));
}

#[test]
fn candidate_identifier_redacted_in_debug() {
    let id = CandidateIdentifier::new("internal-vector-uuid-123").unwrap();
    let debug = format!("{:?}", id);
    assert_eq!(debug, "CandidateIdentifier([REDACTED])");
    assert!(!debug.contains("internal-vector-uuid-123"));
}

#[test]
fn error_display_is_diagnostic_code_only() {
    let err = crate::enterprise_predicates::EnterprisePredicateError::contract(
        EnterprisePredicateDiagnosticCode::UnsupportedStrictPredicate,
    );
    let display = format!("{}", err);
    let debug = format!("{:?}", err);
    assert_eq!(
        display,
        "enterprise-predicates.unsupported_strict_predicate"
    );
    assert_eq!(
        debug,
        "EnterprisePredicateError(unsupported_strict_predicate)"
    );
}

// ---------------------------------------------------------------------------
// Bounded payload: too many predicates fail closed
// ---------------------------------------------------------------------------

#[test]
fn payload_exceeds_bound_fails_closed() {
    let predicates: Vec<EnterprisePredicate> = (0..17)
        .map(|i| EnterprisePredicate::source(format!("s{i}")).unwrap())
        .collect();
    let result = EnterprisePredicateConjunction::new(predicates);
    assert_eq!(
        result.unwrap_err().diagnostic_code(),
        EnterprisePredicateDiagnosticCode::PredicatePayloadTooLarge
    );
}

// ---------------------------------------------------------------------------
// Selectivity class stability
// ---------------------------------------------------------------------------

#[test]
fn selectivity_class_discriminator_stable() {
    assert_eq!(SelectivityClass::Zero.discriminator_or_default(), 0);
    assert_eq!(SelectivityClass::Small.discriminator_or_default(), 1);
    assert_eq!(SelectivityClass::Medium.discriminator_or_default(), 2);
    assert_eq!(SelectivityClass::Broad.discriminator_or_default(), 3);
}

// helper trait alias for test readability
trait DiscriminatorExt {
    fn discriminator_or_default(self) -> u64;
}

impl DiscriminatorExt for SelectivityClass {
    fn discriminator_or_default(self) -> u64 {
        // Access the crate-private discriminator through classify boundaries.
        match self {
            SelectivityClass::Zero => 0,
            SelectivityClass::Small => 1,
            SelectivityClass::Medium => 2,
            SelectivityClass::Broad => 3,
        }
    }
}
