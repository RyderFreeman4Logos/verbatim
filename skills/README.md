# Verbatim Agent Skills

This directory contains repo-local skills for AI agents operating Verbatim. Load the relevant `SKILL.md` before acting:

- `verbatim-agent-operator/` — daemon/config/task/install discipline and routing.
- `verbatim-collection-builder/` — create, sync, ingest, watch, and troubleshoot collections from user-described corpora.
- `verbatim-query-workflows/` — retrieve, inspect scores, debug ranking, and answer with evidence.
- `verbatim-bible-canonical-recall/` — Bible/canonical sources, passage retrieval, and recall-style concept/story search.

These skills are intentionally project-local. They teach agents how to operate this repository and local Verbatim daemon without adding LLM orchestration into Verbatim core.
