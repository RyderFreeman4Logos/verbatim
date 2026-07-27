use super::*;

fn assert_code(error: DurabilityError, expected: DurabilityDiagnosticCode) {
    assert_eq!(error, DurabilityError::validation(expected));
    assert_eq!(
        error.to_string(),
        format!("durability.{}", expected.as_str())
    );
    assert_eq!(
        format!("{error:?}"),
        format!("DurabilityError({})", expected.as_str())
    );
}

#[test]
fn durability_profile_defaults_are_explicit_and_valid() {
    let cases = [
        (
            DurabilityProfile::Durable,
            JournalMode::Wal,
            SynchronousMode::Full,
            CheckpointMode::Full,
            RpoGuarantee::AcknowledgedCommits,
        ),
        (
            DurabilityProfile::Balanced,
            JournalMode::Wal,
            SynchronousMode::Normal,
            CheckpointMode::Passive,
            RpoGuarantee::UnboundedPowerLoss,
        ),
        (
            DurabilityProfile::Ephemeral,
            JournalMode::Delete,
            SynchronousMode::Off,
            CheckpointMode::Truncate,
            RpoGuarantee::NoGuarantee,
        ),
    ];

    for (profile, journal_mode, synchronous, checkpoint_mode, rpo) in cases {
        let config = profile.default_config();
        assert_eq!(config.profile, profile);
        assert_eq!(config.journal_mode, journal_mode);
        assert_eq!(config.synchronous, synchronous);
        assert_eq!(config.checkpoint_interval.mode, checkpoint_mode);
        assert!(config.checkpoint_interval.interval_seconds > 0);
        assert!(config.wal_autocheckpoint_pages > 0);
        assert!(config.busy_timeout_ms > 0);
        config.validate_for(profile).unwrap();

        let contract = profile.rpo_contract();
        assert_eq!(contract.profile, profile);
        assert_eq!(contract.rpo, rpo);
        contract.validate().unwrap();
    }
}

#[test]
fn every_profile_and_pragma_combination_is_validated_fail_closed() {
    for profile in DurabilityProfile::ALL {
        let expected = profile.default_config();
        for journal_mode in JournalMode::ALL {
            for synchronous in SynchronousMode::ALL {
                let candidate = DurabilityConfig {
                    journal_mode,
                    synchronous,
                    ..expected.clone()
                };
                let actual = candidate.validate_for(profile);
                let accepted =
                    journal_mode == expected.journal_mode && synchronous == expected.synchronous;
                assert_eq!(
                    actual.is_ok(),
                    accepted,
                    "{profile:?} {journal_mode:?} {synchronous:?}"
                );
            }
        }
    }
}

#[test]
fn durable_rejects_non_wal_or_non_full_even_when_other_settings_match() {
    let durable = DurabilityProfile::Durable.default_config();
    let non_wal = DurabilityConfig {
        journal_mode: JournalMode::Delete,
        ..durable.clone()
    };
    assert_code(
        non_wal
            .validate_for(DurabilityProfile::Durable)
            .unwrap_err(),
        DurabilityDiagnosticCode::DurableRequiresWal,
    );

    let non_full = DurabilityConfig {
        synchronous: SynchronousMode::Normal,
        ..durable
    };
    assert_code(
        non_full
            .validate_for(DurabilityProfile::Durable)
            .unwrap_err(),
        DurabilityDiagnosticCode::DurableRequiresFullSynchronous,
    );
}

#[test]
fn config_round_trip_revalidates_profile_binding_and_rejects_tampering() {
    for profile in DurabilityProfile::ALL {
        let config = profile.default_config();
        let json = encode_durability_config_json(&config).unwrap();
        assert_eq!(
            decode_durability_config_json(&json, profile).unwrap(),
            config
        );
    }

    let tampered = br#"{
        "profile":"durable",
        "journal_mode":"DELETE",
        "synchronous":"FULL",
        "wal_autocheckpoint_pages":100,
        "busy_timeout_ms":30000,
        "checkpoint_interval":{"mode":"FULL","interval_seconds":30}
    }"#;
    assert_code(
        decode_durability_config_json(tampered, DurabilityProfile::Durable).unwrap_err(),
        DurabilityDiagnosticCode::DurableRequiresWal,
    );
}

#[test]
fn disk_full_and_enospc_fail_closed_without_publishing_active_generation() {
    let policy = DiskSpacePolicy::for_profile(DurabilityProfile::Durable);
    policy.validate().unwrap();
    assert_eq!(
        policy.full_behavior,
        DiskFullBehavior::RejectWritePreserveActiveGeneration
    );
    assert_code(
        policy.preflight(policy.reserve_bytes - 1).unwrap_err(),
        DurabilityDiagnosticCode::DiskReserveNotMet,
    );
    assert_code(
        policy.fail_closed(DiskFullSignal::SqliteFull).unwrap_err(),
        DurabilityDiagnosticCode::SqliteFullFailClosed,
    );
    assert_code(
        policy.fail_closed(DiskFullSignal::Enospc).unwrap_err(),
        DurabilityDiagnosticCode::EnospcFailClosed,
    );
}

#[test]
fn publication_order_requires_source_index_task_cache() {
    let canonical = PublicationOrder::canonical();
    canonical.validate().unwrap();
    assert_eq!(
        canonical.steps(),
        &[
            PublicationStep::SourceReplacement,
            PublicationStep::IndexPublication,
            PublicationStep::TaskStatus,
            PublicationStep::CacheInvalidation,
        ]
    );

    let reordered = PublicationOrder::new([
        PublicationStep::IndexPublication,
        PublicationStep::SourceReplacement,
        PublicationStep::TaskStatus,
        PublicationStep::CacheInvalidation,
    ]);
    assert_code(
        reordered.validate().unwrap_err(),
        DurabilityDiagnosticCode::PublicationOrderInvalid,
    );
}

#[test]
fn durable_and_balanced_require_integrity_and_foreign_key_checks_after_abnormal_shutdown() {
    for profile in [DurabilityProfile::Durable, DurabilityProfile::Balanced] {
        let policy = RecoveryPolicy::for_profile(profile);
        assert!(policy.run_integrity_check_after_abnormal_shutdown);
        assert!(policy.run_foreign_key_check_after_abnormal_shutdown);
        policy.validate_for(profile).unwrap();

        let missing_integrity = RecoveryPolicy {
            run_integrity_check_after_abnormal_shutdown: false,
            ..policy
        };
        assert_code(
            missing_integrity.validate_for(profile).unwrap_err(),
            DurabilityDiagnosticCode::RecoveryIntegrityCheckRequired,
        );
    }

    RecoveryPolicy::for_profile(DurabilityProfile::Ephemeral)
        .validate_for(DurabilityProfile::Ephemeral)
        .unwrap();
}

#[test]
fn rpo_contracts_bind_profiles_to_dr_001_backups() {
    for profile in DurabilityProfile::ALL {
        let contract = profile.rpo_contract();
        contract.validate().unwrap();
        assert_eq!(
            contract.dr_001_backup,
            Dr001BackupRequirement::RequiredForHostOrMediaLoss
        );
        assert!(contract.rto_seconds > 0);
    }

    let invalid = RpoContract {
        dr_001_backup: Dr001BackupRequirement::NotRequired,
        ..DurabilityProfile::Durable.rpo_contract()
    };
    assert_code(
        invalid.validate().unwrap_err(),
        DurabilityDiagnosticCode::Dr001BackupRequired,
    );
}

#[test]
fn errors_render_only_closed_diagnostic_codes() {
    let error = DurabilityError::validation(DurabilityDiagnosticCode::PublicationOrderInvalid);
    assert_eq!(error.to_string(), "durability.publication_order_invalid");
    assert_eq!(
        format!("{error:?}"),
        "DurabilityError(publication_order_invalid)"
    );
}
