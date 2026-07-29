//! Focused unit tests for the generation publication and migration contract
//! (Refs #379).
//!
//! Covers: lifecycle stages and transitions, lease tracking and GC gating,
//! coordinator exclusivity, migration profiles and no-default fusion,
//! quarantine isolation, rollback durability, mixed-generation read rejection,
//! manifest validation, and JSON round-trip.

#![cfg(test)]

use super::error::{GenerationPublicationDiagnosticCode, GenerationPublicationError};
use super::identity::{ContentHash, CoordinatorEpoch, PublicationGenerationId, ShardOrdinal};
use super::lifecycle::{
    can_promote, validate_stage_transition, GenerationLease, LeaseRegistry, PublicationPointer,
};
use super::manifest::{
    decode_publication_manifest_json, encode_publication_manifest_json, BuildResourceReport,
    CandidateQuantizer, CompatibilityContract, FilterAclBinding, OriginalVectorEncoding,
    PublicationManifest, PublicationStage, SampledRecallReport, ShardDescriptor, UpdateCheckpoint,
    VectorBackendProvider, VectorMetric, VectorNormalization,
    GENERATION_PUBLICATION_SCHEMA_VERSION,
};
use super::migration::{
    reject_mixed_generation_read, CoordinatorLock, CoordinatorLockRegistry, FusionPolicy,
    MigrationCandidateMetrics, MigrationProfile, QuarantineRecord, QuarantineRegistry,
    RollbackFsyncAttestation, RollbackReceipt,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn valid_hash(n: u8) -> ContentHash {
    let hex = format!("sha256:{n:064x}");
    ContentHash::new(hex).expect("valid hash")
}

fn gen(n: u64) -> PublicationGenerationId {
    PublicationGenerationId::new(n).expect("valid generation")
}

fn epoch(n: u64) -> CoordinatorEpoch {
    CoordinatorEpoch::new(n).expect("valid epoch")
}

fn shard(ordinal: u32, count: u64) -> ShardDescriptor {
    ShardDescriptor {
        ordinal: ShardOrdinal::new(ordinal).expect("valid ordinal"),
        range_start: 1,
        range_end: count,
        vector_count: count,
        byte_size: count * 768 * 4, // dimension 768, float32
        graph_degree: 32,
        checksum: valid_hash(ordinal as u8),
    }
}

fn valid_manifest() -> PublicationManifest {
    PublicationManifest {
        schema_version: GENERATION_PUBLICATION_SCHEMA_VERSION,
        generation: gen(1),
        vector_space_id: "vs_main".to_string(),
        encoder_profile_id: "encoder_v1".to_string(),
        dimension: 768,
        metric: VectorMetric::Cosine,
        normalization: VectorNormalization::L2Unit,
        original_vector_encoding: OriginalVectorEncoding::Float32,
        provider: VectorBackendProvider::DiskAnn3Standard,
        shards: vec![shard(1, 100)],
        graph_max_degree: 32,
        build_search_list_size: 200,
        candidate_quantizer: CandidateQuantizer::ScalarQuantized,
        exact_vector_hash: valid_hash(0xa1),
        id_map_hash: valid_hash(0xa2),
        filter_acl: FilterAclBinding {
            filter_schema_version: 1,
            acl_policy_generation: 1,
        },
        update_checkpoint: UpdateCheckpoint {
            last_mutation_version: 1,
            tombstone_generation: 1,
        },
        sampled_recall: Some(SampledRecallReport {
            sample_size: 1000,
            recall_at_10: 0.95,
            min_recall_at_10: 0.88,
        }),
        build_resources: Some(BuildResourceReport {
            peak_memory_bytes: 1_073_741_824,
            build_duration_us: 3_600_000_000,
            ssd_bytes_written: 5_000_000_000,
            cpu_seconds: 1800.0,
        }),
        compatibility: CompatibilityContract {
            diskann3_version: "3.2.1".to_string(),
            source_revision: "abcdef0123456789abcdef0123456789abcdef01".to_string(),
            minimum_reader_version: 1,
        },
        stage: PublicationStage::Ready,
        sealed_at: "2026-07-28T00:00:00Z".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Identity tests
// ---------------------------------------------------------------------------

#[test]
fn generation_rejects_zero() {
    assert_eq!(
        PublicationGenerationId::new(0)
            .unwrap_err()
            .diagnostic_code(),
        GenerationPublicationDiagnosticCode::InvalidIdentity
    );
}

#[test]
fn epoch_advances_monotonically() {
    let e = epoch(5);
    assert_eq!(e.next().value(), 6);
}

#[test]
fn shard_ordinal_rejects_zero_and_overflow() {
    assert!(ShardOrdinal::new(0).is_err());
    assert!(ShardOrdinal::new(ShardOrdinal::MAX + 1).is_err());
    assert!(ShardOrdinal::new(1).is_ok());
}

#[test]
fn content_hash_redacts_debug() {
    let h = valid_hash(1);
    let dbg = format!("{h:?}");
    assert_eq!(dbg, "ContentHash(REDACTED)");
}

#[test]
fn content_hash_rejects_invalid() {
    assert!(ContentHash::new("not-a-hash").is_err());
    assert!(ContentHash::new("sha256:short").is_err());
    assert!(ContentHash::new("sha256:G".repeat(64)).is_err()); // non-hex
    assert!(valid_hash(1).validate().is_ok());
}

// ---------------------------------------------------------------------------
// Error redaction tests
// ---------------------------------------------------------------------------

#[test]
fn error_debug_emits_only_code() {
    let err =
        GenerationPublicationError::contract(GenerationPublicationDiagnosticCode::PointerConflict);
    let dbg = format!("{err:?}");
    assert_eq!(dbg, "GenerationPublicationError(pointer_conflict)");
    assert_eq!(err.to_string(), "generation-publication.pointer_conflict");
}

// ---------------------------------------------------------------------------
// Manifest validation tests
// ---------------------------------------------------------------------------

#[test]
fn valid_manifest_passes_validation() {
    assert!(valid_manifest().validate().is_ok());
}

#[test]
fn manifest_rejects_unknown_schema_version() {
    let mut m = valid_manifest();
    m.schema_version = 999;
    assert_eq!(
        m.validate().unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::InvalidContract
    );
}

#[test]
fn manifest_rejects_empty_vector_space() {
    let mut m = valid_manifest();
    m.vector_space_id = "  ".to_string();
    assert!(m.validate().is_err());
}

#[test]
fn manifest_rejects_zero_dimension() {
    let mut m = valid_manifest();
    m.dimension = 0;
    assert_eq!(
        m.validate().unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::InvalidBounds
    );
}

#[test]
fn manifest_rejects_empty_shards() {
    let mut m = valid_manifest();
    m.shards.clear();
    assert_eq!(
        m.validate().unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::MissingComponent
    );
}

#[test]
fn manifest_rejects_duplicate_shard_ordinals() {
    let mut m = valid_manifest();
    m.shards.push(shard(1, 50));
    assert_eq!(
        m.validate().unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::DuplicateShard
    );
}

#[test]
fn manifest_rejects_shard_byte_size_below_float32_minimum() {
    let mut m = valid_manifest();
    m.shards[0].byte_size = 1; // way too small for 100 * 768 * 4
    assert_eq!(
        m.validate().unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::InvalidBounds
    );
}

#[test]
fn manifest_total_vector_count_sums_shards() {
    let mut m = valid_manifest();
    m.shards.push(shard(2, 200));
    assert_eq!(m.total_vector_count(), 300);
}

#[test]
fn manifest_json_round_trip_preserves_fields() {
    let original = valid_manifest();
    let json = encode_publication_manifest_json(&original).expect("encode");
    let decoded = decode_publication_manifest_json(&json).expect("decode");
    assert_eq!(original, decoded);
}

#[test]
fn manifest_decode_rejects_malformed_json() {
    assert!(decode_publication_manifest_json("{ not json").is_err());
}

// ---------------------------------------------------------------------------
// Lifecycle / stage tests
// ---------------------------------------------------------------------------

#[test]
fn only_ready_can_be_promoted() {
    use PublicationStage::*;
    assert!(!can_promote(SnapshotFixed));
    assert!(!can_promote(Staging));
    assert!(!can_promote(Validating));
    assert!(can_promote(Ready));
    assert!(!can_promote(Active));
    assert!(!can_promote(Retained));
    assert!(!can_promote(GarbageCollected));
    assert!(!can_promote(Quarantined));
}

#[test]
fn stage_transitions_follow_lifecycle() {
    use PublicationStage::*;
    // Valid forward transitions.
    assert!(validate_stage_transition(SnapshotFixed, Staging).is_ok());
    assert!(validate_stage_transition(Staging, Validating).is_ok());
    assert!(validate_stage_transition(Validating, Ready).is_ok());
    assert!(validate_stage_transition(Validating, Quarantined).is_ok());
    assert!(validate_stage_transition(Ready, Active).is_ok());
    assert!(validate_stage_transition(Ready, Quarantined).is_ok());
    assert!(validate_stage_transition(Active, Retained).is_ok());
    assert!(validate_stage_transition(Retained, GarbageCollected).is_ok());
    assert!(validate_stage_transition(Retained, Quarantined).is_ok());
}

#[test]
fn stage_transitions_reject_invalid() {
    use PublicationStage::*;
    // Cannot skip stages or go backwards.
    assert!(validate_stage_transition(SnapshotFixed, Active).is_err());
    assert!(validate_stage_transition(Ready, Ready).is_err());
    assert!(validate_stage_transition(Active, Staging).is_err());
    assert!(validate_stage_transition(GarbageCollected, Active).is_err());
    assert!(validate_stage_transition(Quarantined, Active).is_err());
}

// ---------------------------------------------------------------------------
// Pointer tests
// ---------------------------------------------------------------------------

#[test]
fn pointer_rejects_empty_timestamp() {
    assert!(PublicationPointer::new(gen(1), epoch(1), "").is_err());
    assert!(PublicationPointer::new(gen(1), epoch(1), "  ").is_err());
}

#[test]
fn pointer_with_previous_round_trips() {
    let p = PublicationPointer::new(gen(2), epoch(5), "2026-07-28T00:00:00Z")
        .unwrap()
        .with_previous(gen(1));
    assert_eq!(p.previous_generation, Some(gen(1)));
    assert_eq!(p.active_generation, gen(2));
}

// ---------------------------------------------------------------------------
// Lease tests
// ---------------------------------------------------------------------------

#[test]
fn lease_rejects_zero_id_and_expiry() {
    assert!(GenerationLease::new(gen(1), 0, 100).is_err());
    assert!(GenerationLease::new(gen(1), 1, 0).is_err());
}

#[test]
fn lease_expiry_check() {
    let l = GenerationLease::new(gen(1), 1, 100).unwrap();
    assert!(!l.is_expired(50));
    assert!(l.is_expired(100));
    assert!(l.is_expired(200));
}

#[test]
fn lease_registry_acquire_and_release() {
    let mut reg = LeaseRegistry::new();
    let lease = GenerationLease::new(gen(1), 10, 1000).unwrap();
    reg.acquire(lease).unwrap();
    assert!(reg.has_active_leases(gen(1), 0).unwrap());
    reg.release(gen(1), 10).unwrap();
    assert!(!reg.has_active_leases(gen(1), 0).unwrap());
}

#[test]
fn lease_registry_rejects_duplicate_id() {
    let mut reg = LeaseRegistry::new();
    reg.acquire(GenerationLease::new(gen(1), 5, 1000).unwrap())
        .unwrap();
    assert!(reg
        .acquire(GenerationLease::new(gen(1), 5, 2000).unwrap())
        .is_err());
}

#[test]
fn lease_registry_release_unknown_fails() {
    let mut reg = LeaseRegistry::new();
    assert!(reg.release(gen(1), 99).is_err());
}

#[test]
fn lease_registry_prune_expired() {
    let mut reg = LeaseRegistry::new();
    reg.acquire(GenerationLease::new(gen(1), 1, 100).unwrap())
        .unwrap();
    reg.acquire(GenerationLease::new(gen(1), 2, 500).unwrap())
        .unwrap();
    let pruned = reg.prune_expired(200).unwrap();
    assert_eq!(pruned, 1);
    assert!(reg.has_active_leases(gen(1), 200).unwrap());
    let pruned2 = reg.prune_expired(600).unwrap();
    assert_eq!(pruned2, 1);
    assert!(!reg.has_active_leases(gen(1), 600).unwrap());
}

#[test]
fn gc_gated_by_active_lease() {
    let mut reg = LeaseRegistry::new();
    reg.acquire(GenerationLease::new(gen(1), 1, 10_000).unwrap())
        .unwrap();
    // Lease active → GC blocked.
    assert!(reg.has_active_leases(gen(1), 1).unwrap());
}

// ---------------------------------------------------------------------------
// Coordinator lock tests
// ---------------------------------------------------------------------------

#[test]
fn coordinator_lock_exclusivity() {
    let mut registry = CoordinatorLockRegistry::new();
    let lock_a = CoordinatorLock::new(1, epoch(1), gen(10)).unwrap();
    registry.acquire(lock_a).unwrap();

    // Different generation → rejected.
    let lock_b = CoordinatorLock::new(2, epoch(1), gen(20)).unwrap();
    assert_eq!(
        registry.acquire(lock_b).unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::CoordinatorLocked
    );

    // Different coordinator, same target → rejected.
    let lock_c = CoordinatorLock::new(3, epoch(1), gen(10)).unwrap();
    assert!(registry.acquire(lock_c).is_err());

    // Same coordinator, same target → ok (re-acquire).
    let lock_d = CoordinatorLock::new(1, epoch(1), gen(10)).unwrap();
    assert!(registry.acquire(lock_d).is_ok());
}

#[test]
fn coordinator_lock_release_by_epoch() {
    let mut registry = CoordinatorLockRegistry::new();
    registry
        .acquire(CoordinatorLock::new(1, epoch(5), gen(10)).unwrap())
        .unwrap();
    // Wrong epoch → cannot release.
    assert!(registry.release(1, epoch(99)).is_err());
    // Correct epoch → release.
    assert!(registry.release(1, epoch(5)).is_ok());
    assert!(registry.current().is_none());
}

#[test]
fn coordinator_lock_rejects_zero_id() {
    assert!(CoordinatorLock::new(0, epoch(1), gen(1)).is_err());
}

// ---------------------------------------------------------------------------
// Migration profile tests
// ---------------------------------------------------------------------------

fn candidate_metrics(recall: f64) -> MigrationCandidateMetrics {
    MigrationCandidateMetrics {
        recall_at_10: recall,
        p99_latency_us: 5000.0,
        peak_memory_bytes: 1_000_000_000,
        read_amplification: 2.0,
    }
}

#[test]
fn migration_profile_validates() {
    let profile = MigrationProfile {
        incumbent_generation: gen(1),
        candidate_generation: gen(2),
        incumbent_backend: VectorBackendProvider::DiskAnn3Standard,
        candidate_backend: VectorBackendProvider::DiskAnn3Aisaq,
        sample_size: 500,
        fusion_policy: FusionPolicy::None,
        incumbent_metrics: candidate_metrics(0.90),
        candidate_metrics: candidate_metrics(0.95),
    };
    assert!(profile.validate().is_ok());
    assert!(profile.candidate_recall_meets_or_exceeds_incumbent());
    assert!(!profile.fuses_by_default());
}

#[test]
fn migration_profile_rejects_same_generation() {
    let profile = MigrationProfile {
        incumbent_generation: gen(1),
        candidate_generation: gen(1),
        incumbent_backend: VectorBackendProvider::DiskAnn3Standard,
        candidate_backend: VectorBackendProvider::DiskAnn3Aisaq,
        sample_size: 500,
        fusion_policy: FusionPolicy::None,
        incumbent_metrics: candidate_metrics(0.90),
        candidate_metrics: candidate_metrics(0.95),
    };
    assert!(profile.validate().is_err());
}

#[test]
fn migration_profile_rejects_invalid_metrics() {
    let mut profile = MigrationProfile {
        incumbent_generation: gen(1),
        candidate_generation: gen(2),
        incumbent_backend: VectorBackendProvider::DiskAnn3Standard,
        candidate_backend: VectorBackendProvider::DiskAnn3Aisaq,
        sample_size: 500,
        fusion_policy: FusionPolicy::None,
        incumbent_metrics: candidate_metrics(0.90),
        candidate_metrics: candidate_metrics(0.95),
    };
    profile.candidate_metrics.recall_at_10 = 1.5; // out of range
    assert_eq!(
        profile.validate().unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::InvalidBounds
    );
}

#[test]
fn migration_profile_no_default_fusion() {
    let profile = MigrationProfile {
        incumbent_generation: gen(1),
        candidate_generation: gen(2),
        incumbent_backend: VectorBackendProvider::DiskAnn3Standard,
        candidate_backend: VectorBackendProvider::Qdrant,
        sample_size: 100,
        fusion_policy: FusionPolicy::None,
        incumbent_metrics: candidate_metrics(0.90),
        candidate_metrics: candidate_metrics(0.80),
    };
    assert!(!profile.fuses_by_default());
}

#[test]
fn migration_profile_experiment_fusion_opt_in() {
    let profile = MigrationProfile {
        incumbent_generation: gen(1),
        candidate_generation: gen(2),
        incumbent_backend: VectorBackendProvider::DiskAnn3Standard,
        candidate_backend: VectorBackendProvider::LanceDb,
        sample_size: 100,
        fusion_policy: FusionPolicy::Experiment,
        incumbent_metrics: candidate_metrics(0.90),
        candidate_metrics: candidate_metrics(0.92),
    };
    assert!(profile.fuses_by_default());
}

// ---------------------------------------------------------------------------
// Quarantine tests
// ---------------------------------------------------------------------------

#[test]
fn quarantine_record_validates() {
    let record = QuarantineRecord::new(
        gen(3),
        "2026-07-28T00:00:00Z",
        GenerationPublicationDiagnosticCode::StagingNotDurable,
    )
    .unwrap();
    assert_eq!(
        record.reason,
        GenerationPublicationDiagnosticCode::StagingNotDurable
    );
}

#[test]
fn quarantine_record_rejects_empty_timestamp() {
    assert!(QuarantineRecord::new(
        gen(3),
        "",
        GenerationPublicationDiagnosticCode::StagingNotDurable
    )
    .is_err());
}

#[test]
fn quarantine_conflicts_with_newer_generation() {
    let record = QuarantineRecord::new(
        gen(1),
        "t",
        GenerationPublicationDiagnosticCode::IncompatibleBackend,
    )
    .unwrap();
    // Older generation quarantined; promoting under newer gen is blocked.
    assert!(record.conflicts_with_newer_generation(gen(5)));
    assert!(!record.conflicts_with_newer_generation(gen(1)));
}

#[test]
fn quarantine_registry_isolation() {
    let mut reg = QuarantineRegistry::new();
    let record = QuarantineRecord::new(
        gen(3),
        "t",
        GenerationPublicationDiagnosticCode::StagingNotDurable,
    )
    .unwrap();
    reg.quarantine(record).unwrap();
    assert!(reg.is_quarantined(gen(3)).unwrap());
    assert!(!reg.is_quarantined(gen(4)).unwrap());
    // Double-quarantine fails.
    let dup = QuarantineRecord::new(
        gen(3),
        "t2",
        GenerationPublicationDiagnosticCode::IncompatibleBackend,
    )
    .unwrap();
    assert_eq!(
        reg.quarantine(dup).unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::QuarantineConflict
    );
}

// ---------------------------------------------------------------------------
// Rollback durability tests
// ---------------------------------------------------------------------------

#[test]
fn rollback_receipt_requires_full_fsync() {
    // Fully durable → ok.
    let receipt = RollbackReceipt::new(
        gen(2),
        gen(1),
        epoch(3),
        RollbackFsyncAttestation::new(true, true),
        "2026-07-28T00:00:00Z",
    )
    .unwrap();
    assert!(receipt.is_durable_across_restart());

    // Missing dir fsync → rejected.
    let err = RollbackReceipt::new(
        gen(2),
        gen(1),
        epoch(3),
        RollbackFsyncAttestation::new(true, false),
        "t",
    )
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        GenerationPublicationDiagnosticCode::RollbackNotDurable
    );
}

#[test]
fn rollback_receipt_rejects_same_generation() {
    let err = RollbackReceipt::new(
        gen(1),
        gen(1),
        epoch(3),
        RollbackFsyncAttestation::new(true, true),
        "t",
    )
    .unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        GenerationPublicationDiagnosticCode::InvalidContract
    );
}

#[test]
fn rollback_receipt_rejects_empty_timestamp() {
    assert!(RollbackReceipt::new(
        gen(2),
        gen(1),
        epoch(3),
        RollbackFsyncAttestation::new(true, true),
        "",
    )
    .is_err());
}

#[test]
fn fsync_attestation_durability() {
    assert!(RollbackFsyncAttestation::new(true, true).is_fully_durable());
    assert!(!RollbackFsyncAttestation::new(true, false).is_fully_durable());
    assert!(!RollbackFsyncAttestation::new(false, true).is_fully_durable());
    assert!(!RollbackFsyncAttestation::new(false, false).is_fully_durable());
}

// ---------------------------------------------------------------------------
// Mixed-generation read rejection
// ---------------------------------------------------------------------------

#[test]
fn mixed_generation_read_rejected() {
    assert!(reject_mixed_generation_read(gen(1), gen(1)).is_ok());
    let err = reject_mixed_generation_read(gen(1), gen(2)).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        GenerationPublicationDiagnosticCode::MixedGenerationRead
    );
}

// ---------------------------------------------------------------------------
// Sampled recall gate
// ---------------------------------------------------------------------------

#[test]
fn recall_gate_threshold() {
    let passing = SampledRecallReport {
        sample_size: 1000,
        recall_at_10: 0.95,
        min_recall_at_10: 0.85,
    };
    assert!(passing.validate().is_ok());
    assert!(passing.meets_threshold());

    let failing = SampledRecallReport {
        sample_size: 1000,
        recall_at_10: 0.80,
        min_recall_at_10: 0.70,
    };
    assert!(failing.validate().is_ok());
    assert!(!failing.meets_threshold());
}

#[test]
fn recall_rejects_min_above_mean() {
    let bad = SampledRecallReport {
        sample_size: 1000,
        recall_at_10: 0.80,
        min_recall_at_10: 0.90, // min > mean
    };
    assert_eq!(
        bad.validate().unwrap_err().diagnostic_code(),
        GenerationPublicationDiagnosticCode::InvalidBounds
    );
}

#[test]
fn recall_rejects_zero_sample() {
    let bad = SampledRecallReport {
        sample_size: 0,
        recall_at_10: 0.95,
        min_recall_at_10: 0.85,
    };
    assert!(bad.validate().is_err());
}
