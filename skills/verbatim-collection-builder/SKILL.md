---
name: verbatim-collection-builder
description: "Use when a user asks an agent to create, update, sync, ingest, watch, or troubleshoot Verbatim collections from files, directories, repositories, article trees, book folders, or other local corpora."
version: 1.0.0
author: Verbatim contributors
license: MIT
metadata:
  hermes:
    tags: [verbatim, collections, ingest, sources, watcher, rag]
    related_skills: [verbatim-agent-operator, verbatim-query-workflows]
---

# Verbatim Collection Builder

## Overview

Users often say “帮我把这些资料做成 collection” rather than running commands themselves. The agent should translate that intent into a complete Verbatim collection workflow: choose a stable name, register roots, avoid unsupported files, sync membership, queue ingest, wait for completion, enable watcher if appropriate, and verify retrieval.

A Verbatim collection is materialized membership. Retrieval with `--collection <name>` does **not** rescan directories. If files change, sync/watch/ingest must update the daemon catalog first.

## Inputs to Resolve

Resolve these before acting; ask only for what cannot be inferred from the environment:

- Collection name: stable, lowercase-ish identifier such as `articles`, `tech-books`, `csb_bible`.
- Root paths: files/directories/symlinks to include.
- Corpus shape: markdown-only, mixed repo, PDFs/books, canonical JSONL, articles.
- Watch behavior: one-shot collection or persistent watched collection with auto-index.
- Freshness expectation: can retrieval use stale results, or must it wait for ingest?

If the user gave an obvious directory/path, act on it. If they only gave a vague label (“我的讲章资料”), search likely project/drafts directories before asking.

## Standard Workflow

```bash
# For live operations, override any temporary smoke config left in the shell.
export VERBATIM_CONFIG="$HOME/.config/verbatim/config.toml"
verbatim daemon status

# 1. Create collection record. Repeat --ignore as needed.
verbatim collection create <name> \
  --ignore '.git/*' \
  --ignore '.claude/*' \
  --ignore 'node_modules/*' \
  --ignore 'target/*'

# 2. Add persistent roots.
verbatim collection add-root <name> <path>

# 3. Materialize membership.
verbatim collection sync <name>

# 4. Inspect result.
verbatim collection status <name>
verbatim collection get <name>

# 5. Queue ingest if new/stale sources were added.
TASK=$(verbatim ingest --background | grep -oE 'task-[A-Za-z0-9_:-]+' | head -n1)
verbatim task wait --timeout 25m "$TASK"
verbatim task show "$TASK"

# 6. Verify retrieval.
verbatim retrieve --collection <name> --require-fresh --page-size 3 --limit 3 "<smoke query>"
```

Completion criteria:
- `collection status/get` shows expected roots and member count.
- Ingest task reaches a terminal state.
- A retrieval scoped to the collection returns expected evidence or reports a clear, verified absence.

## Mixed Repository Ignore Patterns

For repositories or book/source trees with images, code, build outputs, and configs, start conservative. Verbatim has repeatable `--ignore`, not a `--include` flag.

```bash
verbatim collection create <name> \
  --ignore '.git/*' --ignore '.claude/*' --ignore 'node_modules/*' --ignore 'target/*' \
  --ignore '*.png' --ignore '*.jpg' --ignore '*.jpeg' --ignore '*.gif' --ignore '*.svg' \
  --ignore '*.webp' --ignore '*.ico' --ignore '*.woff*' --ignore '*.ttf' --ignore '*.eot' \
  --ignore '*.js' --ignore '*.css' --ignore '*.html' --ignore '*.wasm' --ignore '*.map' \
  --ignore '*.toml' --ignore '*.yml' --ignore '*.yaml' --ignore '*.json' --ignore '*.lock' \
  --ignore '*.dot' --ignore '*.py' --ignore '*.sh' --ignore '*.rs' --ignore '*.txt' \
  --ignore '.gitignore' --ignore 'LICENSE'
```

After sync, compare member count to the expected document count. If the collection should be markdown-only, use a file search/count outside Verbatim and make sure collection members are not polluted by code/binary assets.

## Watcher / Auto-Index Policy

Enable watcher only for persistent collections whose roots will change:

```bash
verbatim collection watch enable <name> --auto-index
verbatim collection watch status <name>
```

Use `--no-auto-index` or leave watcher disabled for large one-shot imports, experiments, or roots where automatic reindexing would surprise the user.

Completion criteria:
- Watch status matches the user’s expectation.
- For auto-index collections, a later file change should be handled by watcher maintenance; for one-shot collections, document that manual `collection sync` is required.

## Source-Specific Operations

For a single file/source, prefer source operations:

```bash
verbatim source add <file>
verbatim source list
verbatim source inspect <source-id>
verbatim ingest --background <source-id>
```

Use collection operations when the user wants a named corpus, multiple files, roots, or future collection-scoped retrieval.

## Handling Existing Collections

If `collection create` says the name exists:

1. Inspect it:
   ```bash
   verbatim collection get <name>
   verbatim collection status <name>
   ```
2. If the existing collection is the intended target, add roots/sync rather than deleting it.
3. If it is wrong, ask before deleting unless the user explicitly asked to recreate it.

## Pipeline Busy and Stale Cleanup

If source removal, sync, or ingest returns `pipeline_busy`:

```bash
verbatim task list --details
curl -fsS http://127.0.0.1:7700/api/health >/tmp/verbatim-health.json
```

Wait for active ingest tasks when possible. Do not restart the daemon unless the pipeline is stuck and health/task evidence supports it.

If a failed mixed-content sync created many stale unsupported sources, fix the ignore patterns first, then remove stale catalog entries or recreate the collection. Keep deletion predicates scoped to the affected roots; do not bulk-delete unrelated sources.

## Reporting to the User

Final reports should include:

- Collection name.
- Root paths added.
- Watch/auto-index state.
- Member count/status.
- Ingest task id and terminal status.
- Smoke query and first result or explanation of no result.
- Any unsupported/stale files and what was done about them.

## Common Pitfalls

1. **Ingesting the whole backlog accidentally.** `verbatim ingest --background` without a source id queues all pending/stale sources. Inspect status first if the daemon has unrelated stale work.
2. **Assuming `collection sync` indexes content.** Sync registers/materializes membership; ingest builds chunks/vectors.
3. **Missing extensionless files.** `.gitignore` and `LICENSE` can slip into mixed collections and fail ingest if not ignored.
4. **Using `-s <collection>` in retrieval.** `-s/--source-id` is a source id, not a collection. Use `--collection <name>`.
5. **Forgetting symlink/loop behavior.** Roots may be symlinks; inspect member counts and avoid unexpectedly huge traversals.
6. **Leaving users with commands.** If the user asked an agent to create the collection, the agent should create, sync, ingest, and verify it.

## Verification Checklist

- [ ] Daemon/config target confirmed.
- [ ] Collection name and roots are correct.
- [ ] Ignore patterns match corpus shape.
- [ ] `collection sync` completed.
- [ ] Member count is plausible.
- [ ] Ingest/reindex task terminal status checked.
- [ ] Watch state is deliberate.
- [ ] Collection-scoped retrieval smoke succeeded or failed with evidence.
