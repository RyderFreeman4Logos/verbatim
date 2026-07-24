# Supply-chain provenance (SUPPLY-001)

Status: walking skeleton for
[#341](https://github.com/RyderFreeman4Logos/verbatim/issues/341).
Machine-readable schema:
[`../supply-chain/provenance-manifest.schema.toml`](../supply-chain/provenance-manifest.schema.toml).
Examples: [`../supply-chain/examples/`](../supply-chain/examples/).
Validation: `bash scripts/tests/supply-chain-provenance-tests.sh`.
Optional SBOM generation: `bash scripts/generate-workspace-sbom.sh`.

This document defines the **shape** of Verbatim release, model, workflow,
adapter, and container provenance. It does **not** close the full SUPPLY-001
epic (signing, multi-arch container SBOMs, automated model hash pipelines, or
release CI attestations remain residual).

## Component classes

Every runtime profile and release attestation must account for these classes.
Each class is a distinct trust boundary with its own digest, identity, and
revocation surface:

| Class | Examples | What must be identifiable |
| --- | --- | --- |
| `binary` | `verbatim`, `verbatim-daemon` release artifacts | name, version, content digest, build target/platform |
| `crate` | Rust crates and native shared libraries linked into builds | name, version, source (registry/git/path), content or lock identity |
| `container` | OCI images used for offline or operator deploys | image name/tag or digest, platform, optional SBOM digest |
| `model` | embedding models, tokenizers, quant packs | artifact id, version or revision, content digest, optional license |
| `workflow` | skills, workflow packs, operator playbooks | id, version, content digest, source path or URL |
| `parser` | OCR tools, converters, extractors, thin adapters | tool name, version or binary digest, invocation policy reference |

Aliases accepted in manifests (normalized by validators):

- `crate` ← `native`, `native-dep`, `dependency`
- `model` ← `tokenizer`, `quant`, `model-tokenizer-quant`
- `workflow` ← `skill`, `workflow-skill`
- `parser` ← `ocr`, `converter`, `adapter`, `parser-ocr-converter-adapter`

## Baseline already in tree

| Control | Location | Role under SUPPLY-001 |
| --- | --- | --- |
| License + advisory policy | [`deny.toml`](../../deny.toml), `just deny` | crate license allow-list, RustSec advisories, unknown source denial |
| Lockfile | `Cargo.lock` | reproducible crate graph for workspace builds |
| Upstream reuse recording | [`upstream-first.md`](./upstream-first.md), substitution matrix | points at SUPPLY-001 for hashes/features/licenses; does not replace it |

`cargo deny` remains the **license/advisory baseline**. SBOM generation and
runtime-profile digests extend that baseline; they do not invent a second
license ledger.

## CycloneDX SBOM generation (Rust workspace)

Preferred path for the Rust workspace is **CycloneDX JSON** via
[`cargo-cyclonedx`](https://github.com/CycloneDX/cyclonedx-rust-cargo)
(`cargo cyclonedx`).

Operator entry:

```sh
bash scripts/generate-workspace-sbom.sh
```

Behavior:

1. Requires `cargo cyclonedx` on `PATH` (or `CARGO_CYCLONEDX` override).
2. Writes JSON SBOMs under `target/sbom/` (gitignored via the `target/` build
   directory symlink; **do not commit** generated SBOM blobs).
3. If the tool is missing, the script exits non-zero with install instructions.
4. Unit/gate tests for this walking skeleton **do not** require
   `cargo-cyclonedx`; only the generate script does.

Suggested install (operators, not a gate dependency):

```sh
cargo install cargo-cyclonedx --locked
```

## Runtime / profile digests

A **runtime profile** is a TOML document listing the concrete artifacts an
operator or offline host intends to trust for a named profile. Schema and
required fields live in
[`../supply-chain/provenance-manifest.schema.toml`](../supply-chain/provenance-manifest.schema.toml).

Minimum shape:

- `schema_version` (currently `1`)
- `profile_id`
- `app` identity: name, version, git SHA placeholder or concrete SHA
- `[[components]]` covering **every** required class above, each with:
  - `id`, `class`, `name`
  - `digest_alg` (e.g. `sha256`) and `digest` (lowercase hex)
  - class-specific identity fields (`version`, `source`, `platform`, …)

Example: [`../supply-chain/examples/runtime-profile.example.toml`](../supply-chain/examples/runtime-profile.example.toml).

## Allow / deny and revocation policy **shape**

This skeleton defines data shape only (no enforcement daemon yet):

| Document | Purpose |
| --- | --- |
| Runtime profile | Positive allow-list of digests and identities for a profile |
| Revocation list | Explicit deny of digests (compromised, withdrawn, wrong quant, …) |

Revocation example:
[`../supply-chain/examples/revocation.example.toml`](../supply-chain/examples/revocation.example.toml).

**Strict mode** (implemented by the validation harness): if any component digest
in a profile appears in the active revocation list, validation **fails closed**.
Missing required classes also fail closed. Unknown classes fail closed unless
listed as aliases of a required class.

Future allow/deny policy may add:

- profile-scoped allow digests only (default deny unknown)
- expiry / `not_after` on allow entries
- signed profile envelopes (residual)

## Operator checklist

### Verification

1. Confirm `Cargo.lock` is committed with the release and `just deny` is clean.
2. Generate workspace SBOMs: `bash scripts/generate-workspace-sbom.sh`.
3. Build or obtain release binaries; record `sha256` digests in a runtime profile.
4. Record model/tokenizer/quant digests (manual or pipeline) under class `model`.
5. Record workflow/skill and parser/OCR/adapter identities and digests.
6. Validate profile + revocation:

   ```sh
   bash scripts/tests/supply-chain-provenance-tests.sh
   ```

   For a custom pair of fixtures, set:

   ```sh
   SUPPLY_PROFILE_PATH=/path/to/profile.toml \
   SUPPLY_REVOCATION_PATH=/path/to/revocation.toml \
   SUPPLY_STRICT=1 \
     bash scripts/tests/supply-chain-provenance-tests.sh
   ```

### Rotation

1. Bump app version / git SHA in the profile when shipping a new binary.
2. Replace component digests for any rebuilt artifact; never rewrite history of
   a published profile id without a new `profile_id` or version field.
3. Re-run SBOM generation after dependency changes; keep deny baseline green.
4. Rotate model/tokenizer digests when weights or tokenizers change; treat old
   digests as candidates for revocation if unsafe.

### Revocation

1. Append the compromised digest to the revocation list with `reason` and
   `revoked_on` (ISO date).
2. Re-validate all active profiles in strict mode; fail closed until profiles
   stop referencing revoked digests.
3. Publish the updated revocation list with the same distribution channel as
   profiles (path TBD under residual release CI work).

### Offline use

1. Carry: runtime profile, revocation list, SBOM JSON under `target/sbom/` (or a
   copied offline bundle), and `deny.toml` / lockfile snapshot for the build.
2. Verify digests of binaries, models, and tools against the profile **before**
   first use on an air-gapped host.
3. Do not load a model, skill, or parser whose digest is absent from the profile
   or present on the revocation list.
4. Optional CycloneDX tooling may be unavailable offline; validation of the
   TOML profile/revocation pair does not require it.

## How to run validation

```sh
bash scripts/tests/supply-chain-provenance-tests.sh
```

The justfile is agent-immutable; there is no `just` recipe for this skeleton.
Operators and agents must invoke the bash entry above. The script:

- validates the committed schema and example fixtures
- checks required component classes
- rejects a synthetic profile missing a required class
- rejects a profile that references a revoked digest in strict mode

## Residual (explicitly deferred)

- Full release signing and key ceremony
- Container multi-arch SBOM attachment and registry attestations
- Automated model weight / tokenizer hash pipeline
- Release CI provenance attestations (SLSA / GitHub artifact attestations)
- Runtime enforcement inside `verbatim-daemon` (policy load + deny)
- Closing epic #341 / SUPPLY-001

## Related work

- Parent coordination: `ROADMAP-004` / issue #341
- License and advisory baseline: `deny.toml`, `just deny`
- Upstream adoption recording: [`upstream-first.md`](./upstream-first.md)
- Ingest tool trust boundary (execution, not supply digests):
  [`ingest-security-boundary.md`](./ingest-security-boundary.md)
