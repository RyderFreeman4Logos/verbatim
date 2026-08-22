---
name: verbatim-query-workflows
description: "Use when an agent must retrieve evidence, answer questions, inspect scores, debug ranking, compare results, or produce citation-grounded responses from Verbatim sources or collections."
version: 1.0.0
author: Verbatim contributors
license: MIT
metadata:
  hermes:
    tags: [verbatim, retrieval, rag, evidence, citations, scores]
    related_skills: [verbatim-agent-operator, verbatim-collection-builder, verbatim-bible-canonical-recall]
---

# Verbatim Query Workflows

## Overview

Use Verbatim as an evidence engine. The agent may reason over results, rewrite queries, and summarize, but the factual evidence must come from Verbatim output. Retrieval never invokes chat generation; `ask` may invoke chat unless run with context-only/no-generate options.

For agent answers, prefer retrieving evidence first, reading enough context, then writing a grounded answer with citations/locators. If results are weak, say so instead of forcing a confident answer.

## Choose the Right Retrieval Format

- Default markdown: compact rank, score, citation, stable evidence id, and snippet. Good for humans and reliability inspection.
- `--text-only` / `--format snippets`: cleanest stdout for agent context, but omits scores.
- `--format tsv` / `csv`: fixed columns including rank, score, citation, collection, source, locator, snippet.
- `--format json`: full structured fields, evidence ids, locators, provenance; best for programmatic merging/deduping.
- `--show-debug`: compact retrieval diagnostics to stderr; use when debugging ranking or candidate counts.
- `--show-debug --verbose`: full diagnostics; redirect to a file, do not paste into an LLM context.

## Default Retrieval

```bash
verbatim retrieve "<question>"
```

The daemon supplies `retrieval.default_collections`, `default_limit`, and
`default_page_size`. Use `--collection`, `--source-id`, `--limit`, or
`--page-size` only to override that context for a specific request.

For low-noise agent context:

```bash
verbatim retrieve "<question>" --format snippets
```

For programmatic confidence/ranking analysis:

```bash
verbatim retrieve "<question>" --format json > /tmp/verbatim-results.json
```

Completion criteria:
- Results are scoped to the intended collection/source.
- The agent records top locators/scores or evidence ids when relevant.
- The final answer only claims facts supported by retrieved text.

Canonical compact results select the matching in-chunk support unit by default.
Use `--passage` only when the full canonical chunk is needed.

## Scores and Confidence

Scores are ranking/model scores, not calibrated probabilities.

Use them as warning signals:
- Very low top score, small gap between top candidates, or unrelated top passages: answer with uncertainty.
- High score plus direct lexical/story match: stronger evidence.
- High score alone can still be wrong for cross-lingual or ambiguous queries.

When the user asks to inspect reliability, avoid `--text-only` and use default markdown, TSV, or JSON because `--text-only` omits scores:

```bash
verbatim retrieve "<question>"
verbatim retrieve "<question>" --format tsv
```

## Query Expansion Workflow

For vague, cross-lingual, or conceptual questions, do not trust a single query. Run multiple rewrites and merge by locator/source:

1. Original user wording.
2. Short keyword query.
3. Natural-language paraphrase.
4. English translation if the corpus is English.
5. Pronoun/voice normalized variants.
6. Known phrase variants if the user appears to quote from memory.

Use JSON for merging:

```bash
for q in \
  "<original>" \
  "<english translation>" \
  "<keyword phrase>"; do
  verbatim retrieve "$q" --collection <name> --format json --page-size 10 --limit 20 > "/tmp/v-$RANDOM.json"
done
```

Completion criteria:
- Duplicate locators are merged.
- The answer distinguishes “direct match” from “related but less direct”.
- If rewrites disagree, report uncertainty and show the best evidence.

## Debugging Bad Results

Use compact debug first:

```bash
verbatim retrieve "<question>" \
  --collection <name> \
  --show-debug \
  --format json \
  --page-size 5 \
  --limit 5 \
  >/tmp/retrieve.json \
  2>/tmp/retrieve-debug.json
```

Inspect:
- `bm25_hits`: lexical support. Zero BM25 hits means the result is purely semantic.
- `dense_hits`: vector candidate count.
- `rerank_input`: whether the right-looking candidate reached rerank.
- `final_evidence`: final scoring and evidence expansion.

If the likely answer is absent:
- Increase candidate pools (`--dense-top-k`, `--bm25-top-k`, `--rerank-top-n`).
- Try query rewrites or translations.
- Check collection freshness and whether the source is indexed.
- If a phrase is a translation/paraphrase, search the source language wording.

## Determinism Notes

Same query, same config, same index generally returns the same ranking. However:
- Raw JSON includes task ids and timings, so byte hashes differ.
- Reranker scores can drift slightly due remote model/GPU numerics.
- `--no-rerank` is more deterministic but may reduce quality.
- `--no-cache` intentionally changes embedding query text and can change results.

For reproducible comparisons, record:
- Query string.
- Collection/source filters.
- `page-size`, `limit`, top-k/rerank flags.
- Model/config profile.
- Whether `--no-cache` or `--no-rerank` was used.

## Answering with Evidence

When producing a user-facing answer:

1. Retrieve evidence.
2. Read enough context; use `--passage` only when the full canonical chunk is needed.
3. Cite locators/source names.
4. Separate direct evidence from interpretation.
5. State uncertainty if scores/results are weak.

Example final wording:

```text
最直接的证据是 [2 Timothy 4:1-13]，其中包含 “I have fought the good fight...” 和 “There is reserved for me the crown of righteousness...”。
相关但不完全同一主题的还有 ...
```

## Common Pitfalls

1. **Using `-s` for a collection.** `-s/--source-id` requires a source id. Use `--collection <name>` for collections.
2. **Treating top-1 as truth.** Always inspect whether the passage actually supports the claim.
3. **Ignoring stale collections.** `--collection` uses materialized membership; sync/ingest if freshness matters.
4. **Letting debug output flood context.** Redirect verbose debug to `/tmp` and summarize.
5. **Equating score with probability.** Scores are ranking signals, not truth confidence.
6. **Mixing stdout/stderr.** `--show-debug` writes diagnostics to stderr; stdout remains the retrieval result.

## Verification Checklist

- [ ] Configured scope is correct, or an explicit collection/source override was used.
- [ ] Freshness requirement resolved.
- [ ] Retrieval output includes enough context.
- [ ] Scores/debug checked when reliability is in question.
- [ ] Multiple query rewrites used for vague/cross-lingual questions.
- [ ] Final answer cites retrieved evidence and labels uncertainty.
