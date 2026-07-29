use crate::durable_updates::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn gen(n: u64) -> DurableGeneration {
    DurableGeneration::new(n).expect("valid generation")
}

fn vid(n: u64) -> DurableVectorId {
    DurableVectorId::new(n).expect("valid vector id")
}

fn ver(n: u32) -> MutationVersion {
    MutationVersion::new(n).expect("valid version")
}

fn hash(suffix: char) -> ContentHash {
    // 64 hex chars; suffix-varied so distinct hashes are distinct in tests.
    let mut hex = String::from("sha256:");
    for _ in 0..64 {
        hex.push(suffix);
    }
    ContentHash::new(&hex).expect("valid content hash")
}

fn key(suffix: &str) -> MutationIdempotencyKey {
    MutationIdempotencyKey::new(format!("op-key-{suffix}")).expect("valid idempotency key")
}

fn upsert_op(id: u64, version: u32, h: char) -> MutationOperation {
    MutationOperation::upsert(vid(id), ver(version), hash(h))
}

// ===========================================================================
// Identity: DurableGeneration, DurableVectorId, MutationVersion, ContentHash
// ===========================================================================

#[test]
fn generation_rejects_zero_through_constructor_and_serde() {
    assert_eq!(
        DurableGeneration::new(0)
            .expect_err("zero generation fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidIdentity
    );
    // Serde must route through the constructor so zero cannot bypass validation.
    let err = serde_json::from_str::<DurableGeneration>("0").expect_err("serde rejects zero");
    assert!(err.to_string().contains("invalid_identity") || err.is_data());
    assert_eq!(gen(5).value(), 5);
}

#[test]
fn generation_total_order() {
    assert!(gen(1) < gen(2));
    assert!(gen(2) < gen(10));
    assert_eq!(gen(3), gen(3));
}

#[test]
fn vector_id_rejects_zero_and_overflow() {
    assert_eq!(
        DurableVectorId::new(0)
            .expect_err("zero id fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidIdentity
    );
    assert_eq!(vid(1).value(), 1);
    assert_eq!(vid(u64::MAX).value(), u64::MAX);
}

#[test]
fn version_rejects_zero_and_orders() {
    assert_eq!(
        MutationVersion::new(0)
            .expect_err("zero version fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidIdentity
    );
    assert!(ver(1) < ver(2));
    assert_eq!(ver(7), ver(7));
}

#[test]
fn content_hash_rejects_malformed() {
    for invalid in [
        "",
        "abc",
        "sha256:short",
        "SHA256:abcd",
        "sha256:GG".repeat(32).as_str(),
    ] {
        assert_eq!(
            ContentHash::new(invalid)
                .expect_err("invalid hash fails closed")
                .diagnostic_code(),
            DurableUpdateDiagnosticCode::InvalidIdentity
        );
    }
    ContentHash::new(&format!("sha256:{}", "a".repeat(64))).expect("valid lowercase hex");
}

#[test]
fn idempotency_key_rejects_invalid_and_redacts_debug() {
    for invalid in ["", &"x".repeat(129), "héllo", "has\ttab"] {
        assert_eq!(
            MutationIdempotencyKey::new(invalid)
                .expect_err("invalid key fails closed")
                .diagnostic_code(),
            DurableUpdateDiagnosticCode::InvalidIdentity
        );
    }
    let k = key("abc");
    assert!(!format!("{k:?}").contains("op-key-abc"));
    assert!(format!("{k:?}").contains("REDACTED"));
    assert_eq!(k.as_str(), "op-key-abc");
}

// ===========================================================================
// Mutation operations and kinds
// ===========================================================================

#[test]
fn mutation_kinds_construct_correctly() {
    let upsert = MutationOperation::upsert(vid(1), ver(1), hash('a'));
    assert_eq!(upsert.kind(), MutationKind::Upsert);
    assert!(upsert.content_hash().is_some());

    let delete = MutationOperation::delete(vid(2), ver(1));
    assert_eq!(delete.kind(), MutationKind::Delete);
    assert!(delete.content_hash().is_none());

    let tomb = MutationOperation::tombstone(vid(3), ver(1));
    assert_eq!(tomb.kind(), MutationKind::Tombstone);
    assert!(tomb.content_hash().is_none());

    let replace = MutationOperation::source_replace(vid(4), ver(1), hash('b'));
    assert_eq!(replace.kind(), MutationKind::SourceReplace);
    assert!(replace.content_hash().is_some());
}

// ===========================================================================
// MutationBatch: bounded, idempotent, version-ordered
// ===========================================================================

#[test]
fn batch_rejects_empty_and_over_capacity() {
    assert_eq!(
        MutationBatch::new(gen(1), key("e"), vec![])
            .expect_err("empty batch fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidMutationBatch
    );

    let ops: Vec<MutationOperation> = (1..=MutationBatch::MAX_OPERATIONS + 1)
        .map(|i| upsert_op(i as u64, 1, 'a'))
        .collect();
    assert_eq!(
        MutationBatch::new(gen(1), key("big"), ops)
            .expect_err("over-capacity batch fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidMutationBatch
    );
}

#[test]
fn batch_rejects_duplicate_vector_id() {
    let ops = vec![
        MutationOperation::upsert(vid(1), ver(1), hash('a')),
        MutationOperation::delete(vid(1), ver(2)),
    ];
    assert_eq!(
        MutationBatch::new(gen(1), key("dup"), ops)
            .expect_err("duplicate id fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::DuplicateMutationVectorId
    );
}

#[test]
fn batch_accepts_valid_distinct_operations() {
    let ops = vec![
        MutationOperation::upsert(vid(1), ver(1), hash('a')),
        MutationOperation::delete(vid(2), ver(1)),
        MutationOperation::tombstone(vid(3), ver(1)),
    ];
    let batch = MutationBatch::new(gen(1), key("ok"), ops).expect("valid batch");
    assert_eq!(batch.operations().len(), 3);
    assert_eq!(batch.generation(), gen(1));
    // Debug must not leak idempotency key value.
    assert!(!format!("{batch:?}").contains("op-key-ok"));
    assert!(format!("{batch:?}").contains("operation_count"));
}

#[test]
fn batch_debug_redacts_key() {
    let batch =
        MutationBatch::new(gen(1), key("secret"), vec![upsert_op(1, 1, 'a')]).expect("valid batch");
    let debug = format!("{batch:?}");
    assert!(!debug.contains("secret"));
    assert!(debug.contains("REDACTED"));
}

// ===========================================================================
// Version ordering against committed state
// ===========================================================================

#[test]
fn batch_rejects_version_regression() {
    let mut committed = std::collections::BTreeMap::new();
    committed.insert(vid(1), ver(5));

    let ops = vec![MutationOperation::upsert(vid(1), ver(3), hash('b'))];
    let batch = MutationBatch::new(gen(1), key("regress"), ops).expect("valid batch");
    assert_eq!(
        batch
            .validate_against_committed(&committed)
            .expect_err("regression fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::VersionOutOfOrder
    );
}

#[test]
fn batch_accepts_equal_or_greater_version() {
    let mut committed = std::collections::BTreeMap::new();
    committed.insert(vid(1), ver(5));

    let ops = vec![MutationOperation::upsert(vid(1), ver(5), hash('b'))];
    let batch = MutationBatch::new(gen(1), key("eq"), ops).expect("valid batch");
    batch
        .validate_against_committed(&committed)
        .expect("equal version ok");

    let ops = vec![MutationOperation::upsert(vid(1), ver(6), hash('c'))];
    let batch = MutationBatch::new(gen(1), key("gt"), ops).expect("valid batch");
    batch
        .validate_against_committed(&committed)
        .expect("greater version ok");
}

// ===========================================================================
// MutationStage durability and publication flags
// ===========================================================================

#[test]
fn stage_durability_and_publication_flags() {
    assert!(!MutationStage::OperationLogged.is_durable());
    assert!(!MutationStage::VectorUpserted.is_durable());
    assert!(!MutationStage::Tombstoned.is_durable());
    assert!(!MutationStage::GraphEdgeUpdated.is_durable());
    assert!(!MutationStage::FilterIndexUpdated.is_durable());
    assert!(MutationStage::Checkpointed.is_durable());
    assert!(MutationStage::Compacted.is_durable());
    assert!(MutationStage::Validated.is_durable());
    assert!(MutationStage::Published.is_durable());

    assert!(!MutationStage::Checkpointed.is_published());
    assert!(MutationStage::Published.is_published());
}

// ===========================================================================
// TombstoneSet: generation/version awareness, cap, pre-hydration exclusion
// ===========================================================================

#[test]
fn tombstone_excludes_before_hydration_in_matching_generation() {
    let mut set = TombstoneSet::new();
    set.record(Tombstone::new(vid(1), gen(2), ver(3)))
        .expect("record ok");

    // Tombstone in gen 2, candidate indexed at version 2: excluded in gen >= 2.
    assert!(set.is_excluded(vid(1), gen(2), ver(2)));
    assert!(set.is_excluded(vid(1), gen(3), ver(2)));
    // A search in gen 1 (older) does not see the gen-2 tombstone.
    assert!(!set.is_excluded(vid(1), gen(1), ver(2)));
    // Candidate whose index version is newer than the tombstone version survives.
    assert!(!set.is_excluded(vid(1), gen(2), ver(4)));
    // Untombstoned id is never excluded.
    assert!(!set.is_excluded(vid(2), gen(2), ver(1)));
}

#[test]
fn tombstone_exclude_batch_removes_tombstoned_candidates() {
    let mut set = TombstoneSet::new();
    set.record(Tombstone::new(vid(1), gen(1), ver(1)))
        .expect("record ok");
    set.record(Tombstone::new(vid(3), gen(1), ver(1)))
        .expect("record ok");

    let candidates = vec![(vid(1), ver(1)), (vid(2), ver(1)), (vid(3), ver(1))];
    let survivors = set.exclude_tombstoned(&candidates, gen(1));
    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0].0, vid(2));
}

#[test]
fn tombstone_record_is_idempotent_for_same_generation_version() {
    let mut set = TombstoneSet::new();
    let t = Tombstone::new(vid(1), gen(1), ver(1));
    set.record(t).expect("first record ok");
    set.record(t).expect("idempotent re-record ok");
    assert_eq!(set.len(), 1);
}

#[test]
fn tombstone_rejects_version_regression() {
    let mut set = TombstoneSet::new();
    set.record(Tombstone::new(vid(1), gen(1), ver(5)))
        .expect("record v5 ok");
    assert_eq!(
        set.record(Tombstone::new(vid(1), gen(1), ver(2)))
            .expect_err("older version fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::VersionOutOfOrder
    );
}

#[test]
fn tombstone_cap_enforced() {
    let mut set = TombstoneSet::with_cap(3);
    for i in 1..=3 {
        set.record(Tombstone::new(vid(i), gen(1), ver(1)))
            .expect("record ok");
    }
    assert!(set.is_at_cap());
    assert_eq!(
        set.record(Tombstone::new(vid(4), gen(1), ver(1)))
            .expect_err("cap exceeded fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::TombstoneCapExceeded
    );
    // Re-recording an existing id does not trip the cap.
    set.record(Tombstone::new(vid(1), gen(1), ver(2)))
        .expect("update existing ok");
}

#[test]
fn tombstone_set_default_and_empty() {
    let set = TombstoneSet::default();
    assert!(set.is_empty());
    assert_eq!(set.cap(), TombstoneSet::DEFAULT_CAP);
}

// ===========================================================================
// CompactionTrigger: measured signals, thresholds, no time-only trigger
// ===========================================================================

#[test]
fn compaction_trigger_dead_byte_ratio() {
    let thresholds = CompactionThresholds::DEFAULT;
    let trigger =
        CompactionTrigger::new(0.25, 1.0, 100, 1_000.0, thresholds).expect("valid trigger");
    assert!(
        trigger.should_compact(),
        "dead byte ratio exceeds threshold"
    );
}

#[test]
fn compaction_trigger_read_amplification() {
    let thresholds = CompactionThresholds::DEFAULT;
    let trigger =
        CompactionTrigger::new(0.05, 5.0, 100, 1_000.0, thresholds).expect("valid trigger");
    assert!(
        trigger.should_compact(),
        "read amplification exceeds threshold"
    );
}

#[test]
fn compaction_trigger_update_volume() {
    let thresholds = CompactionThresholds::DEFAULT;
    let trigger =
        CompactionTrigger::new(0.05, 1.0, 60_000, 1_000.0, thresholds).expect("valid trigger");
    assert!(trigger.should_compact(), "update volume exceeds threshold");
}

#[test]
fn compaction_trigger_latency() {
    let thresholds = CompactionThresholds::DEFAULT;
    let trigger =
        CompactionTrigger::new(0.05, 1.0, 100, 60_000.0, thresholds).expect("valid trigger");
    assert!(trigger.should_compact(), "latency exceeds threshold");
}

#[test]
fn compaction_trigger_no_signal_below_thresholds() {
    let thresholds = CompactionThresholds::DEFAULT;
    let trigger =
        CompactionTrigger::new(0.05, 1.0, 100, 1_000.0, thresholds).expect("valid trigger");
    assert!(
        !trigger.should_compact(),
        "no signal exceeds threshold; time alone is insufficient"
    );
}

#[test]
fn compaction_trigger_rejects_non_finite_and_invalid_ratio() {
    let thresholds = CompactionThresholds::DEFAULT;
    assert_eq!(
        CompactionTrigger::new(f64::NAN, 1.0, 100, 1_000.0, thresholds)
            .expect_err("nan fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidCompactionPlan
    );
    assert_eq!(
        CompactionTrigger::new(1.5, 1.0, 100, 1_000.0, thresholds)
            .expect_err("ratio > 1 fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidCompactionPlan
    );
}

#[test]
fn compaction_thresholds_validate_rejects_bad_values() {
    let bad = CompactionThresholds {
        dead_byte_ratio: -0.1,
        read_amplification: 4.0,
        update_volume: 50_000,
        p99_latency_us: 50_000.0,
    };
    assert_eq!(
        bad.validate()
            .expect_err("negative threshold fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidCompactionPlan
    );

    let bad2 = CompactionThresholds {
        dead_byte_ratio: 1.5,
        read_amplification: 4.0,
        update_volume: 50_000,
        p99_latency_us: 50_000.0,
    };
    assert_eq!(
        bad2.validate()
            .expect_err("ratio > 1 threshold fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidCompactionPlan
    );
}

// ===========================================================================
// CompactionPlan: resumable, restart-safe, staged immutable artifact
// ===========================================================================

#[test]
fn compaction_plan_rejects_non_monotonic_generations() {
    assert_eq!(
        CompactionPlan::new(gen(5), gen(3), CompactionStage::Pending, false)
            .expect_err("target <= source fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidCompactionPlan
    );
    assert_eq!(
        CompactionPlan::new(gen(5), gen(5), CompactionStage::Pending, false)
            .expect_err("equal generations fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::InvalidCompactionPlan
    );
}

#[test]
fn compaction_plan_staged_requires_fsync() {
    assert_eq!(
        CompactionPlan::new(gen(1), gen(2), CompactionStage::Staged, false)
            .expect_err("staged without fsync fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::CheckpointNotDurable
    );
    let plan = CompactionPlan::new(gen(1), gen(2), CompactionStage::Staged, true)
        .expect("valid staged plan");
    assert!(plan.is_ready_to_publish());
}

#[test]
fn compaction_plan_not_ready_until_staged() {
    let plan = CompactionPlan::new(gen(1), gen(2), CompactionStage::Validating, false)
        .expect("valid plan");
    assert!(!plan.is_ready_to_publish());
}

#[test]
fn compaction_stage_flags() {
    assert!(CompactionStage::Staged.is_staged());
    assert!(CompactionStage::Complete.is_staged());
    assert!(!CompactionStage::Streaming.is_staged());
    assert!(CompactionStage::Complete.is_complete());
    assert!(!CompactionStage::Staged.is_complete());
}

// ===========================================================================
// MutationLease: generation/query lease, reclamation gating
// ===========================================================================

#[test]
fn lease_expiry_and_ordering() {
    let l1 = MutationLease::new(gen(1), 100);
    let l2 = MutationLease::new(gen(1), 200);
    assert!(l1 < l2);
    assert!(!l1.is_expired(50));
    assert!(!l1.is_expired(99));
    assert!(l1.is_expired(100));
    assert!(l1.is_expired(150));
}

#[test]
fn reclamation_blocked_by_active_lease() {
    let leases = vec![MutationLease::new(gen(1), 100)];
    assert_eq!(
        can_reclaim_generation(gen(1), &leases, 50)
            .expect_err("active lease blocks reclamation")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::LeaseActive
    );
}

#[test]
fn reclamation_allowed_after_lease_expiry() {
    let leases = vec![MutationLease::new(gen(1), 100)];
    can_reclaim_generation(gen(1), &leases, 100).expect("expired lease allows reclamation");
    can_reclaim_generation(gen(1), &leases, 200).expect("past expiry allows reclamation");
}

#[test]
fn reclamation_allowed_when_no_lease_protects_generation() {
    let leases = vec![MutationLease::new(gen(1), 100)];
    can_reclaim_generation(gen(2), &leases, 50).expect("other generation unprotected");
}

// ===========================================================================
// CrashRecoveryResult: previous committed, new committed, inconsistent rejected
// ===========================================================================

#[test]
fn recovery_published_with_fsync_yields_new_committed() {
    let result = CrashRecoveryResult::decide(
        gen(2),
        MutationStage::Published,
        RecoveryFsyncAttestation::new(true, true),
        gen(1),
    );
    assert!(matches!(
        result,
        CrashRecoveryResult::NewCommitted { generation, .. } if generation == gen(2)
    ));
    assert!(result.is_consistent());
    assert!(!result.is_rejected());
}

#[test]
fn recovery_published_without_fsync_is_rejected() {
    let result = CrashRecoveryResult::decide(
        gen(2),
        MutationStage::Published,
        RecoveryFsyncAttestation::new(true, false),
        gen(1),
    );
    assert!(result.is_rejected());
    assert!(!result.is_consistent());
    assert_eq!(
        result.rejection_code(),
        Some(DurableUpdateDiagnosticCode::InconsistentRecovery)
    );
}

#[test]
fn recovery_durable_but_not_published_yields_previous() {
    for stage in [
        MutationStage::Checkpointed,
        MutationStage::Compacted,
        MutationStage::Validated,
    ] {
        let result = CrashRecoveryResult::decide(
            gen(2),
            stage,
            RecoveryFsyncAttestation::new(true, true),
            gen(1),
        );
        assert!(
            matches!(
                result,
                CrashRecoveryResult::PreviousCommitted { generation } if generation == gen(1)
            ),
            "stage {stage:?} should yield previous committed"
        );
        assert!(result.is_consistent());
    }
}

#[test]
fn recovery_pre_checkpoint_yields_previous() {
    for stage in [
        MutationStage::OperationLogged,
        MutationStage::VectorUpserted,
        MutationStage::Tombstoned,
        MutationStage::GraphEdgeUpdated,
        MutationStage::FilterIndexUpdated,
    ] {
        let result = CrashRecoveryResult::decide(
            gen(2),
            stage,
            RecoveryFsyncAttestation::new(false, false),
            gen(1),
        );
        assert!(
            matches!(
                result,
                CrashRecoveryResult::PreviousCommitted { generation } if generation == gen(1)
            ),
            "stage {stage:?} should yield previous committed"
        );
    }
}

// ===========================================================================
// Source replacement atomicity
// ===========================================================================

#[test]
fn source_replace_rejects_dual_visibility() {
    assert_eq!(
        validate_source_replace_atomicity(gen(2), true, true)
            .expect_err("dual visibility fails closed")
            .diagnostic_code(),
        DurableUpdateDiagnosticCode::SourceReplaceVisibilityViolation
    );
    validate_source_replace_atomicity(gen(2), true, false).expect("old only is atomic");
    validate_source_replace_atomicity(gen(2), false, true).expect("new only is atomic");
    validate_source_replace_atomicity(gen(2), false, false).expect("neither is atomic");
}

// ===========================================================================
// Error redaction: Debug and Display contain only the closed code
// ===========================================================================

#[test]
fn error_debug_and_display_are_code_only() {
    let err = DurableUpdateError::contract(DurableUpdateDiagnosticCode::VersionOutOfOrder);
    let debug = format!("{err:?}");
    let display = format!("{err}");
    assert!(debug.contains("version_out_of_order"));
    assert!(display.contains("durable-updates.version_out_of_order"));
    // No payload, no caller-controlled data.
    assert!(!debug.contains("vector"));
    assert!(!display.contains("tenant"));
}
