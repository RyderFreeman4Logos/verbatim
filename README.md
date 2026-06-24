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
selects both the user unit name and the service restarted. Optional local hooks
can be installed with `just install-local-daemon-hook post-merge` or
`post-push`; they are opt-in because they restart the local daemon.

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
verbatim retrieve --source-id <source-id> --show-debug \
  "What does the report conclude?"
verbatim evidence <evidence-id>
```

Queue long-running work as daemon tasks:

```sh
verbatim ingest --background <source-id>
verbatim task wait --timeout 25m <task-id>
verbatim task show <task-id>
verbatim task events <task-id>
verbatim task cancel <task-id>
```

`task wait` uses the CLI/config/default wait timeout. That timeout is separate
from model `timeout_seconds` settings used by embedding, rerank, chat, vision,
or OCR providers.

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

**Embedding profile**: A named embedding configuration. Normal parsing ingest
uses `[embedding].profile_id`; `--embedding-profile` is for rebuilding vectors
from existing chunks, normally with `--vectors-only`.

**Task**: A persistent daemon operation such as background ingest, reindex, or
ask. Use `task wait`, `task show`, `task events`, and `task cancel` to follow or
control queued work.

**Retrieval/debug output**: `retrieve` and `ask --context-only` return evidence
without invoking chat generation. Add `--show-debug`, `--show-locator`, or JSON
format when you need deterministic ranking, locator, and provenance details for
agent workflows or debugging.

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
verbatim task {show|events|wait|watch|cancel}
verbatim config {init|show|validate}
verbatim daemon {start|status|install}
```

See [docs/mvp.md](docs/mvp.md) for release gates, local model setup details,
manual smoke testing, PDF image notes, reranker and graph retrieval toggles,
optional Qdrant sync/search, and troubleshooting. See
[docs/evals.md](docs/evals.md) for deterministic regression fixtures.
