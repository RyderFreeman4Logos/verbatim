# Versioned R/A/G wire schemas (API-002)

Status: walking skeleton for
[#353](https://github.com/RyderFreeman4Logos/verbatim/issues/353).
Code: `crates/verbatim-core/src/wire_schemas/` (module facade + common /
identity / ser / envelopes / derived units).

## Problem

R/A/G separation only works when QueryPlan, EvidencePack, ContextPack, derived
artifacts, and workflow runs are **stable public artifacts** rather than internal
Rust structs or prompt strings. Without canonical schema versions and identity
hooks, caches, signatures, migration, third-party workflows, and audit replay
are brittle.

## Design direction

Define explicit schema versions, canonical identity (kind + id + content hash),
deterministic JSON serialization helpers, and fail-closed decode for unknown
versions. Optional extensions and multi-version dual-shape decode remain
residual; this slice freezes the **contract shape** only.

## Contract summary

| Type | Role |
| --- | --- |
| `WIRE_SCHEMA_VERSION` | Current wire schema (`1.0.0`); unknown versions fail closed |
| `WireSchemaVersion` | Semantic `major.minor.patch` stamp for envelopes |
| `WireArtifactKind` | `query_plan` / `evidence_pack` / `context_pack` / `derived_artifact` / `workflow_envelope` |
| `ContentHash` | Non-empty, whitespace-free content digest |
| `CanonicalIdentity` | Kind + schema + artifact id + content hash |
| `WireEnvelopeHeader` | Shared header: schema, identity, optional generation / profile |
| `QueryPlanEnvelope` | Minimal plan: query text + ordered steps |
| `EvidencePackEnvelope` | Direct evidence unit ids bound to a QueryPlan hash |
| `ContextPackEnvelope` | Selected units + EvidencePack hash (+ optional model fingerprint) |
| `DerivedArtifactEnvelope` | Generated/draft/report product bound to a source pack hash |
| `WorkflowEnvelope` | Phase + QueryPlan / optional pack hashes for a run |
| `encode_wire_document` / `decode_*_json` | Compact JSON + fail-closed decode helpers |
| `wire_content_hash` | Hex SHA-256 of canonical body bytes |

### Identity and hashing

1. Body fields (excluding the header) are serialized with `encode_wire_document`.
2. `content_hash = wire_content_hash(body_bytes)` (hex SHA-256).
3. `CanonicalIdentity` stores kind, schema version, artifact id, and that hash.
4. Decode re-validates schema version, structural fields, and that the declared
   content hash matches a re-encoded body.

Query text alone is never a valid cache key; adapters must use identity /
content hashes (see also `cache_identity`).

### Fail-closed rules

- Schema version must equal `WIRE_SCHEMA_VERSION` (`1.0.0`).
- Empty or whitespace-only artifact ids, digests, optional generation/profile
  refs, and required body fields are rejected.
- Envelope identity `kind` must match the concrete envelope type.
- Tampered content hashes fail `validate` / decode.

### Layering

| Layer | Module | Notes |
| --- | --- | --- |
| Wire contracts (this) | `wire_schemas` | Public R/A/G envelopes + identity |
| Cache keys | `cache_identity` | Consumes QueryPlan / ContextPack hashes |
| Migration stamps | `migration_framework::ArtifactKind` | Overlapping kind names; not the same wire types |
| Publication | `index_publication` | Generation fencing for indexes |

Adapters should project full domain types into these envelopes at process
boundaries rather than leaking internal structs across the wire.

## What this slice wires

- Module export from `verbatim-core` (`pub mod wire_schemas`)
- Pure contract types and deterministic encode/hash helpers
- Unit tests: construction, byte-stable round-trip for all five envelopes,
  unknown-schema fail-closed, invalid identity/hash reject, tampered hash reject

## What this slice does **not** do (residual)

- Full production field sets (locators, policy decisions, timings, warnings,
  omission reasons, dual redacted/full-audit views)
- Multi-version dual-shape decode matrix beyond fail-closed unknown
- JSON Schema / OpenAPI generation and SDK/CLI/daemon adoption
- Live retrieve/ask/generate path migration onto these envelopes
- Closing epic #353
