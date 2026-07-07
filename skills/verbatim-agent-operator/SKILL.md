---
name: verbatim-agent-operator
description: "Use when operating Verbatim through an AI agent: daemon health, config selection, source/catalog tasks, long task handling, installed-binary smoke tests, and choosing the right Verbatim project skill before acting."
version: 1.0.0
author: Verbatim contributors
license: MIT
metadata:
  hermes:
    tags: [verbatim, rag, daemon, cli, agent-ops]
    related_skills: [verbatim-collection-builder, verbatim-query-workflows, verbatim-bible-canonical-recall]
---

# Verbatim Agent Operator

## Overview

Use this as the entry point when a user asks an agent to operate Verbatim instead of giving exact commands. Verbatim is a daemon-backed document/canonical RAG system: the CLI talks to a local daemon, persistent tasks do ingest/reindex/ask work, and retrieval must be grounded in indexed evidence.

Agents should do the work end-to-end: inspect the daemon, choose or create collections, queue tasks, wait for terminal status, verify retrieval, and report concrete command output. Do not hand the user a list of commands unless they explicitly ask to run them themselves.

## Route to a More Specific Skill

- Collection/source creation, roots, ignore patterns, watcher/auto-index: load `verbatim-collection-builder`.
- Retrieval, evidence-backed answers, score interpretation, query debugging: load `verbatim-query-workflows`.
- Bible/canonical JSONL, passage retrieval, verse locators, vague story/concept recall: load `verbatim-bible-canonical-recall`.

## Baseline Checks

Always establish the live target before operating:

```bash
# For live operations, override any temporary smoke config left in the shell.
export VERBATIM_CONFIG="$HOME/.config/verbatim/config.toml"

verbatim --version
verbatim-daemon --version || true
verbatim config validate
verbatim daemon status
curl -fsS http://127.0.0.1:7700/api/health >/tmp/verbatim-health.json
```

Completion criteria:
- The CLI version is visible.
- Config validates.
- The daemon status/health is `ok`, or the failure mode is known and being fixed.
- You know whether you are using the live config (`~/.config/verbatim/config.toml`) or an explicit temporary smoke config.

## Config and Daemon Discipline

1. **Use explicit config when switching contexts.** If a previous smoke test used `/tmp/.../config.toml`, set `VERBATIM_CONFIG=$HOME/.config/verbatim/config.toml` for live operations. Do not rely on `${VERBATIM_CONFIG:-...}` because it preserves stale temporary values.
2. **Do not confuse daemon ports.** The standard live daemon is usually `127.0.0.1:7700`; temporary smoke daemons often use another port.
3. **Startup can be slow.** After `systemctl --user restart verbatim.service`, wait for `/api/health` to answer; large catalogs may need 2-3 minutes before the port listens.
4. **Do not restart as a first reaction.** For slow ingest/retrieval, inspect task status/events and health resource pools before restarting.

## Persistent Task Pattern

For long-running work, use daemon background tasks and wait by task id:

```bash
TASK=$(verbatim ingest --background | grep -oE 'task-[A-Za-z0-9_:-]+' | head -n1)
echo "task=$TASK"
verbatim task wait --timeout 25m "$TASK"
verbatim task show "$TASK"
verbatim task events "$TASK" | tail -50
```

Completion criteria:
- The task reaches `succeeded`, `failed`, or `cancelled`.
- If failed, the report includes the task id and the relevant error lines.
- If succeeded, at least one retrieval or inspection command verifies the indexed result.

## Installed-Binary Smoke After Changes

After merging a Verbatim code change, the agent is responsible for installing and verifying it:

```bash
cargo build --release -p verbatim-cli -p verbatim-daemon
install -m 755 target/release/verbatim "$HOME/.local/bin/verbatim"
install -m 755 target/release/verbatim-daemon "$HOME/.local/bin/verbatim-daemon"
systemctl --user restart verbatim.service
# wait for health before testing
```

Completion criteria:
- `~/.local/bin/verbatim --version` runs.
- `systemctl --user is-active verbatim.service` is `active`.
- `/api/health` returns `ok`.
- A smoke retrieval against a known collection succeeds.

## Common Pitfalls

1. **Reporting planned commands instead of operating.** If the user asked the agent to make a collection or answer from a collection, execute the workflow and verify it.
2. **Using the wrong config after a smoke test.** Always print or set `VERBATIM_CONFIG` before live smoke.
3. **Dropping task ids.** Capture the full `task-...` string and include it in the report.
4. **Treating `pipeline_busy` as fatal.** It often means another ingest is active; wait or inspect tasks before retrying.
5. **Assuming retrieval rescans roots.** `--collection` uses materialized membership. Run `collection sync` when files changed.
6. **Leaking secrets.** Config output should be redacted; never paste API keys or bearer tokens.

## Verification Checklist

- [ ] Correct config/daemon target confirmed.
- [ ] Health/status checked before operations.
- [ ] Long tasks have terminal task records.
- [ ] User-facing claims are backed by command output.
- [ ] For code changes, release binary installed and daemon restarted.
- [ ] Final answer names collections, source ids or task ids when relevant.
