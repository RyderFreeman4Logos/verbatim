//! Contract tests for the versioned migration framework (MIGRATE-001 / issue #335).

use super::*;

const APP_ID: u32 = 0x5645_5242; // 'VERB'

fn v(major: u32, minor: u32, patch: u32) -> SchemaVersion {
    SchemaVersion::new(major, minor, patch)
}

fn window(min: SchemaVersion, max: SchemaVersion) -> CompatibilityWindow {
    CompatibilityWindow::new(min, max).unwrap()
}

fn step(
    id: &str,
    sequence: u32,
    from: SchemaVersion,
    to: SchemaVersion,
    body: &str,
) -> MigrationStep {
    MigrationStep::new(MigrationStepParams {
        id: id.into(),
        sequence,
        from_version: from,
        to_version: to,
        checksum: MigrationStep::checksum_of(body.as_bytes()),
        description: format!("step {id}"),
        idempotent: true,
        transactional: true,
    })
    .unwrap()
}

fn framework_with_chain() -> MigrationFramework {
    // Supported window: 1.0.0 ..= 1.2.0 with steps 1.0.0->1.1.0->1.2.0
    let mut fw = MigrationFramework::new(APP_ID, window(v(1, 0, 0), v(1, 2, 0)));
    fw.register(step("m_1_0_to_1_1", 10, v(1, 0, 0), v(1, 1, 0), "body-a"))
        .unwrap();
    fw.register(step("m_1_1_to_1_2", 20, v(1, 1, 0), v(1, 2, 0), "body-b"))
        .unwrap();
    fw
}

#[test]
fn schema_version_parse_and_order() {
    assert_eq!(SchemaVersion::parse("1.2.3").unwrap(), v(1, 2, 3));
    assert!(SchemaVersion::parse("1.2").is_err());
    assert!(SchemaVersion::parse("1.2.3.4").is_err());
    assert!(SchemaVersion::parse("a.b.c").is_err());
    assert!(v(1, 0, 0) < v(1, 1, 0));
    assert!(v(1, 1, 0) < v(2, 0, 0));
    assert_eq!(v(1, 2, 3).as_dotted(), "1.2.3");
}

#[test]
fn compatibility_window_rejects_inverted_bounds() {
    assert!(CompatibilityWindow::new(v(2, 0, 0), v(1, 0, 0)).is_err());
}

#[test]
fn forward_version_fail_closed_without_read_only_adapter() {
    let fw = framework_with_chain();
    let err = fw
        .detect_version(v(1, 3, 0), false)
        .expect_err("newer store must fail closed");
    let msg = err.to_string();
    assert!(msg.contains("newer than supported"), "{msg}");
    assert!(msg.contains("read-only adapter"), "{msg}");
}

#[test]
fn read_only_escape_hatch_for_newer_database() {
    let fw = framework_with_chain();
    match fw.detect_version(v(1, 9, 0), true).unwrap() {
        VersionDecision::NewerThanSupported {
            store_version,
            max_supported,
            access,
        } => {
            assert_eq!(store_version, v(1, 9, 0));
            assert_eq!(max_supported, v(1, 2, 0));
            assert_eq!(access, AccessMode::ReadOnlyAdapter);
        }
        other => panic!("expected NewerThanSupported, got {other:?}"),
    }
}

#[test]
fn upgrade_path_detection_lists_ordered_steps() {
    let fw = framework_with_chain();
    match fw.detect_version(v(1, 0, 0), false).unwrap() {
        VersionDecision::NeedsUpgrade {
            from,
            to,
            steps,
            access,
        } => {
            assert_eq!(from, v(1, 0, 0));
            assert_eq!(to, v(1, 2, 0));
            assert_eq!(
                steps,
                vec!["m_1_0_to_1_1".to_string(), "m_1_1_to_1_2".to_string()]
            );
            assert_eq!(access, AccessMode::ReadWrite);
        }
        other => panic!("expected NeedsUpgrade, got {other:?}"),
    }

    match fw.detect_version(v(1, 2, 0), false).unwrap() {
        VersionDecision::UpToDate { version, access } => {
            assert_eq!(version, v(1, 2, 0));
            assert_eq!(access, AccessMode::ReadWrite);
        }
        other => panic!("expected UpToDate, got {other:?}"),
    }
}

#[test]
fn older_than_window_is_not_silently_served() {
    let fw = framework_with_chain();
    match fw.detect_version(v(0, 9, 0), false).unwrap() {
        VersionDecision::OlderThanSupported {
            store_version,
            min_supported,
        } => {
            assert_eq!(store_version, v(0, 9, 0));
            assert_eq!(min_supported, v(1, 0, 0));
        }
        other => panic!("expected OlderThanSupported, got {other:?}"),
    }
}

#[test]
fn idempotent_migration_can_be_reapplied() {
    let fw = framework_with_chain();
    let mut history = MigrationHistory::new();
    let mut counter = 0u64;

    let v1 = fw
        .apply_pending(v(1, 0, 0), v(1, 2, 0), &mut history, &mut counter, None)
        .unwrap();
    assert_eq!(v1, v(1, 2, 0));
    let applied_once = history
        .entries
        .iter()
        .filter(|e| e.status == MigrationApplyStatus::Applied)
        .count();
    assert_eq!(applied_once, 2);

    // Re-run full plan: steps become SkippedIdempotent.
    let v2 = fw
        .apply_pending(v(1, 0, 0), v(1, 2, 0), &mut history, &mut counter, None)
        .unwrap();
    assert_eq!(v2, v(1, 2, 0));
    let skipped = history
        .entries
        .iter()
        .filter(|e| e.status == MigrationApplyStatus::SkippedIdempotent)
        .count();
    assert_eq!(skipped, 2);
    assert!(history.applied_ids().len() >= 4);
}

#[test]
fn checksum_mismatch_fails_closed() {
    let fw = framework_with_chain();
    let mut history = MigrationHistory::new();
    // Poison history with wrong checksum for a known id.
    history.entries.push(MigrationHistoryEntry {
        migration_id: "m_1_0_to_1_1".into(),
        sequence: 10,
        from_version: v(1, 0, 0),
        to_version: v(1, 1, 0),
        checksum: "deadbeef".into(),
        status: MigrationApplyStatus::Applied,
        applied_at_counter: 1,
    });

    let err = fw
        .verify_history_checksums(&history)
        .expect_err("checksum mismatch must fail");
    assert!(err.to_string().contains("checksum mismatch"), "{}", err);

    let mut counter = 1u64;
    let apply_err = fw
        .apply_pending(v(1, 1, 0), v(1, 2, 0), &mut history, &mut counter, None)
        .expect_err("apply must refuse corrupt history");
    assert!(
        apply_err.to_string().contains("checksum mismatch"),
        "{apply_err}"
    );
}

#[test]
fn disk_space_check_fails_when_insufficient() {
    let fw = framework_with_chain();
    let history = MigrationHistory::new();
    let disk = DiskSpaceRequirement {
        available_bytes: 100,
        required_bytes: 1_000,
    };
    let err = fw
        .preflight(v(1, 0, 0), v(1, 2, 0), disk, &history, None, "pre-migrate")
        .expect_err("disk preflight must fail");
    assert!(err.to_string().contains("insufficient disk space"), "{err}");
}

#[test]
fn preflight_runs_backup_hook_and_window_checks() {
    let fw = framework_with_chain();
    let history = MigrationHistory::new();
    let disk = DiskSpaceRequirement {
        available_bytes: 10_000,
        required_bytes: 1_000,
    };
    let mut backup = RecordingBackupHook::default();
    let report = fw
        .preflight(
            v(1, 0, 0),
            v(1, 2, 0),
            disk,
            &history,
            Some(&mut backup),
            "pre-migrate-label",
        )
        .unwrap();
    assert!(report.backup_created);
    assert_eq!(
        report.backup_id.as_deref(),
        Some("backup:pre-migrate-label")
    );
    assert_eq!(backup.calls, vec!["pre-migrate-label".to_string()]);
    assert_eq!(report.current_version, v(1, 0, 0));
    assert_eq!(report.target_version, v(1, 2, 0));

    // Outside window target rejected.
    let err = fw
        .preflight(v(1, 0, 0), v(9, 0, 0), disk, &history, None, "x")
        .expect_err("target outside window");
    assert!(
        err.to_string().contains("outside compatibility window"),
        "{err}"
    );
}

#[test]
fn preflight_fails_when_backup_hook_fails() {
    let fw = framework_with_chain();
    let history = MigrationHistory::new();
    let disk = DiskSpaceRequirement {
        available_bytes: 10_000,
        required_bytes: 1,
    };
    let mut backup = RecordingBackupHook {
        fail: true,
        ..Default::default()
    };
    let err = fw
        .preflight(
            v(1, 0, 0),
            v(1, 1, 0),
            disk,
            &history,
            Some(&mut backup),
            "must-fail",
        )
        .expect_err("backup failure must abort preflight");
    assert!(err.to_string().contains("backup hook failed"), "{err}");
}

#[test]
fn interrupted_migration_leaves_recoverable_prior_state() {
    let fw = framework_with_chain();
    let mut history = MigrationHistory::new();
    let mut counter = 0u64;

    let err = fw
        .apply_pending(
            v(1, 0, 0),
            v(1, 2, 0),
            &mut history,
            &mut counter,
            Some(0), // interrupt first runnable step
        )
        .expect_err("forced interruption");
    assert!(err.to_string().contains("interrupted"), "{err}");
    assert!(history.has_unrecovered_interruption());
    // No successful completion yet.
    assert!(history.last_completed_version().is_none());

    // Cannot apply again until recovery.
    let blocked = fw
        .apply_pending(v(1, 0, 0), v(1, 2, 0), &mut history, &mut counter, None)
        .expect_err("blocked while interrupted");
    assert!(
        blocked.to_string().contains("unrecovered interruption"),
        "{blocked}"
    );

    MigrationFramework::recover_interrupted(&mut history).unwrap();
    assert!(!history.has_unrecovered_interruption());

    let final_version = fw
        .apply_pending(v(1, 0, 0), v(1, 2, 0), &mut history, &mut counter, None)
        .unwrap();
    assert_eq!(final_version, v(1, 2, 0));
    assert_eq!(history.last_completed_version(), Some(v(1, 2, 0)));
}

#[test]
fn interrupt_after_first_step_preserves_completed_version() {
    let fw = framework_with_chain();
    let mut history = MigrationHistory::new();
    let mut counter = 0u64;

    // Apply first step fully, interrupt second.
    let mid = fw
        .apply_pending(v(1, 0, 0), v(1, 1, 0), &mut history, &mut counter, None)
        .unwrap();
    assert_eq!(mid, v(1, 1, 0));
    assert_eq!(history.last_completed_version(), Some(v(1, 1, 0)));

    let err = fw
        .apply_pending(v(1, 1, 0), v(1, 2, 0), &mut history, &mut counter, Some(0))
        .expect_err("interrupt second step");
    assert!(err.to_string().contains("interrupted"), "{err}");
    // Prior completed version remains discoverable after recovery of tail.
    MigrationFramework::recover_interrupted(&mut history).unwrap();
    assert_eq!(history.last_completed_version(), Some(v(1, 1, 0)));

    let done = fw
        .apply_pending(v(1, 1, 0), v(1, 2, 0), &mut history, &mut counter, None)
        .unwrap();
    assert_eq!(done, v(1, 2, 0));
}

#[test]
fn artifact_version_read_only_adapter_for_newer_immutable() {
    let fw = framework_with_chain();
    let artifact = ArtifactVersion::new(ArtifactKind::ContextPack, v(2, 0, 0), "abc123", true);
    let err = fw
        .classify_artifact(&artifact, false)
        .expect_err("newer artifact fails closed");
    assert!(
        err.to_string().contains("newer than max_supported"),
        "{err}"
    );

    let mode = fw.classify_artifact(&artifact, true).unwrap();
    assert_eq!(mode, AccessMode::ReadOnlyAdapter);

    let current = ArtifactVersion::new(ArtifactKind::QueryPlan, v(1, 1, 0), "q", false);
    assert_eq!(
        fw.classify_artifact(&current, false).unwrap(),
        AccessMode::ReadWrite
    );
}

#[test]
fn empty_artifact_checksum_rejected() {
    let artifact = ArtifactVersion::new(ArtifactKind::EvidencePack, v(1, 0, 0), "  ", true);
    assert!(artifact.validate().is_err());
}

#[test]
fn migration_step_must_advance_version_and_carry_checksum() {
    assert!(MigrationStep::new(MigrationStepParams {
        id: "noop".into(),
        sequence: 1,
        from_version: v(1, 0, 0),
        to_version: v(1, 0, 0),
        checksum: "x".into(),
        description: "bad".into(),
        idempotent: true,
        transactional: true,
    })
    .is_err());

    assert!(MigrationStep::new(MigrationStepParams {
        id: "empty-ck".into(),
        sequence: 1,
        from_version: v(1, 0, 0),
        to_version: v(1, 0, 1),
        checksum: "".into(),
        description: "bad".into(),
        idempotent: true,
        transactional: true,
    })
    .is_err());
}

#[test]
fn history_and_preflight_unknown_schema_fail_closed() {
    let mut history = MigrationHistory::new();
    history.schema_version = 99;
    assert!(history.validate_schema().is_err());
    assert!(decode_migration_history_json(r#"{"schema_version":99,"entries":[]}"#).is_err());

    let raw = serde_json::to_string(&MigrationHistory::new()).unwrap();
    let decoded = decode_migration_history_json(&raw).unwrap();
    assert_eq!(decoded.schema_version, MIGRATION_FRAMEWORK_SCHEMA_VERSION);

    let report = PreflightReport {
        schema_version: 99,
        current_version: v(1, 0, 0),
        target_version: v(1, 0, 0),
        window: window(v(1, 0, 0), v(1, 0, 0)),
        disk: DiskSpaceRequirement {
            available_bytes: 1,
            required_bytes: 1,
        },
        backup_created: false,
        backup_id: None,
    };
    assert!(report.validate_schema().is_err());
}

#[test]
fn downgrade_is_rejected_in_preflight() {
    let fw = framework_with_chain();
    let history = MigrationHistory::new();
    let disk = DiskSpaceRequirement {
        available_bytes: 1000,
        required_bytes: 1,
    };
    let err = fw
        .preflight(v(1, 2, 0), v(1, 0, 0), disk, &history, None, "no-downgrade")
        .expect_err("downgrade must fail");
    assert!(err.to_string().contains("downgrade"), "{err}");
}

#[test]
fn incomplete_migration_path_fails_planning() {
    let mut fw = MigrationFramework::new(APP_ID, window(v(1, 0, 0), v(1, 2, 0)));
    // Only first hop registered — path incomplete.
    fw.register(step("only_first", 10, v(1, 0, 0), v(1, 1, 0), "body"))
        .unwrap();
    let err = fw
        .plan_upgrade(v(1, 0, 0), v(1, 2, 0))
        .expect_err("incomplete path");
    assert!(
        err.to_string().contains("incomplete migration path"),
        "{err}"
    );
}

#[test]
fn serde_roundtrip_history_and_step() {
    let s = step("m1", 1, v(1, 0, 0), v(1, 0, 1), "canonical-body");
    let json = serde_json::to_string(&s).unwrap();
    let back: MigrationStep = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);

    let mut history = MigrationHistory::new();
    history.entries.push(MigrationHistoryEntry {
        migration_id: s.id.clone(),
        sequence: s.sequence,
        from_version: s.from_version,
        to_version: s.to_version,
        checksum: s.checksum.clone(),
        status: MigrationApplyStatus::Applied,
        applied_at_counter: 7,
    });
    let hjson = serde_json::to_string(&history).unwrap();
    let hback = decode_migration_history_json(&hjson).unwrap();
    assert_eq!(hback, history);
}
