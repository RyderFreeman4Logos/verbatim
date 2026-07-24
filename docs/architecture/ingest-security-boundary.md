# Ingest security boundary (INGEST-SEC-001)

Status: walking skeleton for
[#337](https://github.com/RyderFreeman4Logos/verbatim/issues/337).
Code: `crates/verbatim-core/src/ingest_security.rs`.

## Policy summary

Every parser, converter, archive tool, OCR adapter, and filesystem path accepted
at ingest is an **untrusted execution boundary**. Defaults are fail-closed:

| Control | Default |
| --- | --- |
| `allow_network` | `false` |
| `max_expanded_archive_bytes` | 512 MiB |
| `max_archive_members` | 10_000 |
| `max_archive_nesting_depth` | 3 |
| `max_wall_time_seconds` | 120 |
| `max_stdout_bytes` / `max_stderr_bytes` | 4 MiB / 64 KiB |
| `quarantine_on_failure` | `true` |
| image dimensions / pixels | composed from `ImageArtifactLimits` |

Helpers provided in this slice:

- `safe_join` / `validate_contained_path` — zip-slip and absolute/symlink escape
  rejection under a containment root
- `InputSnapshotIdentity` — content digest + size (+ optional mtime/inode)
- pure archive bound checks for member count, nesting depth, and expanded bytes
- `IngestSecurityPolicy::apply_to_external_command` — strips proxy / network
  client environment keys when `allow_network` is false

## What this slice wires

- Module export from `verbatim-core`
- OCR external-command spawn (`ocr::configure_ocr_command`) applies the default
  policy env hardening and documents network denial
- Unit tests cover adversarial path cases, policy defaults, archive bounds, and
  snapshot identity

## What this slice does **not** do (residual)

- Full OS sandbox (bubblewrap, landlock, seccomp)
- Complete archive extractor rewrite / jail
- Fuzz harness infrastructure and corpus expansion
- Wiring every ingest entry point in `ingest.rs` (monolith-capped; helpers live
  in `ingest_security.rs` for call sites to adopt incrementally)
- Closing epic #337

## Integration notes

Prefer calling `safe_join` / `validate_contained_path` / `InputSnapshotIdentity`
at the earliest filesystem boundary that accepts relative or archive-derived
paths. Do not grow `ingest.rs` solely to wire these helpers; adopt from
non-capped modules or thin call sites as extractors are touched.
