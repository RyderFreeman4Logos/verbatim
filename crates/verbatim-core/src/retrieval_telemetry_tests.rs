use crate::retrieval_telemetry::{
    BackendAttribute, BackendAttributeKey, BackendAttributeValue, CandidateCounters,
    DiskannProviderLayout, LanceDbIndexType, MemoryEventCounters, MemorySnapshot,
    MemorySnapshotFields, PrivacyPolicy, QdrantQuantization, RedactedTelemetryId, ResourceCounters,
    SpanKind, StageSpan, StorageAccessMode, StorageCounters, TelemetryDataClass,
    TelemetryDestination, TelemetryDiagnosticCode, TelemetryError, MAX_STAGE_DURATION_MICROS,
};

#[test]
fn retrieval_telemetry_every_span_kind_constructs_and_validates() {
    for kind in SpanKind::ALL {
        let mut span = StageSpan::new(kind, 1_000, 1_025).expect("bounded stage span");
        assert_eq!(span.kind(), kind);
        assert_eq!(span.duration_micros(), 25);
        span.extend_by_micros(5).expect("bounded extension");
        assert_eq!(span.duration_micros(), 30);
        span.validate().expect("valid stage span");
    }

    assert_eq!(
        StageSpan::new(SpanKind::DenseRetrieval, 10, 9)
            .expect_err("end before start must fail closed")
            .diagnostic_code(),
        TelemetryDiagnosticCode::InvalidSpanTiming
    );
    assert_eq!(
        StageSpan::new(SpanKind::DenseRetrieval, 0, MAX_STAGE_DURATION_MICROS + 1,)
            .expect_err("unbounded span must fail closed")
            .diagnostic_code(),
        TelemetryDiagnosticCode::SpanDurationExceeded
    );
}

#[test]
fn retrieval_telemetry_counters_are_checked_and_overflow_safe() {
    let mut candidates = CandidateCounters::new();
    candidates
        .add_requested_k(SpanKind::DenseRetrieval, u64::MAX)
        .expect("first candidate count fits");
    assert_eq!(
        candidates
            .add_requested_k(SpanKind::DenseRetrieval, 1)
            .expect_err("candidate counter overflow must fail closed")
            .diagnostic_code(),
        TelemetryDiagnosticCode::CounterOverflow
    );
    assert_eq!(
        candidates.requested_k(SpanKind::DenseRetrieval),
        u64::MAX,
        "overflow must not mutate the counter"
    );

    let mut storage = StorageCounters::new();
    storage
        .add_sql_statements(u64::MAX)
        .expect("first storage count fits");
    assert_eq!(
        storage
            .add_sql_statements(1)
            .expect_err("storage counter overflow must fail closed")
            .diagnostic_code(),
        TelemetryDiagnosticCode::CounterOverflow
    );
    storage
        .record_access_mode(StorageAccessMode::Direct, 2)
        .expect("mode count is bounded");
    assert_eq!(storage.access_mode_count(StorageAccessMode::Direct), 2);

    let mut resources = ResourceCounters::new();
    resources
        .add_ssd_operations(u64::MAX)
        .expect("first resource count fits");
    assert_eq!(
        resources
            .add_ssd_operations(1)
            .expect_err("resource counter overflow must fail closed")
            .diagnostic_code(),
        TelemetryDiagnosticCode::CounterOverflow
    );
}

#[test]
fn retrieval_telemetry_memory_snapshot_exposes_cgroup_breakdown_without_paths() {
    let events = MemoryEventCounters {
        low: 1,
        high: 2,
        max: 3,
        oom: 4,
        oom_kill: 5,
    };
    let snapshot = MemorySnapshot::new(MemorySnapshotFields {
        cgroup_current_bytes: 1_024,
        cgroup_peak_bytes: 2_048,
        events,
        anonymous_bytes: 256,
        file_bytes: 512,
        kernel_bytes: 128,
    })
    .expect("bounded cgroup snapshot");

    assert_eq!(snapshot.cgroup_current_bytes(), 1_024);
    assert_eq!(snapshot.cgroup_peak_bytes(), 2_048);
    assert_eq!(snapshot.events(), events);
    assert_eq!(snapshot.anonymous_bytes(), 256);
    assert_eq!(snapshot.file_bytes(), 512);
    assert_eq!(snapshot.kernel_bytes(), 128);
}

#[test]
fn retrieval_telemetry_backend_attribute_keys_are_closed_and_value_validated() {
    let valid_attributes = [
        BackendAttribute::new(
            BackendAttributeKey::DiskannSearchEffort,
            BackendAttributeValue::Unsigned(64),
        ),
        BackendAttribute::new(
            BackendAttributeKey::DiskannProviderLayout,
            BackendAttributeValue::DiskannProviderLayout(DiskannProviderLayout::InMemory),
        ),
        BackendAttribute::new(
            BackendAttributeKey::LanceDbProbes,
            BackendAttributeValue::Unsigned(8),
        ),
        BackendAttribute::new(
            BackendAttributeKey::LanceDbRefinement,
            BackendAttributeValue::Unsigned(16),
        ),
        BackendAttribute::new(
            BackendAttributeKey::LanceDbIndexType,
            BackendAttributeValue::LanceDbIndexType(LanceDbIndexType::IvfPq),
        ),
        BackendAttribute::new(
            BackendAttributeKey::QdrantHnswEf,
            BackendAttributeValue::Unsigned(128),
        ),
        BackendAttribute::new(
            BackendAttributeKey::QdrantQuantization,
            BackendAttributeValue::QdrantQuantization(QdrantQuantization::Scalar),
        ),
        BackendAttribute::new(
            BackendAttributeKey::QdrantOversampling,
            BackendAttributeValue::Unsigned(2),
        ),
        BackendAttribute::new(
            BackendAttributeKey::QdrantRescore,
            BackendAttributeValue::Boolean(true),
        ),
        BackendAttribute::new(
            BackendAttributeKey::ExactScanCardinality,
            BackendAttributeValue::Unsigned(1_024),
        ),
    ];
    for attribute in valid_attributes {
        let attribute = attribute.expect("closed key accepts its compatible bounded value");
        attribute.validate().expect("attribute remains valid");
        assert!(!attribute.is_metric_label());
    }

    assert_eq!(
        BackendAttribute::new(
            BackendAttributeKey::DiskannSearchEffort,
            BackendAttributeValue::Boolean(true),
        )
        .expect_err("incompatible key/value pairing must fail closed")
        .diagnostic_code(),
        TelemetryDiagnosticCode::InvalidBackendAttribute
    );
}

#[test]
fn retrieval_telemetry_privacy_policy_blocks_raw_text_and_identifiers() {
    let policy = PrivacyPolicy::strict_default();
    for data_class in [
        TelemetryDataClass::RawQueryText,
        TelemetryDataClass::EvidenceText,
        TelemetryDataClass::FilesystemPath,
        TelemetryDataClass::Identifier,
        TelemetryDataClass::AclValue,
        TelemetryDataClass::Token,
        TelemetryDataClass::SourceLabel,
        TelemetryDataClass::TenantLabel,
    ] {
        assert_eq!(
            policy
                .validate_emission(TelemetryDestination::DefaultMetric, data_class)
                .expect_err("default metrics must reject private material")
                .diagnostic_code(),
            TelemetryDiagnosticCode::PrivacyPolicyViolation
        );
    }

    let raw_request_id = "private-request-id-should-never-render";
    let redacted = RedactedTelemetryId::new(raw_request_id).expect("opaque correlation ID");
    let rendered = format!("{redacted:?}|{redacted}|{}", redacted.as_opaque_token());
    assert!(!rendered.contains(raw_request_id));
    assert!(policy
        .validate_emission(
            TelemetryDestination::SpanAttribute,
            TelemetryDataClass::RedactedTelemetryId,
        )
        .is_ok());
    assert!(policy
        .validate_emission(
            TelemetryDestination::DefaultMetric,
            TelemetryDataClass::RedactedTelemetryId,
        )
        .is_err());
}

#[test]
fn retrieval_telemetry_diagnostics_render_only_stable_codes() {
    let secret = "query=very-private";
    for code in TelemetryDiagnosticCode::ALL {
        let error = TelemetryError::contract(code);
        assert_eq!(
            format!("{error:?}"),
            format!("TelemetryError({})", code.as_str())
        );
        assert_eq!(
            error.to_string(),
            format!("retrieval-telemetry.{}", code.as_str())
        );
        assert!(!format!("{error:?}").contains(secret));
        assert!(!error.to_string().contains(secret));
    }
}

#[test]
fn retrieval_telemetry_deserialization_revalidates_invariants() {
    let invalid_span = serde_json::json!({
        "kind": "dense_retrieval",
        "start_micros": 10,
        "end_micros": 9,
    });
    assert!(serde_json::from_value::<StageSpan>(invalid_span).is_err());

    let invalid_snapshot = serde_json::json!({
        "cgroup_current_bytes": 50,
        "cgroup_peak_bytes": 40,
        "events": {"low": 0, "high": 0, "max": 0, "oom": 0, "oom_kill": 0},
        "anonymous_bytes": 10,
        "file_bytes": 10,
        "kernel_bytes": 10,
    });
    assert!(serde_json::from_value::<MemorySnapshot>(invalid_snapshot).is_err());

    let valid_attribute = BackendAttribute::new(
        BackendAttributeKey::DiskannSearchEffort,
        BackendAttributeValue::Unsigned(32),
    )
    .expect("valid attribute");
    let mut invalid_attribute = serde_json::to_value(valid_attribute).expect("serialize attribute");
    invalid_attribute["value"] = serde_json::json!({"boolean": true});
    assert!(serde_json::from_value::<BackendAttribute>(invalid_attribute).is_err());

    assert!(serde_json::from_value::<RedactedTelemetryId>(serde_json::json!("raw-id"),).is_err());
}

#[test]
fn categorical_key_with_unsigned_value_is_invalid_attribute() {
    let error = BackendAttribute::new(
        BackendAttributeKey::DiskannProviderLayout,
        BackendAttributeValue::Unsigned(1),
    )
    .expect_err("unsigned value with categorical key must fail");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::InvalidBackendAttribute
    );
}

#[test]
fn numeric_key_zero_value_is_out_of_bounds() {
    let error = BackendAttribute::new(
        BackendAttributeKey::DiskannSearchEffort,
        BackendAttributeValue::Unsigned(0),
    )
    .expect_err("zero numeric value must fail");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::BackendAttributeValueOutOfBounds
    );
}

#[test]
fn numeric_key_over_limit_is_out_of_bounds() {
    let error = BackendAttribute::new(
        BackendAttributeKey::DiskannSearchEffort,
        BackendAttributeValue::Unsigned(u64::MAX),
    )
    .expect_err("over-limit numeric value must fail");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::BackendAttributeValueOutOfBounds
    );
}

#[test]
fn exact_scan_cardinality_over_limit_is_out_of_bounds() {
    let error = BackendAttribute::new(
        BackendAttributeKey::ExactScanCardinality,
        BackendAttributeValue::Unsigned(u64::MAX),
    )
    .expect_err("over-limit cardinality must fail with bounds error");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::BackendAttributeValueOutOfBounds
    );
}

#[test]
fn memory_snapshot_exceeds_bound_is_diagnostic() {
    use crate::retrieval_telemetry::MemorySnapshotFields;
    let error = MemorySnapshot::new(MemorySnapshotFields {
        cgroup_current_bytes: u64::MAX,
        cgroup_peak_bytes: 10,
        events: Default::default(),
        anonymous_bytes: 0,
        file_bytes: 0,
        kernel_bytes: 0,
    })
    .expect_err("exceeds-bound snapshot must fail");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::MemorySnapshotExceedsBound
    );
}

#[test]
fn memory_snapshot_current_exceeds_peak_is_invalid() {
    use crate::retrieval_telemetry::MemorySnapshotFields;
    let error = MemorySnapshot::new(MemorySnapshotFields {
        cgroup_current_bytes: 100,
        cgroup_peak_bytes: 50,
        events: Default::default(),
        anonymous_bytes: 0,
        file_bytes: 0,
        kernel_bytes: 0,
    })
    .expect_err("current > peak must fail");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::InvalidMemorySnapshot
    );
}

#[test]
fn redacted_telemetry_id_empty_token_is_diagnostic() {
    let error = RedactedTelemetryId::new(String::new()).expect_err("empty token must fail");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::InvalidRedactedTelemetryId
    );
}

#[test]
fn span_extend_overflow_is_diagnostic() {
    let mut span = StageSpan::new(SpanKind::DenseRetrieval, 0, 1).expect("valid span");
    let error = span
        .extend_by_micros(u64::MAX)
        .expect_err("overflow must fail");
    assert_eq!(
        error.diagnostic_code(),
        TelemetryDiagnosticCode::SpanDurationExceeded
    );
}
