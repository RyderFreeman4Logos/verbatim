# Verbatim

Grounded document Q&A with traceable citations. Verbatim runs as a local daemon
with a thin CLI that talks to the daemon over HTTP/SSE.

## Quick Start

```sh
just build
cargo run -p verbatim-cli -- config init
cargo run -p verbatim-daemon
```

In another terminal:

```sh
cargo run -p verbatim-cli -- daemon status
```

Install release binaries with:

```sh
just install
```

See [docs/mvp.md](docs/mvp.md) for the full MVP release gate, local model
configuration, daemon setup, manual smoke sequence, PDF image notes, reranker,
graph retrieval toggles, optional Qdrant sync/search, and troubleshooting. See
[docs/evals.md](docs/evals.md) for deterministic MVP regression fixtures.
