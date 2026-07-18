# MVP Release Gate

This guide is the release checklist for the Verbatim MVP. It is written for a
fresh clone and keeps model-backed steps manual so CI does not require local
Qwen/vLLM endpoints.

The current MVP workspace version is `0.1.90`; `Cargo.toml` and every
workspace package entry in `Cargo.lock` must match this exact version.

## MVP Boundary

Verbatim MVP includes:

- Local daemon plus thin CLI.
- SQLite metadata/text storage, instant-distance HNSW vector search, and
  Tantivy BM25 search.
- Pure Rust PDF parsing through `pdf_oxide` by default, with `pdfplumber` behind
  an optional feature.
- PDF text evidence, PDF image artifact evidence, and optional generated image
  caption evidence.
- Hybrid retrieval with reciprocal-rank fusion.
- Evidence inspection and citation mapping through `verbatim evidence <eid>`.
- Citation verification, enabled by default.
- Contextual retrieval, enabled by default.
- Optional rerank, disabled by default and config-gated.
- Graph expansion, enabled by default with bounded hops.
- LLM graph extraction and full GraphRAG/global search as optional,
  config-gated MVP capabilities.
- Optional Qdrant vector sync/search, disabled by default and config-gated.
- Deterministic MVP regression fixtures. See [evals.md](evals.md).

Post-MVP:

- A Qdrant backend is not required for build, smoke, or regression gates.

Open issue audit for this gate:

- `gh issue list --repo RyderFreeman4Logos/verbatim --state open --limit 100`
  on 2026-06-22 showed #48 and #17 only.
- #48 is this MVP release documentation gate.
- #17 is implemented as an opt-in enhancement; it remains disabled for MVP
  validation unless explicitly configured.

## Prerequisites

Install:

- Rust stable and Cargo.
- `just`.
- `cargo-nextest` for `just test`.
- `cargo-deny` for `just deny` and `just pre-commit-fast`.
- `lefthook` if you want local git hooks installed.
- Tokuin pinned to the gate's audited revision:

  ```sh
  cargo install --git https://github.com/RyderFreeman4Logos/tokuin.git --rev c68d1f804a4c172846716b7be99e9378e16512b7 --locked tokuin
  tokuin --version  # must print: tokuin 0.3.0
  ```

The monolith gate bounds every Tokuin process to 30 seconds. On slower
machines, `MONOLITH_TOKENIZER_TIMEOUT_SECONDS` may be set to an integer from 1
through 300; invalid values and tokenizer timeouts fail the gate closed.

Clone and build:

```sh
git clone https://github.com/RyderFreeman4Logos/verbatim.git
cd verbatim
just build
```

After bumping the workspace version above `origin/main` on your branch, run the fast deterministic release gate:

```sh
just pre-commit-fast
```

Run deterministic regression fixtures:

```sh
cargo test -p verbatim-core --all-features mvp_regression
```

Run the full deterministic workspace test gate when preparing a release:

```sh
cargo test --workspace --all-features
```

## Install

From a clone, either run binaries through Cargo:

```sh
cargo run -p verbatim-cli -- --help
cargo run -p verbatim-daemon
```

Or install release binaries:

```sh
just install
verbatim --help
verbatim-daemon --help
```

`just install` builds `verbatim` and `verbatim-daemon` with all features and
installs them to `/usr/local/bin`.

For this repository's local `systemd --user` daemon workflow, use the explicit
user-daemon deploy recipe after merging a binary change:

```sh
just install-local-daemon
```

It builds release `verbatim` and `verbatim-daemon`, installs them to
`~/.local/bin` by default, reloads the user systemd manager, restarts
`verbatim.service`, and verifies the installed versions plus `verbatim daemon
status`. Use `VERBATIM_LOCAL_BIN_DIR=/path/to/bin` or
`VERBATIM_SYSTEMD_USER_SERVICE=name` for non-default local setups; the service
name selects both `~/.config/systemd/user/<name>.service` and the
`systemctl --user` target. The readiness check waits up to 90 seconds by
default; set `VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS=<seconds>` to a
positive integer when a local populated DB/index needs a different bounded
startup window. If readiness times out, the recipe reports the selected service
unit and prints recent `systemctl --user status` plus `journalctl --user -u`
diagnostics before failing. If the selected unit is missing and unit writes are
disabled, or if the selected unit points at a different `verbatim-daemon`, the
recipe fails before installing binaries instead of silently installing to the
wrong path; set
`VERBATIM_LOCAL_DAEMON_WRITE_UNIT=1` when you intentionally want to regenerate
the user unit.

Developers who want local automation can opt in to git hooks; hooks are not
installed by default:

```sh
just install-local-daemon-hook post-merge
# optional, more aggressive: just install-local-daemon-hook post-push
just remove-local-daemon-hook post-merge
```

Use these hooks only for local clones where automatically replacing the running
daemon after a merge or push is desired. Config-only reload-safe changes should
still be verified through daemon hot reload; binary replacement is the case that
requires a restart.

## Config

Create the local config:

```sh
cargo run -p verbatim-cli -- config init
cargo run -p verbatim-cli -- config validate
```

Installed form:

```sh
verbatim config init
verbatim config validate
```

The config path is `~/.config/verbatim/config.toml`. Data lives under
`~/.local/share/verbatim/` by default.

Minimal local Qwen/vLLM endpoint settings:

```toml
[embedding]
enabled = true
provider = "openai_compatible"
base_url = "http://gb10:18002/v1"
model = "Qwen/Qwen3-Embedding-8B"
dimension = 4096
api_key = ""

[retrieval]
dense_top_k = 80
bm25_top_k = 50
rrf_k = 60
default_limit = 12
default_page_size = 1

[vector_index]
residency = "low_memory" # or "resident_hnsw" for faster queries with higher RAM use

[chat]
enabled = true
provider = "openai_compatible"
base_url = "http://gb10:18009/v1"
model = "qwen3.6-27B"
temperature = 0.0
api_key = ""

[verifier]
enabled = true
```

If your model server expects a key, set the `api_key` field for that role. The
daemon redacts secret-like fields in `verbatim config show`.
Dense retrieval defaults to low-memory residency: the daemon keeps vectors in
SQLite and scans them at query time instead of loading the complete local HNSW
index. Set `[vector_index] residency = "resident_hnsw"` when lower query latency
is worth the extra resident memory.

## Daemon

Run the daemon in the foreground from source:

```sh
cargo run -p verbatim-daemon
```

Or after install:

```sh
verbatim daemon start
```

Check health from another terminal:

```sh
cargo run -p verbatim-cli -- daemon status
```

Installed form:

```sh
verbatim daemon status
```

Optional systemd user service:

```sh
verbatim daemon install
systemctl --user daemon-reload
systemctl --user enable --now verbatim
```

## Manual Smoke Sequence

This is intentionally documented instead of committed as an E2E script because
ingest and ask require live local model endpoints.

Terminal 1, start the daemon:

```sh
cargo run -p verbatim-daemon
```

Terminal 2, create a small source:

```sh
tmpdir="$(mktemp -d)"
cat > "${tmpdir}/mvp-smoke.md" <<'EOF'
# MVP Smoke

Verbatim answers with grounded citations.
Graph expansion and image caption indexing are MVP regression behaviors.
EOF
```

Add and inspect the source:

```sh
cargo run -p verbatim-cli -- source add "${tmpdir}/mvp-smoke.md"
cargo run -p verbatim-cli -- source list
cargo run -p verbatim-cli -- source inspect <source-id>
```

Create and sync example collections without depending on local paths in tests:

```sh
cargo run -p verbatim-cli -- collection create articles
cargo run -p verbatim-cli -- collection add-root articles ../drafts/articles/articles
cargo run -p verbatim-cli -- collection sync articles

cargo run -p verbatim-cli -- collection create areskapitalon
fd 'Areskapitalon.*\.md' ../drafts/articles/articles \
  | cargo run -p verbatim-cli -- collection sync areskapitalon --stdin
```

Retrieve or ask within the materialized collection membership without rescanning
those roots:

```sh
cargo run -p verbatim-cli -- retrieve \
  --collection articles \
  "What evidence is relevant across the articles?"

cargo run -p verbatim-cli -- retrieve \
  --collection areskapitalon \
  "What does Areskapitalon cover?"

cargo run -p verbatim-cli -- ask \
  --collection articles \
  "What does the article set conclude?"
```

Ingest and retrieve a context pack without chat generation:

```sh
cargo run -p verbatim-cli -- ingest <source-id>
cargo run -p verbatim-cli -- retrieve \
  --source-id <source-id> \
  --page-size 1 \
  "What does Verbatim answer with?"
cargo run -p verbatim-cli -- retrieve \
  --source-id <source-id> \
  --format snippets \
  "What does Verbatim answer with?"
cargo run -p verbatim-cli -- retrieve \
  --source-id <source-id> \
  --format tsv \
  "What does Verbatim answer with?"
cargo run -p verbatim-cli -- retrieve \
  --source-id <source-id> \
  --format csv \
  "What does Verbatim answer with?"
```

Generate an answer with the configured chat model when you explicitly want
Verbatim to synthesize natural language:

```sh
cargo run -p verbatim-cli -- ask \
  --source-id <source-id> \
  --show-retrieval \
  "What does Verbatim answer with?"
```

Inspect one evidence id from ask citations or retrieve JSON/debug output:

```sh
cargo run -p verbatim-cli -- evidence <evidence-id>
```

Passing signal:

- `daemon status` reports `ok`.
- `source list` shows the added source.
- `ingest` reports one ingested source.
- `retrieve` returns compact Markdown with rank, score, citation, and snippet
  without calling the chat model.
- `retrieve --format snippets` returns citation plus snippet only.
- `retrieve --format tsv` and `retrieve --format csv` return fixed columns:
  rank, score, citation, collection, source, locator, and snippet.
- `retrieve --format json --show-locator` returns structured locator and
  provenance fields, including evidence ids for API callers.
- `ask` returns an answer with `[E...]` citations after invoking the configured
  chat model.
- `--show-retrieval` shows dense/BM25/RRF debug, graph hits when expansion adds
  results, reranker status, and the final evidence pack.
- `evidence <eid>` prints the source id, evidence kind, locator, position, and
  text.

## PDF Text And Image Evidence

Add and ingest PDFs the same way:

```sh
cargo run -p verbatim-cli -- source add /path/to/document.pdf
cargo run -p verbatim-cli -- ingest <source-id>
```

PDF text evidence is original source text and uses PDF page/paragraph locators.

PDF image artifact evidence is original extracted image metadata. Artifact files
are stored under the data directory and bounded by `[parser.image_artifacts]`
limits. `pdf_oxide` is the default parser. Unsupported image filters are skipped
with warnings; supported text evidence continues to ingest.

Generated image caption evidence is derived evidence. Enable it only when a
vision-capable OpenAI-compatible chat endpoint is available:

```toml
[vision]
enabled = true
provider = "openai_compatible"
base_url = "http://gb10:18009/v1"
model = "qwen3.6-27B"

[chat.vision_attachments]
enabled = true
model_supports_vision = true
max_images = 2
max_total_bytes = 8388608
detail = "auto"
```

Generated captions are searchable and cite their original image evidence through
`derived_from`. Treat them as model-derived descriptions, not exact OCR or
original PDF text.

## Rerank

Rerank is optional and disabled by default:

```toml
[rerank]
enabled = false
```

Enable it only when a reranker endpoint is available:

```toml
[rerank]
enabled = true
provider = "vllm"
base_url = "http://gb10:18003"
model = "Qwen/Qwen3-Reranker-4B"
top_n = 12
```

Use `verbatim retrieve --show-debug ...` or
`verbatim ask --show-retrieval ...` to confirm whether rerank was skipped,
disabled, or applied. Per-request retrieval flags override config defaults, so
`verbatim retrieve --fast`, `--no-rerank`, `--dense-top-k`, `--bm25-top-k`, and
`--rerank-top-n` can trade quality for latency without editing the config.

## Graph Retrieval

Bounded graph expansion is on by default:

```toml
[graph]
enabled = true
max_hops = 1
max_expanded_chunks = 30
max_neighbors_per_seed = 6
```

Disable it for baseline hybrid retrieval:

```toml
[graph]
enabled = false
```

LLM graph extraction is optional and config-gated:

```toml
[graph.extraction]
enabled = false
```

Full GraphRAG/global search is also optional and config-gated:

```toml
[graph.global_search]
enabled = false
```

Enable those only for manual runs with working chat/model endpoints. They are
MVP-capable but not required for the base smoke path or CI.

## Qdrant

Qdrant is an optional remote vector backend. It is disabled by default, and
local SQLite, HNSW, and Tantivy indexes remain authoritative.

```toml
[qdrant]
enabled = false
url = "http://rpi4b:6334"
collection = "verbatim"
prefer_for_search = false
timeout_seconds = 5
```

When `enabled = true`, ingest best-effort syncs chunk vectors to Qdrant with
`chunk_id`, `source_id`, `heading_path`, and `text_preview` payload fields.
Source removal deletes matching Qdrant points by `source_id`; force ingest
recreates the configured collection before uploading the current vector set.

Set `prefer_for_search = true` only when the Qdrant service is reachable and
you want dense retrieval to try Qdrant before local HNSW. If Qdrant is
unavailable, Verbatim logs a warning and falls back to local HNSW search.

## Troubleshooting

- `failed to read config`: run `verbatim config init` and then
  `verbatim config validate`.
- `daemon returned HTTP ...`: check the daemon terminal log and rerun
  `verbatim daemon status`.
- `connection refused`: start `verbatim-daemon` or verify `[daemon].bind`.
- Embedding/chat request errors: verify the endpoint URL includes `/v1` for
  OpenAI-compatible embedding/chat APIs and that the model name matches the
  server.
- `ask` has no citations: rerun with `--show-retrieval`, inspect the final
  evidence pack, and use `verbatim evidence <eid>` for the cited unit.
- Rerank is missing: check `[rerank].enabled` and the reranker `base_url`.
- Graph expansion is missing: check `[graph].enabled`, `max_expanded_chunks`,
  and whether the retrieved seed has graph neighbors.
- PDF image captions are missing: check `[vision].enabled`, the vision endpoint,
  and `[chat.vision_attachments]` if the model must receive image pixels during
  generation/verification.
- Qdrant is not used in MVP. Leave it disabled unless working on issue #17.

## Release Checklist

Before closing the MVP gate:

```sh
just build
cargo test -p verbatim-core --all-features mvp_regression
cargo test --workspace --all-features
just pre-commit-fast
just check-version-bumped
git diff --check main...HEAD
git diff --check
```

Manual model-backed validation:

- Fresh config can be created and validated.
- Daemon starts and health check passes.
- CLI can add, list, inspect, ingest, ask, and inspect evidence.
- PDF text evidence works.
- PDF image artifact evidence works.
- Generated PDF image caption evidence works when `[vision].enabled = true`.
- Local Qwen-compatible embedding/chat endpoints can be configured.
- Rerank can be enabled and disabled.
- Graph expansion can be enabled and disabled.
- LLM graph extraction and full GraphRAG/global search are documented as
  optional/config-gated MVP capabilities.
- Qdrant can be enabled as an optional backend, and local retrieval remains the
  default fallback.
