---
name: Feature or infrastructure change
about: Propose product work or generic infrastructure changes
title: ""
labels: []
---

## Summary

<!-- What problem are you solving? -->

## Proposed change

<!-- Behavior, APIs, or docs impact. -->

## Upstream-reuse check

When this issue implements or substantially expands **generic infrastructure**,
complete the checklist below (or paste from
`docs/templates/upstream-reuse-check.md`). Skip only for pure product-contract
work with no commodity subsystem touch.

- [ ] **Subsystem named**
- [ ] **External inventory** of maintained upstream candidates (or N/A + reason)
- [ ] **Disposition** = exactly one of ADOPT / WRAP / UPSTREAM / KEEP / DELETE
- [ ] **Matrix update** planned if disposition differs from
      `docs/architecture/upstream-substitution-matrix.toml`
- [ ] **KEEP evidence** (if KEEP): dated decision, rationale, owner,
      reconsider_when
- [ ] **Conformance plan** before DELETE/migration
- [ ] **SUPPLY-001** dependency recording path identified
- [ ] **Non-goals** respected (no outsourcing evidence/provenance/authz/publication truth)

Policy: `docs/architecture/upstream-first.md` (UPSTREAM-001 / #364).

## Acceptance criteria

- [ ]
