# Evaluation Fixtures

Verbatim keeps the MVP regression suite deterministic. Fast tests use small in-memory stores, tiny generated PDF fixtures, and mocked model providers for embedding, vision captioning, and verification. They do not require network access or local Qwen/vLLM endpoints.

For the release checklist and manual model-backed smoke sequence, see [mvp.md](mvp.md).

## Fast Regression Suite

Run the focused MVP fixture suite:

```sh
cargo test -p verbatim-core --all-features mvp_regression
```

Run all deterministic core tests:

```sh
cargo test -p verbatim-core --all-features
```

Run the workspace gate used before merging:

```sh
cargo test --workspace --all-features
just pre-commit-fast
```

The `mvp_regression` filter covers Markdown headings and links, plaintext paragraphs, tiny PDF text and diagram fixtures, same-stem source IDs, changed-source re-ingest, source removal, image caption indexing, graph expansion, and verifier pass/revise/fail behavior.

## Optional Model-Backed Eval

Use this only on a machine with `~/.config/verbatim/config.toml` pointing at real OpenAI-compatible embedding and chat endpoints. It is not part of CI.

```sh
tmpdir="$(mktemp -d)"
cat > "${tmpdir}/mvp-eval.md" <<'EOF'
# MVP Eval

Verbatim should answer with a citation when evidence is present.
Graph expansion and image captions are regression-critical behaviors.
EOF

verbatim daemon start
verbatim source add "${tmpdir}/mvp-eval.md"
verbatim ingest
verbatim ask "What behaviors are regression-critical?" --show-retrieval
```

The expected manual signal is an answer with citations plus retrieval debug output. Treat model wording as non-deterministic; inspect citation grounding and retrieval stages rather than exact prose.

## Benchmark Governance (EVAL-015)

Promotion of benchmarks beyond the fast deterministic fixtures follows the
benchmark governance policy:

- Policy: [architecture/benchmark-governance.md](architecture/benchmark-governance.md)
- Schema: [evals/benchmark-manifest.schema.toml](evals/benchmark-manifest.schema.toml)
- Examples: [evals/examples/](evals/examples/)
- Offline validation: `bash scripts/tests/benchmark-governance-tests.sh`

This walking skeleton covers split group isolation, required statistical
fields, contamination baseline declarations, and gold-source rules. Full
harness rewiring remains residual under issue #340.
