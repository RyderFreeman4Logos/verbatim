# Benchmark governance (EVAL-015)

Status: walking skeleton for
[#340](https://github.com/RyderFreeman4Logos/verbatim/issues/340).
Machine-readable schema:
[`../evals/benchmark-manifest.schema.toml`](../evals/benchmark-manifest.schema.toml).
Examples: [`../evals/examples/`](../evals/examples/).
Validation: `bash scripts/tests/benchmark-governance-tests.sh`.

This document defines the **shape** of Verbatim benchmark promotion
manifests: split governance, statistical fields, contamination baselines, and
dataset-change review. It does **not** close the full EVAL-015 epic (harness
rewiring, model-backed CI, hidden-set operations, and live promotion pipelines
remain residual).

## Problem

A single scalar score can reward:

| Failure mode | How it appears | Why it is invalid |
| --- | --- | --- |
| Thread / near-duplicate leakage | Same conversation family or repost appears in both train and test | Overstates generalization; model or retriever memorizes group features |
| Temporal contamination | Future-dated material used while training or tuning on later knowledge | Scores mix memory of later publication with true retrieval skill |
| Single-score noise | One run, one metric, no effect size or confidence | Random variance looks like a regression or a win |
| Filename / generator gold | Labels inferred from paths or synthetic filenames without human or independent review | Training on the labeling channel, not the task |

Fresh technology content is valuable, but publication date alone does not prove
unseen data. Model-generated filenames are not labels.

## Manifest concepts

A **benchmark promotion manifest** is a TOML document that freezes how a named
benchmark may be split, scored, and promoted. Schema and required fields live
in
[`../evals/benchmark-manifest.schema.toml`](../evals/benchmark-manifest.schema.toml).

| Concept | Requirement |
| --- | --- |
| Immutable groups | Each thread, translation/repost/quote-derived, or near-duplicate family has one `group_id`. A `group_id` MUST NOT appear in more than one split role among train / validation / test / hidden_final. |
| Temporal cutoffs | Optional `temporal_holdout` declares a cutoff and optional rolling windows so holdout material is time-separated from tuning. |
| Split roles | Explicit `train`, `validation`, `test` (and optionally other named splits). Validation and test remain distinct from tuning when the policy requires it. |
| Hidden final set | `hidden_final_set` is declared **separately** from any tuning or calibration set. It is never used for hyperparameter search. |
| Contamination baselines | Manifests list which baselines must be reported (`no_context`, `oracle_context`, `retrieved_context`, `opaque_corpus`, …). Fresh-tech results sit **alongside** these baselines, not instead of them. |
| Statistical method | Promotion requires a declared `confidence_method` (e.g. `paired_bootstrap`), `effect_size_min`, and `regression_tolerance`. |
| Dataset / qrel change policy | Changes to datasets and qrels are reviewed like code (`dataset_change_policy.review_required = true` for this schema version). Silent rewrites of historical results are forbidden. |

Example (valid minimal):
[`../evals/examples/minimal-benchmark.example.toml`](../evals/examples/minimal-benchmark.example.toml).

Negative fixture (group leakage across train and test):
[`../evals/examples/group-leakage.fixture.toml`](../evals/examples/group-leakage.fixture.toml).

## Statistical requirements

Promotion reports MUST include, at minimum:

1. **Confidence method** — named and reproducible (for example paired bootstrap
   over query or group units). One method need not cover every metric forever;
   each promotion names the method it used.
2. **Effect size** — a minimum effect size threshold (`effect_size_min`) so
   tiny numerical differences are not treated as wins.
3. **Regression tolerance** — how much degradation is allowed before a change
   is blocked (`regression_tolerance`).
4. **Distributions for model-backed runs** — when model-backed workflows are
   repeated, report distribution summaries plus hard-invariant failures
   (residual for full harness work; the field surface is reserved on the
   schema).

Non-goal: requiring a single statistical test for every metric. Non-goal:
uncalibrated LLM judges as ground truth.

## Dataset and qrel change review

Dataset and qrel additions, removals, and edits are **audited like code**:

- Diffs go through the same review path as source changes.
- Historical promotion results MUST remain reproducible against the pinned
  dataset identity recorded in the manifest (or an immutable dataset digest
  when provided).
- Rewriting labels under an existing benchmark id without a new dataset
  revision is forbidden under `dataset_change_policy`.

This walking skeleton encodes the policy flag and required fields; enforcement
hooks in the full eval harness remain residual.

## Forbidden gold sources

Without `independent_validation = true` on the gold definition:

- Filenames
- Generator or synthetic metadata used as labels
- Path components treated as class or relevance gold

Independent validation means human review, an external authoritative label
source, or another process that does not share the generation channel.

## Operator checklist

### Verification

1. Author or update a promotion manifest under the schema.
2. Ensure every `group_id` is confined to a single split role.
3. Declare `hidden_final_set` separately from tuning/calibration.
4. Fill `confidence_method`, `effect_size_min`, and `regression_tolerance`.
5. List required `contamination_baselines`.
6. Set `dataset_change_policy.review_required = true` (schema default contract).
7. Validate offline:

   ```sh
   bash scripts/tests/benchmark-governance-tests.sh
   ```

### Promotion (policy shape)

1. Run the declared splits with the declared baselines.
2. Produce a report that includes confidence, effect size, and regression
   checks against the prior promoted baseline.
3. Do not promote from a run that used the hidden final set for tuning.
4. Record dataset/qrel revision identity with the promotion.

## How to run validation

```sh
bash scripts/tests/benchmark-governance-tests.sh
```

The justfile is agent-immutable; there is no `just` recipe for this skeleton.
Operators and agents must invoke the bash entry above. The script:

- accepts the committed valid minimal example
- rejects group leakage across splits
- rejects missing `confidence_method` or `effect_size_min`
- rejects `gold_from_filename` without `independent_validation = true`

No network access is required.

## Residual (explicitly deferred)

- Full harness rewiring for all EVAL-* suites
- Model-backed CI and repeated small-model workflow automation
- Operational hidden final set storage and access control
- Live paired-bootstrap execution inside the promotion CLI
- Manifest diff/approval automation beyond offline schema tests
- Closing epic #340 / EVAL-015

## Non-goals (from issue)

- Do not require one statistical test for every metric.
- Do not use uncalibrated LLM judges as truth.

## Related work

- Parent coordination: `ROADMAP-004` / issue #340
- Extends EVAL-001 through EVAL-014 (especially EVAL-011)
- Reviewed FEEDBACK-001 data may be promoted under these rules
- Fast deterministic fixtures: [`../evals.md`](../evals.md)
