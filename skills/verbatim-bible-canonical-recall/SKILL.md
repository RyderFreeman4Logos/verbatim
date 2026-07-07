---
name: verbatim-bible-canonical-recall
description: "Use when an agent works with Bible or other canonical Verbatim sources: canonical JSONL, book/chapter/verse locators, passage retrieval, recall-style story/concept search, cross-lingual Bible queries, and citation-safe theological answers."
version: 1.0.0
author: Verbatim contributors
license: MIT
metadata:
  hermes:
    tags: [verbatim, bible, canonical, recall, passage, theology]
    related_skills: [verbatim-agent-operator, verbatim-query-workflows, verbatim-collection-builder]
---

# Verbatim Bible / Canonical Recall

## Overview

Canonical sources preserve traditional locators such as `John 3:16`, `Psalms 23:1-6`, or `2 Timothy 4:1-13`. For Bible work, Verbatim should return original indexed text and locators; the agent may explain, compare, or reason, but must not invent Bible text.

Use this skill both for exact citation lookup-style questions and for “recall mode” questions where the user only remembers a concept, story, image, or paraphrase.

## Known Collection Pattern

A common Bible collection is:

```text
collection: csb_bible
source id:  csb_bible-...
format:     canonical JSONL
```

Use collection filters, not source-id shorthand, unless the user explicitly gives the full source id:

```bash
verbatim retrieve "The Lord is my shepherd" \
  --collection csb_bible \
  --passage \
  --page-size 3 \
  --limit 10
```

Do **not** use `-s csb_bible`; `-s/--source-id` expects a full source id.

## Passage Retrieval

Prefer `--passage` for Bible/canonical queries. It groups retrieved evidence from the same canonical chunk before pagination, so `--page-size 1` means one passage block, not one verse.

Treat no-passage Bible recall as potentially misleading. Verified example:

```bash
verbatim retrieve "从此以后，有公义的冠冕为我留存" \
  --collection csb_bible \
  --page-size 10 \
  --limit 1
```

This ranks the correct chunk but renders `[2 Timothy 4:1]` with only verse 1 (`I solemnly charge you...`), while the remembered phrase is in 2 Timothy 4:8. This is tracked as product/UX bug #228: the displayed evidence row does not visibly support the query even though the chunk score is high. Adding `--passage` renders `[2 Timothy 4:1-13]` and includes the crown sentence. Until the CLI is fixed, agents should use `--passage` by default for canonical recall and inspect JSON/debug output before calling a no-passage top row unrelated.

```bash
verbatim retrieve "There is reserved for me the crown of righteousness" \
  --collection csb_bible \
  --passage \
  --page-size 1 \
  --limit 1
```

Expected shape:

```text
1. score=... [2 Timothy 4:1-13]
   ... I have fought the good fight ... There is reserved for me the crown of righteousness ...
```

For clean agent context without scores:

```bash
verbatim retrieve "There is reserved for me the crown of righteousness" \
  --collection csb_bible \
  --passage \
  --text-only \
  --page-size 1 \
  --limit 1
```

Use default markdown, TSV, or JSON when score visibility matters.

## Recall-Style Workflow

When the user does not know the locator or is unsure the passage exists:

1. Restate the remembered concept/story internally.
2. Generate multiple query variants.
3. Search the original wording and the likely source-language wording.
4. Merge duplicate locators.
5. Classify results as direct, related, or uncertain.
6. Answer only from retrieved passages.

Example user prompt:

```text
我记得有个人和神摔跤，然后名字被改了。
```

Run variants:

```bash
verbatim retrieve "一个人和神摔跤然后得到一个新名字" --collection csb_bible --passage --page-size 3 --limit 10
verbatim retrieve "a man wrestles with God and receives a new name" --collection csb_bible --passage --page-size 3 --limit 10
verbatim retrieve "Jacob wrestled with God and was named Israel" --collection csb_bible --passage --page-size 3 --limit 10
```

Then answer with evidence, e.g. `Genesis 32:20-31`, and mention related but less direct passages such as `Genesis 35:10-20` only if retrieved.

## Cross-Lingual Caveats

If the collection is English CSB and the user asks in Chinese:

- BM25 may be zero; retrieval depends on embedding/reranker semantics.
- Pronoun shifts matter. A devotional paraphrase like “为你留存” may not match an English first-person verse “reserved for me”.
- Run pronoun-normalized variants and English translations.
- Do not trust one query if the result seems wrong.

Example:

```bash
# May retrieve thematically related crown/righteousness passages but miss 2 Timothy 4.
verbatim retrieve "从此以后，有公义的冠冕为你留存" --collection csb_bible --passage

# Better for CSB because the verse is first-person.
verbatim retrieve "从此以后，有公义的冠冕为我留存" --collection csb_bible --passage
verbatim retrieve "There is reserved for me the crown of righteousness" --collection csb_bible --passage
verbatim retrieve "crown of righteousness reserved for me righteous Judge" --collection csb_bible --passage
```

## Concept / Theology Questions

For abstract theology or ethics questions, retrieve first, interpret second.

Example:

```bash
verbatim retrieve "the idea that forgiving someone should be repeated again and again, not counted" \
  --collection csb_bible \
  --passage \
  --page-size 5 \
  --limit 10
```

Then separate:

- Direct text: e.g. Matthew 18 forgiveness exchange and parable if retrieved.
- Related text: e.g. Luke 17 repeated forgiveness if retrieved.
- Later theological term: if the phrase is doctrinal vocabulary not directly in the Bible, say that and cite supporting texts only.

## Score Interpretation for Bible Queries

Scores help detect weak retrieval but are not calibrated truth probabilities.

Useful warning signs:
- Top score is low.
- Top results disagree across query variants.
- `bm25_hits=0` for cross-lingual queries.
- Top passage contains only one vague theme word but not the remembered story details.

Use debug when ranking is surprising:

```bash
verbatim retrieve "<query>" \
  --collection csb_bible \
  --passage \
  --show-debug \
  --format json \
  --page-size 5 \
  --limit 5 \
  >/tmp/bible-results.json \
  2>/tmp/bible-debug.json
```

Inspect `bm25_hits`, `dense_hits`, `rerank_input`, and final locators.

## Citation-Safe Answer Pattern

Use this answer structure:

1. “最可能是 …” with locator.
2. Quote or summarize only text present in the retrieved passage.
3. Explain why it matches the user's memory.
4. Add related candidates only under “相关但不完全同一处”.
5. If not found, say “没有找到直接对应；以下是主题相近结果”.

Do not produce unsourced verse text. If the user wants a Chinese wording but the indexed collection is English, say the current collection is English and provide the English evidence unless a Chinese collection was searched.

## Canonical JSONL / Bible Collection Creation

When building a Bible collection from canonical JSONL:

```bash
verbatim collection create csb_bible
verbatim collection add-root csb_bible <path-to-canonical-bible.jsonl>
verbatim collection sync csb_bible
TASK=$(verbatim ingest --background | grep -oE 'task-[A-Za-z0-9_:-]+' | head -n1)
verbatim task wait --timeout 25m "$TASK"
verbatim retrieve "The Lord is my shepherd" --collection csb_bible --passage --page-size 1 --limit 1
```

Completion criteria:
- Collection exists and has the canonical JSONL source as member.
- Ingest reaches terminal status.
- A known verse query returns a canonical locator and passage.

## Common Pitfalls

1. **Letting the LLM write Scripture.** Always quote from Verbatim output, not memory.
2. **Using one Chinese query against an English collection.** Run English/normalized variants.
3. **Missing `--passage`.** Verse-only snippets are often too narrow for pastoral/theological questions.
4. **Over-trusting score.** A high score can still be thematically wrong in cross-lingual retrieval.
5. **Forgetting that some terms are doctrinal, not direct biblical phrases.** Cite supporting texts, and label interpretation.
6. **Using source-id syntax for collections.** Use `--collection csb_bible`, not `-s csb_bible`.

## Verification Checklist

- [ ] Correct Bible/canonical collection selected.
- [ ] `--passage` used for context unless the user requested verse-only output.
- [ ] Multiple query variants used for recall/cross-lingual prompts.
- [ ] Locators deduped and classified as direct/related/uncertain.
- [ ] Scores/debug inspected when results are surprising.
- [ ] Final answer quotes or summarizes only retrieved text.
