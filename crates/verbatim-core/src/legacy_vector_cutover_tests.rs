//! Focused tests for the legacy SQLite/HNSW serving cutover contract (Refs #388).

use crate::legacy_vector_cutover::{
    authorize_retirement, AuthoritativeVectorSource, CutoverGates, CutoverManifest,
    CutoverManifestFields, GateClass, LegacyArtifact, LegacyArtifactRemovalPlan, LegacyPath,
    LegacyRetirementDiagnosticCode, MigrationValidation, MigrationValidationFields,
    PublicationGeneration, RemainingServingCapabilities, RollbackWindow, ShadowComparison,
    ShadowComparisonState,
};

fn every_gate() -> Vec<GateClass> {
    GateClass::ALL.to_vec()
}

fn generations() -> (PublicationGeneration, PublicationGeneration) {
    (
        PublicationGeneration::new("legacy-17").expect("legacy generation"),
        PublicationGeneration::new("diskann3-18").expect("candidate generation"),
    )
}

fn valid_migration_fields() -> MigrationValidationFields {
    MigrationValidationFields {
        counts_and_hashes_valid: true,
        metric_profile_generation_valid: true,
        full_dimension_preserved: true,
        normalization_and_ids_valid: true,
        filters_valid: true,
        sampled_exact_recall_valid: true,
        mutation_recovery_valid: true,
        resource_evidence_valid: true,
        publication_manifest_valid: true,
    }
}

fn valid_validation() -> MigrationValidation {
    MigrationValidation::new(valid_migration_fields()).expect("complete migration validation")
}

fn passing_shadow() -> ShadowComparison {
    let (incumbent, candidate) = generations();
    ShadowComparison::new(incumbent, candidate, ShadowComparisonState::Passed)
}

fn removal_plan() -> LegacyArtifactRemovalPlan {
    LegacyArtifactRemovalPlan::new(
        true,
        vec![
            LegacyArtifact::SerializedHnsw,
            LegacyArtifact::StaleVectorJson,
        ],
    )
    .expect("backup-aware removal plan")
}

fn remaining_capabilities() -> RemainingServingCapabilities {
    RemainingServingCapabilities::new(true, true, true, true).expect("required capabilities remain")
}

#[test]
fn legacy_vector_cutover_happy_path_authorizes_retirement_after_rollback_window() {
    let authorization = authorize_retirement(
        &CutoverGates::new(every_gate()),
        &valid_validation(),
        &passing_shadow(),
        RollbackWindow::new(100, 200).expect("valid rollback window"),
        200,
        &removal_plan(),
        &remaining_capabilities(),
    )
    .expect("all gates and the rollback window authorize retirement");

    assert_eq!(authorization.candidate_generation().as_str(), "diskann3-18");
    assert!(authorization.legacy_artifacts_may_be_removed());
}

#[test]
fn legacy_vector_cutover_compile_only_diskann3_is_not_a_gate() {
    let error = CutoverGates::compile_only()
        .require_complete()
        .expect_err("compile-only is insufficient");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::DiskAnn3CompileOnly
    );
}

#[test]
fn legacy_vector_cutover_every_missing_gate_class_fails_closed_with_its_code() {
    for gate in GateClass::ALL {
        let satisfied: Vec<_> = every_gate()
            .into_iter()
            .filter(|candidate| *candidate != gate)
            .collect();
        let error = CutoverGates::new(satisfied)
            .require_complete()
            .expect_err("missing gate must fail closed");
        assert_eq!(error.diagnostic_code(), gate.missing_diagnostic_code());
    }
}

#[test]
fn legacy_vector_cutover_incomplete_shadow_cannot_promote() {
    let (incumbent, candidate) = generations();
    let error = ShadowComparison::new(incumbent, candidate, ShadowComparisonState::Incomplete)
        .require_promotable()
        .expect_err("incomplete shadow is not promotable");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::ShadowComparisonIncomplete
    );
}

#[test]
fn legacy_vector_cutover_failed_shadow_cannot_promote() {
    let (incumbent, candidate) = generations();
    let error = ShadowComparison::new(incumbent, candidate, ShadowComparisonState::Failed)
        .require_promotable()
        .expect_err("failed shadow is not promotable");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::ShadowComparisonFailed
    );
}

#[test]
fn legacy_vector_cutover_reembedding_is_explicit_when_profile_or_bytes_are_invalid() {
    let source = AuthoritativeVectorSource::new(false, true);
    assert!(source.requires_reembedding());
    let error = source
        .require_reusable_bytes()
        .expect_err("silent re-embedding forbidden");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::SilentReembeddingForbidden
    );

    assert!(!AuthoritativeVectorSource::new(true, true).requires_reembedding());
}

#[test]
fn legacy_vector_cutover_rollback_window_blocks_artifact_removal() {
    let error = authorize_retirement(
        &CutoverGates::new(every_gate()),
        &valid_validation(),
        &passing_shadow(),
        RollbackWindow::new(100, 200).expect("valid rollback window"),
        199,
        &removal_plan(),
        &remaining_capabilities(),
    )
    .expect_err("legacy must be retained during rollback window");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::RollbackWindowActive
    );
}

#[test]
fn legacy_vector_cutover_expired_window_and_backup_policy_authorize_removal() {
    let plan = removal_plan();
    assert!(plan.removes(LegacyArtifact::SerializedHnsw));
    assert!(plan.removes(LegacyArtifact::StaleVectorJson));
    assert!(RollbackWindow::new(1, 2).expect("window").has_elapsed(2));
}

#[test]
fn legacy_vector_cutover_authoritative_vectors_and_exact_scan_remain_available() {
    let capabilities = remaining_capabilities();
    assert!(capabilities.authoritative_vectors_remain());
    assert!(capabilities.exact_small_scope_scan_remains());
}

#[test]
fn legacy_vector_cutover_explicitly_enumerates_legacy_serving_paths() {
    assert_eq!(
        LegacyPath::ALL,
        [
            LegacyPath::SqliteLowMemoryWholeTableScan,
            LegacyPath::ResidentHnswInstantDistance,
            LegacyPath::UnconditionalLocalPreSearch,
        ]
    );
}

#[test]
fn legacy_vector_cutover_durable_manifest_rejects_invalid_serde() {
    let error = serde_json::from_str::<CutoverManifest>(
        r#"{"schema_version":1,"incumbent_generation":"","candidate_generation":"diskann3-18","rollback_window_end":200}"#,
    )
    .expect_err("empty generation must not deserialize");
    assert!(error.to_string().contains("invalid_identity"));

    let error = serde_json::from_str::<CutoverManifest>(
        r#"{"schema_version":9,"incumbent_generation":"legacy-17","candidate_generation":"diskann3-18","rollback_window_end":200}"#,
    )
    .expect_err("unknown schema must not deserialize");
    assert!(error.to_string().contains("invalid_manifest"));
}

#[test]
fn legacy_vector_cutover_diagnostics_are_code_only() {
    let error = CutoverGates::compile_only()
        .require_complete()
        .expect_err("compile-only fails");
    assert_eq!(
        error.to_string(),
        "legacy-vector-cutover.diskann3_compile_only"
    );
    assert_eq!(
        format!("{error:?}"),
        "LegacyRetirementError(diskann3_compile_only)"
    );
}

#[test]
fn legacy_vector_cutover_shadow_and_promotion_bind_the_same_generation() {
    let (incumbent, candidate) = generations();
    let wrong_candidate = PublicationGeneration::new("diskann3-19").expect("wrong generation");
    let error = ShadowComparison::new(incumbent, wrong_candidate, ShadowComparisonState::Passed)
        .bind_promotion(&candidate)
        .expect_err("promotion must bind the shadow candidate generation");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::PromotionGenerationMismatch
    );
}

#[test]
fn legacy_vector_cutover_rejects_dimension_reduction_and_incomplete_migration_validation() {
    let error = MigrationValidation::new(MigrationValidationFields {
        full_dimension_preserved: false,
        ..valid_migration_fields()
    })
    .expect_err("dimension reduction is forbidden");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::DimensionReductionForbidden
    );

    let error = MigrationValidation::new(MigrationValidationFields {
        metric_profile_generation_valid: false,
        ..valid_migration_fields()
    })
    .expect_err("every validation artifact is required");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::MigrationValidationIncomplete
    );
}

#[test]
fn legacy_vector_cutover_removal_requires_backup_and_both_legacy_artifacts() {
    let error = LegacyArtifactRemovalPlan::new(false, vec![LegacyArtifact::SerializedHnsw])
        .expect_err("backup-aware maintenance is mandatory");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::BackupRequired
    );

    let error = LegacyArtifactRemovalPlan::new(true, vec![LegacyArtifact::SerializedHnsw])
        .expect_err("stale vector JSON removal needs explicit planning");
    assert_eq!(
        error.diagnostic_code(),
        LegacyRetirementDiagnosticCode::LegacyArtifactPlanIncomplete
    );
}

#[test]
fn legacy_vector_cutover_manifest_uses_constructor_validation() {
    let manifest = CutoverManifest::new(CutoverManifestFields {
        schema_version: 1,
        incumbent_generation: "legacy-17".to_string(),
        candidate_generation: "diskann3-18".to_string(),
        rollback_window_end: 200,
    })
    .expect("valid manifest");
    assert_eq!(manifest.candidate_generation().as_str(), "diskann3-18");
}
