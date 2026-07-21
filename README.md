# Verbatim

Verbatim is a local, daemon-backed document retrieval tool for grounded answers
and citation-first context packs. It indexes your local sources, retrieves
ranked evidence with stable IDs and locators, and can optionally call a
configured chat model to write an answer over that evidence.

Use Verbatim when you need:

- local document search over PDFs, Markdown, text, or code;
- agent-friendly retrieval output with deterministic evidence IDs;
- generated answers that cite inspectable source evidence;
- a thin CLI that talks to one long-running daemon instead of rebuilding state
  per command.

The CLI help is the maintained command reference. Use this README for
orientation, then run `verbatim --help`, `verbatim <command> --help`, and deeper
subcommand help such as `verbatim source add --help` for exhaustive options.

## Quick Start

Build and install the release binaries:

```sh
just build
just install
```

Create and validate the local config:

```sh
verbatim config init
$EDITOR ~/.config/verbatim/config.toml
verbatim config validate
```

Configure OpenAI-compatible endpoints in the config for embedding and, if you
want generated answers, chat. Keep credentials in the config or your own secret
management; do not paste secrets into commands or logs. Runtime data lives under
`~/.local/share/verbatim/` by default.

For the SQLite durability profile, WAL checkpoint policy, disk-full behavior,
RPO contract, and backup/restore drill, see
[SQLite durability and recovery](docs/sqlite-durability.md).

Minimal model role configurations:

```toml
# BM25-only retrieval plus generated ask answers.
[embedding]
enabled = false

[chat]
enabled = true
base_url = "http://127.0.0.1:8000/v1"
model = "qwen3.6-27B"

[rerank]
enabled = false
```

```toml
# Dense retrieval plus chat, with no reranker calls.
[embedding]
enabled = true
base_url = "http://127.0.0.1:8002/v1"
model = "Qwen/Qwen3-Embedding-8B"

[chat]
enabled = true
base_url = "http://127.0.0.1:8000/v1"
model = "qwen3.6-27B"

[rerank]
enabled = false
```

```toml
# Local dense vector residency. low_memory is the default.
[vector_index]
residency = "low_memory" # or "resident_hnsw" for faster queries with higher RAM use
```

```toml
# Dedicated reranker endpoint. The explicit endpoint/model auto-enable rerank.
[rerank]
strategy = "endpoint"
base_url = "http://127.0.0.1:8003/v1"
model = "Qwen/Qwen3-Reranker-4B"
top_n = 12
```

```toml
# Explicit LLM rerank. Chat is not reused as rerank unless configured here.
[rerank]
enabled = true
strategy = "llm"
base_url = "http://127.0.0.1:8000/v1"
model = "qwen3.6-27B"
top_n = 12
```

If `[embedding]` is absent or `embedding.enabled = false`, Verbatim builds and
queries the lexical BM25 index without probing a default embedding endpoint. If
`embedding.enabled = true`, the configured embedding endpoint is required and
endpoint errors fail clearly. If `[rerank]` is absent, rerank stays disabled;
`rerank.enabled = false` always overrides rerank endpoint or model fields.
By default `[vector_index] residency = "low_memory"` keeps dense vectors in
SQLite and scans them at query time; set `resident_hnsw` to load the published
local HNSW index into the daemon for lower query latency on machines with enough
RAM.

Install and start the systemd user service:

```sh
verbatim daemon install
systemctl --user daemon-reload
systemctl --user enable --now verbatim
verbatim daemon status
```

After merging a binary change in a local clone, deploy the new user-daemon
binary explicitly:

```sh
just install-local-daemon
```

This installs release binaries to `~/.local/bin` by default, restarts the
`systemd --user` daemon, and verifies health. `VERBATIM_SYSTEMD_USER_SERVICE`
selects both the user unit name and the service restarted. The readiness check
waits up to 90 seconds by default so populated local indexes have time to open;
set `VERBATIM_LOCAL_DAEMON_READINESS_TIMEOUT_SECONDS=<seconds>` to override it
with a positive integer bound. On timeout, the recipe prints the selected
service unit plus recent `systemctl --user status` and `journalctl --user -u`
diagnostics before failing. Optional local hooks can be installed with
`just install-local-daemon-hook post-merge` or `post-push`; they are opt-in
because they restart the local daemon.

The default generated unit is written to
`~/.config/systemd/user/verbatim.service` unless `XDG_CONFIG_HOME` changes that
base path; custom service names use `<name>.service` under the same directory.
For foreground development, run `verbatim daemon start` instead.

Add a source and ingest it:

```sh
verbatim source add ./docs/report.pdf
verbatim source list
verbatim ingest <source-id>
```

Create a collection from filesystem roots or shell-selected files:

```sh
verbatim collection create articles
verbatim collection add-root articles ../drafts/articles/articles
verbatim collection sync articles

verbatim collection create areskapitalon
fd 'Areskapitalon.*\.md' ../drafts/articles/articles \
  | verbatim collection sync areskapitalon --stdin
```

Collection-scoped retrieval and ask use the daemon's materialized membership;
they do not rescan collection roots during query execution:

```sh
verbatim retrieve --collection articles "What evidence is relevant?"
verbatim retrieve --collection areskapitalon "What does Areskapitalon cover?"
verbatim ask --collection articles "What does the article set conclude?"
```

Ask with citations:

```sh
verbatim ask --source-id <source-id> "What does the report conclude?"
```

Retrieve context without chat generation and inspect one evidence item:

```sh
verbatim retrieve --source-id <source-id> \
  "What does the report conclude?"
verbatim retrieve --source-id <source-id> --format snippets \
  "What does the report conclude?"
verbatim retrieve --source-id <source-id> --format tsv \
  "What does the report conclude?"
verbatim retrieve --source-id <source-id> --format csv \
  "What does the report conclude?"
verbatim retrieve --source-id <source-id> --format json --show-debug \
  "What does the report conclude?"
verbatim evidence <evidence-id>
```

`--show-debug` keeps result stdout pipeable and writes compact retrieval
diagnostics to stderr; add `--verbose` for the full retrieval debug dump.

Queue long-running work as daemon tasks:

```sh
verbatim ingest --background <source-id>
verbatim task wait --timeout 25m <task-id>
verbatim task show <task-id>
verbatim task profile <task-id> --format json
verbatim task events <task-id>
verbatim task cancel <task-id>
```

`task wait` uses the CLI/config/default wait timeout. That timeout is separate
from model `timeout_seconds` settings used by embedding, rerank, chat, vision,
or OCR providers.

`task profile` reads persisted diagnostics for completed tasks with stored
profiles. It does not rerun retrieval, embedding, BM25/dense search, rerank,
chat/generation, citation verification, or evidence expansion; legacy/no-profile
and incomplete tasks report unavailable errors. The default output is
human-readable, and `--format json` is for tooling.

## Local PDF Example

```sh
verbatim config init
verbatim config validate
verbatim daemon install
systemctl --user daemon-reload
systemctl --user enable --now verbatim

verbatim source add ~/papers/example.pdf
verbatim ingest <source-id>

verbatim ask --source-id <source-id> \
  "What are the main claims, and where are they supported?"

verbatim retrieve --source-id <source-id> --format json --show-locator \
  "Which evidence supports the main claims?"

verbatim evidence <evidence-id>
```

PDF text-layer evidence uses page and paragraph locators. Scanned or image-only
PDFs require explicit OCR-backed indexing for deterministic text; vision
captions are model-derived descriptions, not OCR.

## Concepts

**Source**: A daemon-registered file or directory path. Sources are added once,
listed by stable source ID, and can be inspected, removed, or checked for stale
state.

**Collection**: A daemon-registered filesystem grouping. Collections can store
file, directory, and symlink roots; sync safely follows symlinks, materializes
membership in SQLite, and records both collection logical path and canonical
source path. Collection sync creates or reuses canonical-path source IDs, so one
physical file can belong to multiple collections without duplicate indexing.

**Evidence**: The citation unit returned by retrieval and answer generation.
Evidence has a stable ID, source ID, kind, locator, snippet/text, and optional
structured locator/provenance fields.

**Chunks and vectors**: Ingest parses source text into parent/child chunks,
stores metadata and text in SQLite, builds BM25 search indexes with Tantivy, and
builds dense vector indexes for retrieval. Retrieval fuses dense and BM25
results before returning a compact evidence pack.

New dense vector writes store compact little-endian BLOB payloads in SQLite and
leave the legacy `vector_json` payload empty. Read paths still prefer BLOB and
fall back to legacy JSON-only rows, so old databases remain readable. To inspect
old JSON copies without changing SQLite, run:

```sh
verbatim index vector-json-cleanup --dry-run
```

Dry-run reports `eligible`, `json_only`, `missing_blob`, and `malformed_blob`
for both `chunk_vectors` and `embedding_cache`. To clear only JSON payload
copies that already have a valid BLOB, stop write-heavy ingest/reindex work,
back up `~/.local/share/verbatim/verbatim.db` plus any `-wal`/`-shm` files, then
run:

```sh
verbatim index vector-json-cleanup --execute --confirm
```

Cleanup is transactional and skips JSON-only or malformed-BLOB rows. It does
not delete rows, and it does not clear legacy JSON-only payloads because those
may be the only readable vector copy. SQLite may keep freed pages inside the DB
file; reclaiming filesystem space requires a separate `VACUUM` or rebuild-table
maintenance step after an appropriate backup.

**Embedding profile**: A named embedding configuration. Normal parsing ingest
uses `[embedding].profile_id`; `--embedding-profile` is for rebuilding vectors
from existing chunks, normally with `--vectors-only`.

**Task**: A persistent daemon operation such as background ingest, reindex, or
ask. Use `task wait`, `task show`, `task events`, and `task cancel` to follow or
control queued work.

**Retrieval output**: `retrieve` and `ask --context-only` return evidence
without invoking chat generation. Default Markdown is compact: rank, score,
citation, and snippet. Use `--format snippets` or `--text-only` for citation
plus snippet only, and `--format tsv` or `--format csv` for fixed-column output.
Add `--show-debug` for compact deterministic ranking diagnostics, or use
`--show-debug --verbose`, `--show-locator`, or JSON format when you need
locators, provenance, internal evidence ids, and other debugging details.

## Command Reference

The CLI owns command documentation:

```sh
verbatim --help
verbatim source --help
verbatim ingest --help
verbatim retrieve --help
verbatim ask --help
verbatim task wait --help
```

The high-level command shape is:

```sh
verbatim source {add|list|inspect|remove|check}
verbatim collection {create|add-root|list|get|delete|sync|status}
verbatim ingest [source-id] [--force] [--background]
verbatim reindex {--source-id <id>|--all|--stale|--force|--vectors-only}
verbatim retrieve [options] "question"
verbatim ask [options] "question"
verbatim evidence <eid>
verbatim task {show|profile|events|wait|watch|cancel}
verbatim index {status|gc|delete-profile|vector-json-cleanup}
verbatim config {init|show|validate}
verbatim daemon {start|status|install}
```

See [docs/mvp.md](docs/mvp.md) for release gates, local model setup details,
manual smoke testing, PDF image notes, reranker and graph retrieval toggles,
optional Qdrant sync/search, and troubleshooting. See
[docs/evals.md](docs/evals.md) for deterministic regression fixtures.
