# Canonical package

The [versioned normative v1 contract](canonical-package-v1.md) defines manifest
identity/provenance/rights and unit hierarchy, ranges, annotations and selectors.
A package directory contains `manifest.json`, `units.jsonl`, and optional
`relations.jsonl`. Package validation runs before source registration through
the same validator used by the CLI and parser.

```text
verbatim canonical validate path/to/package
verbatim canonical validate path/to/package --format json
```

Legacy single-file canonical `.jsonl` ingest remains supported unchanged. Its
`cjson:v1:` generated IDs remain stable; package IDs are explicitly supplied.
