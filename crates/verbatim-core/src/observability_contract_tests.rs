//! Contract tests for cross-service observability (OBS-001 / issue #338).

use super::*;
use anyhow::Result;
use std::collections::BTreeMap;
use std::time::Duration;

fn req_ctx() -> TraceContext {
    TraceContext::from_request_id("req-001").unwrap()
}

fn full_ctx() -> TraceContext {
    TraceContext::new(TraceContextFields {
        request_id: "req-001".into(),
        retrieval_run_id: Some("ret-9".into()),
        context_pack_id: Some("ctx-pack-3".into()),
        workflow_run_id: Some("wf-2".into()),
        task_id: Some("task-7".into()),
        publication_generation: Some("pub-gen-4".into()),
        trace_id: Some("trace-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
        span_id: Some("span-root-0001".into()),
        parent_span_id: None,
    })
    .unwrap()
}

fn sample_slo(
    success_ratio_target: f64,
    sampling_ratio: f64,
    domains: Vec<SloFailureDomain>,
) -> Result<SloDefinition> {
    SloDefinition::new(SloDefinitionParams {
        name: "ask_success".into(),
        description: "Successful ask path excluding client cancels".into(),
        success_ratio_target,
        latency: LatencyTarget::new(99, 2_000)?,
        window_secs: 86_400,
        sampling_ratio,
        retention_secs: 604_800,
        failure_domains: domains,
    })
}

#[test]
fn trace_context_requires_non_empty_request_id() {
    assert!(TraceContext::from_request_id("").is_err());
    assert!(TraceContext::from_request_id("   ").is_err());
    let ok = TraceContext::from_request_id("req-x").unwrap();
    assert_eq!(ok.request_id, "req-x");
    assert_eq!(ok.schema_version, OBSERVABILITY_CONTRACT_SCHEMA_VERSION);
}

#[test]
fn child_span_propagates_correlation_ids() {
    let parent = full_ctx();
    let child = parent.child_span("span-child-0002").unwrap();
    assert_eq!(child.request_id, parent.request_id);
    assert_eq!(child.retrieval_run_id, parent.retrieval_run_id);
    assert_eq!(child.context_pack_id, parent.context_pack_id);
    assert_eq!(child.workflow_run_id, parent.workflow_run_id);
    assert_eq!(child.task_id, parent.task_id);
    assert_eq!(child.publication_generation, parent.publication_generation);
    assert_eq!(child.trace_id, parent.trace_id);
    assert_eq!(child.span_id.as_deref(), Some("span-child-0002"));
    assert_eq!(child.parent_span_id, parent.span_id);
    assert!(child.same_request_as(&parent));
}

#[test]
fn async_link_drops_span_tree_keeps_request_ids() {
    let parent = full_ctx();
    let linked = parent.for_async_link().unwrap();
    assert_eq!(linked.request_id, parent.request_id);
    assert_eq!(linked.retrieval_run_id, parent.retrieval_run_id);
    assert_eq!(linked.workflow_run_id, parent.workflow_run_id);
    assert_eq!(linked.trace_id, parent.trace_id);
    assert!(linked.span_id.is_none());
    assert!(linked.parent_span_id.is_none());
}

#[test]
fn span_duration_and_status_lifecycle() {
    let mut span = SpanSpec::open(SpanOpenParams {
        name: "retrieve.rrf".into(),
        context: full_ctx(),
        start_unix_ms: 1_000,
    })
    .unwrap();
    assert_eq!(span.status, SpanStatus::Unset);
    assert!(span.duration().is_none());
    span.end_ok(1_250).unwrap();
    assert_eq!(span.status, SpanStatus::Ok);
    assert_eq!(span.duration(), Some(Duration::from_millis(250)));
}

#[test]
fn span_error_requires_error_class() {
    let mut span = SpanSpec::open(SpanOpenParams {
        name: "model.generate".into(),
        context: req_ctx(),
        start_unix_ms: 10,
    })
    .unwrap();
    assert!(span.end_error(20, "").is_err());
    span.end_error(20, "provider_timeout").unwrap();
    assert_eq!(span.status, SpanStatus::Error);
    assert_eq!(span.error_class.as_deref(), Some("provider_timeout"));
}

#[test]
fn span_link_fanout_bounded() {
    let mut span = SpanSpec::open(SpanOpenParams {
        name: "queue.consume".into(),
        context: full_ctx(),
        start_unix_ms: 50,
    })
    .unwrap();
    let link = SpanLink::new("trace-a", "span-peer")
        .unwrap()
        .with_relationship("follows_from");
    span.add_link(link).unwrap();
    assert_eq!(span.links.len(), 1);
    assert_eq!(span.links[0].relationship.as_deref(), Some("follows_from"));

    for i in 0..MAX_SPAN_LINKS {
        if span.links.len() >= MAX_SPAN_LINKS {
            break;
        }
        span.add_link(SpanLink::new(format!("t-{i}"), format!("s-{i}")).unwrap())
            .unwrap();
    }
    assert_eq!(span.links.len(), MAX_SPAN_LINKS);
    let err = span
        .add_link(SpanLink::new("overflow-t", "overflow-s").unwrap())
        .unwrap_err();
    assert!(err.to_string().contains("hard bound"), "{err}");
}

#[test]
fn span_attributes_hard_bound() {
    let mut span = SpanSpec::open(SpanOpenParams {
        name: "hydrate".into(),
        context: req_ctx(),
        start_unix_ms: 1,
    })
    .unwrap();
    for i in 0..MAX_SPAN_ATTRIBUTES {
        span.set_attribute(format!("k{i}"), format!("v{i}"))
            .unwrap();
    }
    let err = span
        .set_attribute("overflow", "x")
        .expect_err("must reject over-bound attributes");
    assert!(err.to_string().contains("hard bound"), "{err}");
    // Overwrite existing key still allowed.
    span.set_attribute("k0", "replaced").unwrap();
}

#[test]
fn metric_rejects_prohibited_and_undeclared_labels() {
    let metric = MetricSpec::new(
        "verbatim_request_latency_ms",
        MetricKind::Histogram,
        "End-to-end request latency",
        "ms",
        vec![
            MetricLabelSpec::approved("stage", 16).unwrap(),
            MetricLabelSpec::approved("status_class", 8).unwrap(),
            MetricLabelSpec::prohibited("query").unwrap(),
        ],
    )
    .unwrap();

    let mut ok = BTreeMap::new();
    ok.insert("stage".into(), "retrieve".into());
    ok.insert("status_class".into(), "ok".into());
    metric.validate_sample_labels(&ok).unwrap();

    let mut prohibited = ok.clone();
    prohibited.insert("query".into(), "how to build a bomb".into());
    let err = metric.validate_sample_labels(&prohibited).unwrap_err();
    assert!(err.to_string().contains("prohibited"), "{err}");

    let mut undeclared = ok;
    undeclared.insert("user_id".into(), "u-1".into());
    let err = metric.validate_sample_labels(&undeclared).unwrap_err();
    assert!(err.to_string().contains("undeclared"), "{err}");
}

#[test]
fn redaction_policy_strips_query_path_and_tokens() {
    let policy = RedactionPolicy::strict_default();
    assert_eq!(
        policy.redact_field("query", "secret user question"),
        "[REDACTED]"
    );
    assert_eq!(
        policy.redact_field("file_path", "/home/obj/private/doc.pdf"),
        "[REDACTED]"
    );
    assert_eq!(
        policy.redact_field("authorization", "Bearer abc"),
        "[REDACTED]"
    );
    assert_eq!(
        policy.redact_field("stage", "retrieve"),
        "retrieve".to_string()
    );
    assert_eq!(
        policy.redact_message("failed reading /var/lib/verbatim/store.db"),
        "[REDACTED]"
    );
    let token = "a".repeat(40);
    assert_eq!(policy.redact_message(&token), "[REDACTED]");
}

#[test]
fn log_entry_build_applies_redaction_automatically() {
    let mut fields = BTreeMap::new();
    fields.insert("query".into(), "private question".into());
    fields.insert("stage".into(), "context".into());
    fields.insert(
        "note".into(),
        "Bearer supersecrettokenvalue0123456789abcd".into(),
    );
    let entry = LogEntry::build(LogEntryParams {
        level: LogLevel::Info,
        message: "assembled context pack".into(),
        context: full_ctx(),
        fields,
        timestamp_unix_ms: 99,
        policy: RedactionPolicy::strict_default(),
    })
    .unwrap();
    assert_eq!(
        entry.fields.get("query").map(String::as_str),
        Some("[REDACTED]")
    );
    assert_eq!(
        entry.fields.get("stage").map(String::as_str),
        Some("context")
    );
    assert_eq!(
        entry.fields.get("note").map(String::as_str),
        Some("[REDACTED]")
    );
    assert_eq!(entry.context.request_id, "req-001");
}

#[test]
fn cardinality_guard_enforces_budget_under_thousands_of_ids() {
    let mut guard = CardinalityGuard::new(3).unwrap();
    guard.set_budget("request_id", 3).unwrap();
    assert!(guard.observe("request_id", "r1").unwrap());
    assert!(guard.observe("request_id", "r2").unwrap());
    assert!(guard.observe("request_id", "r3").unwrap());
    assert!(!guard.observe("request_id", "r2").unwrap());
    let err = guard.observe("request_id", "r4").unwrap_err();
    assert!(err.to_string().contains("cardinality exceeded"), "{err}");
    assert_eq!(guard.distinct_count("request_id"), 3);

    // High-volume simulation: default budget blocks unique request IDs as labels.
    let mut high = CardinalityGuard::with_default_budget();
    high.set_budget("stage", 4).unwrap();
    for i in 0..10_000 {
        let id = format!("req-{i}");
        if i < DEFAULT_MAX_LABEL_CARDINALITY as usize {
            high.observe("request_id", &id).unwrap();
        } else {
            assert!(high.observe("request_id", &id).is_err());
            break;
        }
    }
    for stage in ["parse", "retrieve", "rerank", "generate"] {
        high.observe("stage", stage).unwrap();
    }
    assert!(high.observe("stage", "extra").is_err());
}

#[test]
fn slo_error_budget_burn_and_remaining() {
    let slo = sample_slo(
        0.99,
        0.1,
        vec![
            SloFailureDomain::Provider,
            SloFailureDomain::Storage,
            SloFailureDomain::Application,
        ],
    )
    .unwrap();
    assert!((slo.error_budget_ratio() - 0.01).abs() < 1e-9);
    assert_eq!(slo.allowed_failures(10_000), 100);
    let ok = slo.budget_status(10_000, 50).unwrap();
    assert!(!ok.burned_out);
    assert_eq!(ok.remaining_failures, 50);
    let burned = slo.budget_status(10_000, 101).unwrap();
    assert!(burned.burned_out);
    assert_eq!(burned.remaining_failures, 0);
    assert!(slo.budget_status(10, 11).is_err());
}

#[test]
fn slo_rejects_invalid_targets() {
    assert!(LatencyTarget::new(0, 100).is_err());
    assert!(LatencyTarget::new(101, 100).is_err());
    assert!(LatencyTarget::new(50, 0).is_err());
    assert!(sample_slo(1.5, 1.0, vec![SloFailureDomain::Cache]).is_err());
    assert!(sample_slo(0.99, 0.0, vec![SloFailureDomain::Cache]).is_err());
    assert!(sample_slo(0.99, 1.0, vec![]).is_err());
}

#[test]
fn decode_slo_rejects_invalid_nested_latency() {
    // Serde accepts raw structs; validate/decode must fail closed on nested latency.
    let zero_percentile = br#"{
        "schema_version": 1,
        "name": "ask_success",
        "description": "ok",
        "success_ratio_target": 0.99,
        "latency": {"percentile": 0, "max_latency_ms": 100},
        "window_secs": 3600,
        "sampling_ratio": 0.1,
        "retention_secs": 86400,
        "failure_domains": ["cache"]
    }"#;
    let err = decode_slo_definition_json(zero_percentile).unwrap_err();
    assert!(
        err.to_string().contains("percentile"),
        "expected percentile error, got {err}"
    );

    let zero_max_latency = br#"{
        "schema_version": 1,
        "name": "ask_success",
        "description": "ok",
        "success_ratio_target": 0.99,
        "latency": {"percentile": 99, "max_latency_ms": 0},
        "window_secs": 3600,
        "sampling_ratio": 0.1,
        "retention_secs": 86400,
        "failure_domains": ["cache"]
    }"#;
    let err = decode_slo_definition_json(zero_max_latency).unwrap_err();
    assert!(
        err.to_string().contains("max_latency_ms"),
        "expected max_latency_ms error, got {err}"
    );

    let both_zero = br#"{
        "schema_version": 1,
        "name": "ask_success",
        "description": "ok",
        "success_ratio_target": 0.99,
        "latency": {"percentile": 0, "max_latency_ms": 0},
        "window_secs": 3600,
        "sampling_ratio": 0.1,
        "retention_secs": 86400,
        "failure_domains": ["cache"]
    }"#;
    assert!(decode_slo_definition_json(both_zero).is_err());

    // Direct validate path (bypass constructor) must also reject nested latency.
    let mut slo = sample_slo(0.99, 0.1, vec![SloFailureDomain::Cache]).unwrap();
    slo.latency = LatencyTarget {
        percentile: 0,
        max_latency_ms: 0,
    };
    assert!(slo.validate().is_err());
}

#[test]
fn serde_roundtrip_and_unknown_schema_fail_closed() {
    let ctx = full_ctx();
    let bytes = serde_json::to_vec(&ctx).unwrap();
    let decoded = decode_trace_context_json(&bytes).unwrap();
    assert_eq!(decoded, ctx);

    let mut span = SpanSpec::open(SpanOpenParams {
        name: "parse".into(),
        context: ctx.clone(),
        start_unix_ms: 5,
    })
    .unwrap();
    span.add_link(SpanLink::new("trace-z", "span-z").unwrap())
        .unwrap();
    span.end_ok(15).unwrap();
    let span_bytes = serde_json::to_vec(&span).unwrap();
    assert_eq!(decode_span_spec_json(&span_bytes).unwrap(), span);

    let metric = MetricSpec::new(
        "verbatim_queue_depth",
        MetricKind::Gauge,
        "In-flight queue depth",
        "1",
        vec![MetricLabelSpec::approved("queue", 8).unwrap()],
    )
    .unwrap();
    let metric_bytes = serde_json::to_vec(&metric).unwrap();
    assert_eq!(decode_metric_spec_json(&metric_bytes).unwrap(), metric);

    let policy = RedactionPolicy::strict_default();
    let policy_bytes = serde_json::to_vec(&policy).unwrap();
    assert_eq!(decode_redaction_policy_json(&policy_bytes).unwrap(), policy);

    let entry = LogEntry::build(LogEntryParams {
        level: LogLevel::Warn,
        message: "partial results".into(),
        context: ctx,
        fields: BTreeMap::new(),
        timestamp_unix_ms: 1,
        policy,
    })
    .unwrap();
    let entry_bytes = serde_json::to_vec(&entry).unwrap();
    assert_eq!(decode_log_entry_json(&entry_bytes).unwrap(), entry);

    let slo = SloDefinition::new(SloDefinitionParams {
        name: "retrieve_latency".into(),
        description: "Retrieval p99".into(),
        success_ratio_target: 0.999,
        latency: LatencyTarget::new(99, 500).unwrap(),
        window_secs: 3600,
        sampling_ratio: 0.25,
        retention_secs: 86_400,
        failure_domains: vec![SloFailureDomain::Cache, SloFailureDomain::Storage],
    })
    .unwrap();
    let slo_bytes = serde_json::to_vec(&slo).unwrap();
    let slo2 = decode_slo_definition_json(&slo_bytes).unwrap();
    assert_eq!(slo2.name, slo.name);
    assert_eq!(slo2.latency, slo.latency);

    let bad = br#"{"schema_version":99,"request_id":"r"}"#;
    let err = decode_trace_context_json(bad).unwrap_err();
    assert!(err.to_string().contains("unsupported"), "{err}");
}

#[test]
fn span_end_before_start_rejected() {
    let mut span = SpanSpec::open(SpanOpenParams {
        name: "x".into(),
        context: req_ctx(),
        start_unix_ms: 100,
    })
    .unwrap();
    assert!(span.end_ok(50).is_err());
}
