# Daemon module split

Status: first walking-skeleton slice for
[#342](https://github.com/RyderFreeman4Logos/verbatim/issues/342).

## Problem

`crates/verbatim-daemon/src/main.rs` is a pre-existing capped monolith under the
immutable no-growth ratchet (`scripts/monolith/baseline.toml`, issue #368). It
owns startup, shared state, HTTP handlers, middleware wiring, and tests in one
file. Incremental extraction is required so later slices can move handlers and
runtime concerns without expanding the monolith.

## Goals

1. Split the daemon binary into deep modules with clear ownership.
2. Never grow `main.rs` lines or tokens relative to the trusted baseline.
3. Keep behavior identical across extractions (route inventory, auth, handlers).
4. Prefer interface-complete modules that can eventually stand alone.

## Non-goals (this slice)

- Moving handler bodies out of `main.rs`.
- Changing `AppState` / `SharedState` layout.
- Splitting auth middleware or deletion API further.
- Introducing a library crate for the daemon.

## Target shape

```text
crates/verbatim-daemon/src/
  main.rs              # process entry, AppState, remaining handlers
  routes.rs            # route inventory + router construction (#342 slice 1)
  auth_middleware.rs   # auth bind validation + request middleware
  deletion_api.rs      # deletion / reconcile HTTP surface
  sqlite_durability_ops.rs
  tests/               # large integration suites included from main
```

Later slices (not in this commit) may extract:

- handler groups (sources, collections, tasks, ask/retrieve, index ops)
- startup / lifecycle runtime
- readiness and idle reclaim coordination

Each slice must shrink `main.rs` and leave a focused inventory or contract test
in the new module.

## Slice 1 — route inventory (`routes.rs`)

What moved:

- `Router::new()...route(...).layer(...).with_state(...)` construction
- Auth middleware state capture used only for router layering
- A route registration count constant and inventory tests

What stayed in `main.rs`:

- All handler functions
- `daemon_router` as a thin facade calling `routes::build_router`
- `AppState` / `SharedState` definitions

Contract tests in `routes.rs`:

- `ROUTE_REGISTRATION_COUNT` equals the inventoried path template list
- Collection path templates match `CollectionApiEndpoint` in `verbatim-core`

Existing daemon router auth behavior remains covered by
`auth_middleware_daemon_tests.rs` via `daemon_router(...)`.

## Verification

```sh
just test-f route_inventory
just test-f daemon_router_enforces_loopback
JUST_NO_DOTENV=true just check-monolith staged
JUST_NO_DOTENV=true just pre-commit-fast staged
JUST_NO_DOTENV=true just pre-commit staged
```

`main.rs` must report fewer lines/tokens than its baseline cap after this slice.
