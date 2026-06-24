# CLI/API Parity

Verbatim's daemon API is the reusable product surface. The CLI is a thin
client for daemon-capable operations; future GUI code should send the same
request/response payloads from `verbatim_core::api` instead of reimplementing
CLI-only filesystem or collection logic.

## Collection Guardrail

Collection workflows are daemon-authoritative. The CLI may normalize explicit
user input, such as turning a relative path into an absolute path, but it must
not discover roots, diff membership, watch directories, or mutate collection
storage locally. Those behaviors live in the daemon/core path behind
`/api/collections...`.

The mechanical guardrail is `COLLECTION_CLI_API_PARITY` in
`verbatim_core::api`. It maps every canonical `verbatim collection ...` leaf
command to a daemon endpoint and is checked against clap command metadata in
CLI tests. `HttpDaemonClient` and daemon route registration also use
`CollectionApiEndpoint`, so collection route changes have one shared source of
truth.

## Inventory

| CLI surface | Classification | API surface | Notes |
| --- | --- | --- | --- |
| `source add/list/inspect/remove/check` | daemon-backed | `/api/sources...` | Source catalog is daemon state. |
| `collection create/add-root/list/get/delete/sync/status` | daemon-backed | `/api/collections...` | Covered by `COLLECTION_CLI_API_PARITY`. |
| `collection watch enable/disable/status` | daemon-backed | `/api/collections.../watcher`, `/api/collections/watchers/status` | Watch execution remains daemon-side. |
| `ingest`, `reindex` | daemon-backed | `/api/ingest...`, `/api/reindex`, `/api/tasks/...` | Background mode queues daemon tasks. |
| `ask`, `retrieve` | daemon-backed | `/api/ask/stream`, `/api/retrieve`, `/api/tasks/ask` | `--collection` becomes `CollectionFilterRequest`. |
| `evidence` | daemon-backed | `/api/evidence/{eid}` | Evidence IDs are daemon-issued. |
| `task show/events/wait/watch/cancel/resume` | daemon-backed | `/api/tasks...` | Wait/watch stream daemon SSE events. |
| `config show` | daemon-backed | `/api/config` | Daemon returns redacted runtime config. |
| `config init`, `config validate` | local-only by necessity | none | These create or validate the local config file before daemon use. |
| `daemon status` | daemon-backed | `/api/health` | Health probe only. |
| `daemon start`, `daemon install` | local-only by necessity | none | These manage the local foreground process or user service unit. |

No collection-era CLI command is missing a daemon API as of issue #113, so no
follow-up issue was filed. If a future collection command cannot be
daemon-backed, file a focused issue before merging the CLI surface and document
the blocker in this inventory.

## GUI Path

A GUI should depend on the daemon API, not on CLI local modules. Collection UI
flows should use:

- `CreateCollectionRequest` for collection records.
- `AddCollectionRootRequest` for user-selected roots.
- `CollectionSyncRequest` for explicit sync inputs.
- `CollectionFilterRequest` on retrieve/ask requests for scoped evidence.
- Watcher endpoints for daemon-side refresh and optional auto-index settings.

This keeps collection membership, watcher behavior, indexing, and provenance in
one daemon/core implementation and avoids a separate GUI file manager.
