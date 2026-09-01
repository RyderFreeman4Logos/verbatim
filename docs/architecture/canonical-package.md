# Canonical package v1

A canonical package is a directory containing exactly the required walking-skeleton files:

- `manifest.json`: schema version, profile, content kind, work ID, edition/version ID, language, and optional original-source SHA-256 plus `DerivedConversionMetadata`.
- `units.jsonl`: one canonical unit per line. Units retain the existing canonical JSONL fields and IDs (`cjson:v1:`), must match manifest identity fields, include text, and include at least one serialized `BackingSelector`. Supplied selectors are retained; the walking skeleton resolves `SourceNative { scheme: "usfm" }` against the unit's canonical reference. An optional `text_hash` must match the text.

Validate locally without a daemon:

```text
verbatim canonical validate path/to/package
verbatim canonical validate path/to/package --format json
```

Validation is fail-closed for unsupported schema majors and malformed or unresolvable source-native selectors. JSON reports expose package hash separately from original-source/converter provenance and each unit's canonical locator, selector, and text hash. Package validation runs before source registration; invalid packages do not create source or index state.

Legacy single-file canonical `.jsonl` ingest remains supported unchanged. This v1 boundary does not yet cover relations, assets, source trees, exhaustive hierarchy validation, or migration.
