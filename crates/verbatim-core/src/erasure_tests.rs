use super::*;

fn canonical_plan() -> DeletionPlan {
    let matrix = DeletionPropagationMatrix::canonical();
    let scope = DeletionScope::new(vec!["source-363".into()], &matrix).expect("valid scope");
    DeletionPlan::new(scope, DeletionPolicy::default(), matrix).expect("valid plan")
}

fn assert_code(error: ErasureError, expected: ErasureDiagnosticCode) {
    assert_eq!(error, ErasureError::validation(expected));
    assert_eq!(error.to_string(), format!("erasure.{}", expected.as_str()));
    assert_eq!(
        format!("{error:?}"),
        format!("ErasureError({})", expected.as_str())
    );
}

#[test]
fn canonical_matrix_covers_every_target_and_enforces_authoritative_derived_cache_remote_order() {
    let matrix = DeletionPropagationMatrix::canonical();
    matrix.validate().expect("canonical matrix is valid");
    assert_eq!(matrix.entries().len(), DeletionTarget::ALL.len());

    let orders: Vec<_> = matrix
        .entries()
        .iter()
        .map(|entry| entry.ordering)
        .collect();
    assert!(orders.windows(2).all(|window| window[0] <= window[1]));
    assert_eq!(
        matrix.entries()[0].ordering,
        DeletionOrdering::Authoritative
    );
    assert_eq!(
        matrix.entries().last().expect("non-empty matrix").ordering,
        DeletionOrdering::RemoteReplica
    );
}

#[test]
fn every_product_target_state_combination_is_validated_fail_closed() {
    let canonical = DeletionPropagationMatrix::canonical();
    for target in DeletionTarget::ALL {
        let canonical_entry = canonical.entry(target).expect("target is covered");
        for product in DataProduct::ALL {
            for state in DeletionState::ALL {
                let mut entries = canonical.entries().to_vec();
                let position = entries
                    .iter()
                    .position(|entry| entry.target == target)
                    .expect("target is covered");
                entries[position].product = product;
                entries[position].state = state;
                let candidate = DeletionPropagationMatrix::new(entries);
                assert_eq!(
                    candidate.validate().is_ok(),
                    product == canonical_entry.product && state == canonical_entry.state,
                    "{target:?} {product:?} {state:?}"
                );
            }
        }
    }
}

#[test]
fn deletion_scope_fences_stale_reads_and_validates_all_target_classifications() {
    let matrix = DeletionPropagationMatrix::canonical();
    let scope = DeletionScope::new(vec!["source-363".into()], &matrix).expect("valid scope");
    assert!(scope.blocks_serving());
    scope.validate(&matrix).expect("scope tracks every target");

    let mut bad_scope = scope.clone();
    bad_scope.targets.remove(&DeletionTarget::Qdrant);
    assert_code(
        bad_scope
            .validate(&matrix)
            .expect_err("partial scope rejected"),
        ErasureDiagnosticCode::ScopeTargetSetIncomplete,
    );
}

#[test]
fn scope_and_plan_debug_redact_source_identifiers() {
    let restricted_source_id = "restricted-source-id-363";
    let matrix = DeletionPropagationMatrix::canonical();
    let scope =
        DeletionScope::new(vec![restricted_source_id.into()], &matrix).expect("valid scope");
    let plan =
        DeletionPlan::new(scope.clone(), DeletionPolicy::default(), matrix).expect("valid plan");

    let scope_debug = format!("{scope:?}");
    assert!(scope_debug.contains("source_id_count: 1"));
    assert!(!scope_debug.contains(restricted_source_id));

    let plan_debug = format!("{plan:?}");
    assert!(plan_debug.contains("DeletionPlan"));
    assert!(!plan_debug.contains(restricted_source_id));
}

#[test]
fn deletion_state_transition_matrix_is_exhaustive() {
    for from in DeletionState::ALL {
        for to in DeletionState::ALL {
            let expected = matches!(
                (from, to),
                (DeletionState::LogicalDelete, DeletionState::Quarantine)
                    | (DeletionState::LogicalDelete, DeletionState::Tombstone)
                    | (DeletionState::Quarantine, DeletionState::Tombstone)
                    | (
                        DeletionState::Quarantine,
                        DeletionState::ImmediatePhysicalErase
                    )
                    | (
                        DeletionState::Tombstone,
                        DeletionState::ImmediatePhysicalErase
                    )
                    | (DeletionState::Tombstone, DeletionState::DelayedBackupExpiry)
            );
            assert_eq!(
                from.can_transition_to(to),
                expected,
                "unexpected transition from {from:?} to {to:?}"
            );
        }
    }
}

#[test]
fn legal_hold_and_incomplete_propagation_fail_closed() {
    let matrix = DeletionPropagationMatrix::canonical();
    let scope = DeletionScope::new(vec!["source-363".into()], &matrix).expect("valid scope");

    let held_policy = DeletionPolicy {
        legal_hold: true,
        ..DeletionPolicy::default()
    };
    assert_code(
        DeletionPlan::new(scope.clone(), held_policy, matrix.clone())
            .expect_err("legal hold blocks all deletion"),
        ErasureDiagnosticCode::LegalHoldBlocksDeletion,
    );

    let incomplete_policy = DeletionPolicy {
        propagation: PolicyPropagation {
            cache_keys: false,
            ..PolicyPropagation::required()
        },
        ..DeletionPolicy::default()
    };
    assert_code(
        DeletionPlan::new(scope, incomplete_policy, matrix).expect_err("cache fence is mandatory"),
        ErasureDiagnosticCode::PolicyPropagationIncomplete,
    );
}

#[test]
fn remote_failures_must_enter_dead_letter_with_operator_alert() {
    let retry = RetryPolicy::default();
    let reconciliation = RemoteReconciliation::remote_failure(DeletionTarget::Qdrant, retry)
        .expect("remote failure is tracked");
    reconciliation.validate().expect("dead letter is required");
    assert_eq!(reconciliation.dead_letter, DeadLetterState::Enqueued);
    assert_eq!(reconciliation.operator_alert, OperatorAlertState::Required);

    let invalid = RemoteReconciliation {
        dead_letter: DeadLetterState::NotRequired,
        ..reconciliation
    };
    assert_code(
        invalid.validate().expect_err("failure cannot be dropped"),
        ErasureDiagnosticCode::RemoteFailureDeadLetterRequired,
    );
}

#[test]
fn pending_remote_propagation_requires_matching_dead_letter_reconciliation() {
    let plan = canonical_plan();
    let pending_remote = PropagationReceipt::with_pending_remote(
        &plan,
        [DeletionTarget::Qdrant].into_iter().collect(),
    )
    .expect("Qdrant may await reconciliation");

    assert_code(
        ReconciliationReceipt::new(
            &plan,
            pending_remote,
            RemoteReconciliation::complete(RetryPolicy::default()),
        )
        .expect_err("pending remote work cannot be reconciled as complete"),
        ErasureDiagnosticCode::ReconciliationMismatch,
    );
}

#[test]
fn cryptographic_erasure_requires_key_rotation_when_backup_rewrite_is_impractical() {
    let invalid = CryptographicErasure {
        backup_rewrite_impractical: true,
        key_rotation: KeyRotationRequirement::NotApplicable,
    };
    assert_code(
        invalid.validate().expect_err("key rotation is mandatory"),
        ErasureDiagnosticCode::CryptographicErasureKeyRotationRequired,
    );
    CryptographicErasure::default()
        .validate()
        .expect("default applies key rotation requirement");
}

#[test]
fn scope_matrix_propagation_reconciliation_and_plan_json_round_trip() {
    let plan = canonical_plan();
    let bytes = encode_deletion_plan_json(&plan).expect("plan encodes");
    let decoded = decode_deletion_plan_json(&bytes).expect("plan decodes and revalidates");
    assert_eq!(decoded, plan);

    let propagation = PropagationReceipt::complete(&decoded).expect("ordered propagation");
    let reconciliation = ReconciliationReceipt::new(
        &decoded,
        propagation,
        RemoteReconciliation::complete(RetryPolicy::default()),
    )
    .expect("reconciliation is valid");
    let proof = DeletionProof::from_reconciliation(&decoded, &reconciliation)
        .expect("proof is redaction safe and verifiable");
    proof.validate().expect("proof validates");
    assert_eq!(proof.source_count, 1);
}

#[test]
fn deletion_proof_excludes_restricted_content_and_errors_render_only_codes() {
    let restricted_content = "restricted source body must never appear in proof or errors";
    let plan = canonical_plan();
    let reconciliation = ReconciliationReceipt::new(
        &plan,
        PropagationReceipt::complete(&plan).expect("complete propagation"),
        RemoteReconciliation::complete(RetryPolicy::default()),
    )
    .expect("valid reconciliation");
    let proof = DeletionProof::from_reconciliation(&plan, &reconciliation).expect("proof");
    let encoded = serde_json::to_string(&proof).expect("proof serializes");
    assert!(!encoded.contains(restricted_content));
    assert!(!format!("{proof:?}").contains(restricted_content));

    assert_code(
        decode_deletion_plan_json(b"not-json").expect_err("untrusted JSON is rejected"),
        ErasureDiagnosticCode::InvalidPlanJson,
    );
}

#[test]
fn workflow_trait_only_allows_atomic_execute() {
    struct ContractWorkflow;

    impl DeletionWorkflow for ContractWorkflow {
        fn execute(
            &self,
            scope: DeletionScope,
            policy: DeletionPolicy,
        ) -> ErasureResult<DeletionProof> {
            let plan = DeletionPlan::new(scope, policy, DeletionPropagationMatrix::canonical())?;
            let propagation = PropagationReceipt::complete(&plan)?;
            let reconciliation = ReconciliationReceipt::new(
                &plan,
                propagation,
                RemoteReconciliation::complete(RetryPolicy::default()),
            )?;
            DeletionProof::from_reconciliation(&plan, &reconciliation)
        }
    }

    let matrix = DeletionPropagationMatrix::canonical();
    let scope = DeletionScope::new(vec!["source-363".into()], &matrix).expect("valid scope");
    let workflow = ContractWorkflow;
    let proof = workflow
        .execute(scope, DeletionPolicy::default())
        .expect("atomic workflow execution");
    proof.validate().expect("proof validates");
}
