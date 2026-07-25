# Cross-service observability contract (OBS-001)

Status: walking skeleton for
[#338](https://github.com/RyderFreeman4Logos/verbatim/issues/338).
Code: `crates/verbatim-core/src/observability_contract/` (module facade +
trace/span/metric/log/slo units).

## Problem

Task profiles and model telemetry do not provide one chain across client,
coordinator, storage/index, model endpoints, ContextPack, workflows, generation,
and verification. Distributed failures need correlation without logging private
queries, evidence, credentials, or high-cardinality IDs as metric labels.

## Contract summary

| Type | Role |
| --- | --- |
| `TraceContext` | Correlated IDs: request, retrieval run, ContextPack, workflow, task, publication generation, plus optional OTel-shaped trace/span ids |
| `SpanSpec` / `SpanLink` | Bounded stage spans with start/end/duration/status/attributes/links |
| `MetricSpec` / `MetricLabelSpec` | Low-cardinality counter/histogram/gauge schemas with privacy-reviewed labels |
| `LogEntry` | Structured log record built under automatic redaction |
| `RedactionPolicy` | Redact queries, evidence, paths, keys/tokens, sensitive metadata |
| `CardinalityGuard` | Enforce per-label distinct-value budgets |
| `SloDefinition` / `ErrorBudgetStatus` | Latency + success targets, sampling/retention, error-budget burn |
| `OBSERVABILITY_CONTRACT_SCHEMA_VERSION` | Wire schema; unknown versions fail closed |

### Correlation IDs

Every hop should carry the IDs it knows:

1. `request_id` — end-to-end request (required)
2. `retrieval_run_id` — one retrieval orchestration under the request
3. `context_pack_id` — grounded ContextPack identity
4. `workflow_run_id` — multi-step / GraphRAG / workflow job
5. `task_id` — daemon task identity
6. `publication_generation` — index/publication freshness fence
7. `trace_id` / `span_id` / `parent_span_id` — OpenTelemetry-compatible tree

`TraceContext::child_span` continues a parent tree. `for_async_link` preserves
correlation IDs for queue hops but clears remote span ids so consumers open a
linked span rather than trusting inbound baggage blindly.

### Bounded spans

`SpanSpec` is a pure specification (not a live exporter):

- hard-bounded attributes (`MAX_SPAN_ATTRIBUTES`) and links (`MAX_SPAN_LINKS`)
- status `Unset | Ok | Error` with stable `error_class` on errors
- duration derived from start/end Unix-ms timestamps
- links express queue fan-out/fan-in (`follows_from`, etc.)

Sensitive attribute values must pass through `RedactionPolicy` before attach.

### Metrics and cardinality

`MetricSpec` declares name, kind, unit, and label keys. Each label is either
`Approved` (with `max_cardinality`) or `Prohibited`. Sample maps using
prohibited or undeclared keys fail validation.

`CardinalityGuard` records distinct values per key and refuse-closed when a
budget would be exceeded — including under thousands of unique request IDs
mistakenly used as labels.

### Logs and redaction

`LogEntry::build` applies `RedactionPolicy` to the message and every field.
Default policy redacts known sensitive field names (query, evidence, path,
authorization, tokens, …) and heuristic path/token patterns. Replacement token
is `[REDACTED]`.

### SLOs

`SloDefinition` binds:

- success ratio target and latency percentile/threshold
- accounting window, sampling ratio, retention
- failure domains (`Provider`, `Queue`, `Storage`, `Cache`, `Application`, …)

`error_budget_ratio`, `allowed_failures`, and `budget_status` compute burn
without side effects so adapters can surface residual budget independently of
exporters.

### Schema identity

`schema_version` must equal `OBSERVABILITY_CONTRACT_SCHEMA_VERSION` (currently
`1`). Decode helpers reject unknown versions instead of silently accepting them.

## What this slice wires

- Module export from `verbatim-core` (`pub mod observability_contract`)
- Typed trace/span/metric/log/SLO/redaction/cardinality contracts
- Unit tests: trace propagation, async link, span linking bounds, redaction,
  cardinality under high unique IDs, SLO budget burn, serde + unknown-schema
  rejection

## What this slice does **not** do (residual)

- Wire live daemon/store/retriever stages to emit spans/metrics/logs
- OTLP exporter or vendor backend
- Trust-policy for inbound baggage beyond `for_async_link` ID allow-list
- Runtime sampling engines, retention workers, or dashboard alerts
- Closing epic #338

## Integration notes

When a later slice instruments a stage, construct `TraceContext` at the
authorization/ingress boundary, open a `SpanSpec` per stage, link queue work
with `SpanLink`, emit only `MetricSpec`-declared labels under a
`CardinalityGuard`, and build logs via `LogEntry::build` with
`RedactionPolicy::strict_default` (or a tighter policy). Prefer adapters in
non-capped modules — do not grow `store.rs`, `main.rs`, or `client.rs` solely to
adopt this contract. Observability failure must not block core retrieval unless
a separate audit-delivery policy explicitly requires it.
