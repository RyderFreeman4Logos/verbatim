# Upstream-reuse check (for new issues)

Copy this section into any issue or PR that implements or substantially expands
generic infrastructure. Policy: [`docs/architecture/upstream-first.md`](../architecture/upstream-first.md).
Matrix: [`docs/architecture/upstream-substitution-matrix.toml`](../architecture/upstream-substitution-matrix.toml).

## Upstream-reuse check

- [ ] **Subsystem named** — Which generic subsystem is affected (workflow, model
  clients, retry/cancel, session/checkpoint, auth, telemetry, eval, RAG, vectors,
  parsers, caches, migrations, backup/durability, HTTP, CLI, other)?
- [ ] **External inventory** — Listed actively maintained upstream crates/projects
  considered (or N/A with reason if product-contract only).
- [ ] **Disposition** — Exactly one of `ADOPT` / `WRAP` / `UPSTREAM` / `KEEP` /
  `DELETE`, consistent with the substitution matrix (update the matrix in the
  same change when disposition changes).
- [ ] **KEEP evidence** — If `KEEP`: dated decision, rationale/evidence, owner,
  and reconsideration trigger are recorded in the matrix.
- [ ] **Conformance plan** — For migrations/deletions: same fixtures will compare
  old/local vs upstream-backed behavior before removal.
- [ ] **SUPPLY-001** — Dependency features, license, MSRV, and upgrade/advisory
  evidence will be recorded under supply-chain process (or already covered by
  lockfile + `cargo deny`).
- [ ] **Non-goals respected** — Does not outsource evidence/provenance/
  completeness/authorization/publication truth; does not adopt abandoned shallow
  wrappers; does not fork without exhaustion of contribute/adapt paths.

## Notes

_Optional free-form notes, candidate links, or link to matrix row `id`._
