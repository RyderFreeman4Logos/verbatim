# Source / evidence metadata contract (META-001)

Status: walking skeleton for
[#336](https://github.com/RyderFreeman4Logos/verbatim/issues/336).
Code: `crates/verbatim-core/src/source_metadata.rs`.

## Problem

Queryable metadata (title, dates, language, lifecycle, ACL/rights, tags, etc.)
can arrive from front matter, native source metadata, filesystem state,
deterministic parsers, user overrides, or model guesses. Untyped JSON cannot
express type, origin, confidence, scope, or deterministic override rules.
Model-generated filenames must not become authoritative titles or benchmark
gold. Lower-trust origins must never weaken ACL, lifecycle, or rights.

## Contract summary

| Type | Role |
| --- | --- |
| `MetadataFieldName` | Well-known + namespaced custom field keys |
| `MetadataOrigin` | source-native / front-matter / parser / filesystem / user / deterministic-rule / model-derived |
| `MetadataScope` | source / snapshot / evidence / collection |
| `MetadataValue` / `MetadataValueType` | Typed payloads (text, datetime, URL, lists, lifecycle, …) |
| `MetadataConfidence` | high / medium / low / hint-only |
| `MetadataProvenance` | Origin, extractor id, observation time, reason, superseded link |
| `SourceMetadataField` | Single typed observation with full provenance |
| `SourceMetadata` | Field map + superseded audit trail + schema version |
| `SOURCE_METADATA_SCHEMA_VERSION` | Wire schema; unknown versions fail closed |
| `origin_precedence_rank` | Field-specific deterministic origin ranking |
| `markdown_thread_filename_hint` | Filename → hint-only title helper |

### Required observation attributes

Every queryable field observation records:

1. field name (typed enum / custom key)
2. value type + typed value
3. origin
4. confidence
5. extractor / override identity
6. observation time (Unix seconds, UTC)
7. scope
8. provenance (why selected or superseded)

### Deterministic precedence

Precedence is **field-specific**. Base origin trust is:

`user > source-native > front-matter > deterministic-rule > parser > filesystem > model-derived`

Specializations:

- **Title**: filesystem/filename is lowest and **never** becomes the winner;
  use `markdown_thread_filename_hint` / `SourceMetadataField::filename_hint`.
- **Dates**: source-native and front matter outrank filesystem mtime; timezone
  strings are preserved (no silent UTC coercion in the contract layer).
- **Lifecycle / classification / rights / ACL**: model-derived and other
  low-trust origins cannot install values; candidates that **weaken** a more
  restrictive protected state are rejected even when origin rank would allow
  replacement.

Superseded candidates are retained in `SourceMetadata.superseded` for audit.

### Schema identity

`schema_version` must equal `SOURCE_METADATA_SCHEMA_VERSION` (currently `1`).
`decode_source_metadata_json` and `SourceMetadata::validate_schema` fail closed
on unknown versions.

### Strict query surface

`SourceMetadata::require_field` rejects missing fields, hint-only confidence,
and empty datetime values. `approved_fields` projects non-hint winners for
DSL/facets/exports.

## What this slice wires

- Module export from `verbatim-core` (`pub mod source_metadata`)
- Typed fields, provenance, precedence, protected-field rules
- Markdown-thread filename-as-hint helper
- Unit tests for conflicting origins, timezone/missing date, model cannot
  weaken ACL/lifecycle, filename-independent benchmark titles, thread id
  duplicates, multilingual title carry, serde roundtrip, unknown-schema rejection

## What this slice does **not** do (residual)

- Wire ingest/parser adapters or store columns to `SourceMetadata`
- Full Markdown-thread adapter beyond filename hint helper
- DSL/facet/export allow-lists beyond `approved_fields`
- Index/cache invalidation when metadata changes
- Migration of existing untyped JSON metadata
- Closing epic #336

## Integration notes

When a later slice extracts metadata, build `SourceMetadataField` observations
and apply them through `SourceMetadata::apply_candidate` so precedence and
protected-field rules stay centralized. Prefer `require_field` at strict query
boundaries. Do not grow `store.rs`, `main.rs`, or `client.rs` solely to adopt
this contract; keep adapters in non-capped modules. Do not treat model-derived
or filename values as gold for evals/benchmarks.
