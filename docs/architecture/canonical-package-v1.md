# Canonical package 1.0.0 normative contract

The words MUST and MUST NOT define the admission contract implemented by
`verbatim_core::parser::canonical_package::validate_package`. CLI validation and
source registration use that same function. Legacy standalone JSONL is a separate,
unchanged interface. A package is immutable during validation and ingest.

## Manifest

`manifest.json` is a UTF-8 JSON object. These fields are required:

| Field | Contract |
| --- | --- |
| `schema_version` | Three unsigned decimal components, `1.minor.patch`. Unsupported majors fail with `CANONICAL_PACKAGE_UNSUPPORTED_SCHEMA_MAJOR`. |
| `profile`, `content_kind` | This version supports `bible`, `text`. Other profiles require a defined hierarchy before admission. |
| `work_id`, `version_id`, `language` | Nonempty work, edition and language identifiers. Unit identity MUST match. |
| `canon_id`, `versification_id` | Explicit registry identifiers, currently `protestant-66/v1`. |
| `original_source` | Object with nonempty `format`, `name`, and optional bundled `path`. |
| `original_source_hash` | Lowercase SHA-256 of the original source bytes. |
| `conversion` | Nonempty `adapter`, `converter`, `converter_version`, `original_source_hash`, `output_hash`. The source hash MUST agree with the manifest. |
| `converter_artifact_hash` | Lowercase SHA-256 of the actual converter artifact, distinct from its output hash. |
| `validation` | Nonempty `validator`, `version`, and `status` equal to `passed` or `warnings`. This is a producer claim, never trusted in place of local validation. |
| `rights` | Nonempty `license` identifier and `statement`. `unknown` is explicit uncertainty, never a grant of permission. |

The registry owns book order and reference bounds. Its current non-John bounds
are conservative; this contract does not claim a complete theological
versification matrix or independently authenticate producer provenance.

## Units

`units.jsonl` contains at least one UTF-8 JSON object per nonblank physical line.
Fields: `unit_id`, `source_profile`, `work_id`, `version_id`, `language`,
`components`, `text`, `text_hash`, `backing_selectors`; optional `canon_id` and
`versification_id` MUST agree with the manifest. IDs and text MUST be nonempty.
`text_hash` MUST equal SHA-256 of the exact UTF-8 text, without normalization.
`content_kind` is `text` (default), `verse`, or `footnote`.

`components` MUST be ordered `book`, `chapter`, `verse`; footnotes MUST append
`note`. Every component has string `level`, string `value`, and positive integer
`ordinal`. Book ordinals MUST agree with the canon registry; numeric values use
canonical decimal spelling and MUST equal their ordinals. Book aliases resolve
to the same coordinate. No omitted, repeated or reordered levels are allowed.

Optional `end_components` represents an inclusive text range. It MUST be a
complete bounded book/chapter/verse endpoint at or after the start; footnotes
cannot carry ranges. Range endpoints are retained in Evidence locators and
normalized keys. Text intervals MUST NOT overlap. Units MUST follow canonical
coordinate order, with the verse before its numbered notes. Unit IDs and logical
coordinates MUST be unique, including alias-equivalent coordinates. Gaps are
allowed: a package can be a selection rather than a complete Bible.

Optional `display_citation` is presentation text, not a source of coordinates.
`metadata.section_heading` becomes the Evidence heading path.
`metadata.annotations` contains at most eight string pairs, at most 64 UTF-8 bytes
per key/value, with nonempty keys. Metadata remains in the package even when it
has no corresponding Evidence field.

At least one tagged `BackingSelector` is required:

| `type` | Shape |
| --- | --- |
| `ByteRange` | Unsigned `start < end`, zero-based, end exclusive. |
| `LineRange` | Integer `1 <= start <= end`, inclusive. |
| `TextQuote` | Nonblank `exact`; optional string `prefix`, `suffix`. |
| `XmlId` | Nonempty `id` with no whitespace. |
| `SourceNative` | `scheme: "usfm"`, `value: "BOOK chapter:verse"` resolving to the unit's start using registry book IDs. |

## Relations and validation

Optional `relations.jsonl` records `relation_type`, `from_unit_id`, `to_unit_id`.
The supported relation is `footnote_references_verse`; both endpoints MUST exist
and respectively have `footnote` and `verse` content kinds. Notes stay separate
from verse text and verse chunks.

Reports provide `valid`, schema version, unit count, complete declared manifest,
package hash, original source/conversion provenance, unit locators/text hashes,
ordered diagnostics with stable `code` and package-relative `location`, and a
machine-readable SHA-256 `report_hash`. Locations use `file:physical-line` for
records and `manifest.json:field` for manifest fields. Invalid admission precedes
`Store::add_source`. Producer validation claims never bypass local checks.

## Files and hashes

The complete layout is `manifest.json`, `units.jsonl`, optional `relations.jsonl`,
optional `assets/`, and optional `source/`. Unknown root files/directories,
symlink members, special files and non-UTF-8 names MUST be rejected before member
content is opened. File names are portable relative paths: no empty/dot/parent
components, backslashes, colons or control characters.

`manifest.files` is an optional array (default empty) of objects with `path`,
`sha256` and `media_type`. Every file below `assets/` or `source/` MUST appear
exactly once; every entry MUST exist as a regular file, have a lowercase SHA-256
matching its bytes, and have a nonempty `type/subtype` media type. Missing,
unlisted, duplicate, unsafe or corrupt files are errors. Empty directories have
no semantic content. Assets are retained package files, never implicit Evidence.

If `original_source.path` is supplied, it MUST point to an inventoried `source/`
file whose digest equals `original_source_hash`. Without bundled original bytes,
validation emits `CANONICAL_PACKAGE_SOURCE_NOT_BUNDLED`: the claim is preserved,
but its digest cannot be verified locally. `conversion.output_hash` MUST equal
SHA-256 of the exact `units.jsonl` bytes, including whitespace and line endings.
Converter artifact hashes are declarations of identity, not authenticity proofs.

The package hash covers every regular package file, including source and assets,
in lexicographic portable-path order. Each file contributes big-endian u64 name
byte length, UTF-8 name bytes, big-endian u64 content length, and content bytes.
The report hash is SHA-256 of compact UTF-8 JSON for the report with `report_hash`
omitted and object keys sorted recursively. Neither hash includes the package's
absolute directory path. Even invalid reports use the same report-hash rule.

## Warning and migration policy

Warnings are separate from error `diagnostics` and do not make `valid` false.
A producer `validation.status` of `warnings` emits
`CANONICAL_PACKAGE_PRODUCER_WARNINGS`; a rights license of `unknown` emits
`CANONICAL_PACKAGE_RIGHTS_UNKNOWN`. Human and JSON reports include warnings.
`canonical validate --deny-warnings` returns exit 1 for either errors or warnings;
the default returns exit 1 only for errors, otherwise 0. `--format json` includes
the same report hash as human output. Local validation never contacts a daemon.

A multilingual package declares `language: "mul"`; each unit still supplies its
own nonempty language identifier. Other manifests require exact language matches.
Relations MUST be unique, and multiple footnotes use distinct note ordinals.

`verbatim canonical migrate legacy.jsonl new-package --manifest identity.json`
is the compatibility adapter. An identity template supplies `language` and
`rights`; it may explicitly supply work/edition and registry identifiers. Missing
work/edition come from the first legacy record. Unknown edition identity is not
invented. Registry IDs default to the current supported registry; input record
identity must agree with the completed manifest. Missing unit languages use the
manifest language, or `und` for a multilingual manifest. Existing record fields,
metadata and display citations are preserved; no endpoint is inferred from
presentation text. Legacy generated Evidence IDs are reused unchanged.

The adapter bundles the exact original JSONL as `source/original.jsonl`, hashes
its own running binary as the converter artifact, and computes output integrity.
Its provenance starts at this JSONL input; it does not claim to reconstruct an
unknown upstream EPUB conversion history. It uses a temporary sibling directory
and publishes only after the shared validator succeeds. The destination MUST be
absent and owned by the caller throughout migration; existing targets are never
intentionally replaced. Invalid migration leaves no destination or staging files.

Example identity template (rights must describe the actual input):

```json
{"language":"en","rights":{"license":"LicenseRef-Private","statement":"Local use only; no redistribution."}}
```

The public `complete` fixture retains the canonical Bible fixture's fields and
adds a separately backed synthetic note, a relation and an inventoried asset.
Private corpora use the same CLI adapter; compare the result against the specific
input record count and hash. Private source text is not committed. Standalone `.jsonl` parsing and ingest
remain unchanged, including their original generated IDs and language semantics.
