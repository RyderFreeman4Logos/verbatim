# Safety Model

## Scope

Verbatim ranks probabilistically. Its source-bounded retrieval does not
generate evidence. Every returned evidence passage is resolved from indexed,
traceable source material.

This model applies to `verbatim retrieve`, `verbatim ask --context-only`, and
`verbatim evidence <evidence-id>`. Free-form `ask` output and streaming output
are not covered unless citation verification passes. Check the original
locator before acting on a generated interpretation.

## Terms

- **Source-bounded retrieval** selects only material that Verbatim indexed from
  the selected sources.
- **Zero fabricated evidence** means Verbatim does not invent the returned
  evidence text. It does not mean the source is correct or complete.
- **Evidence selection** ranks and chooses existing evidence IDs.
- **Evidence rendering** resolves those IDs and publishes their source text.
- **Generated interpretation** is model-written prose about retrieved evidence.
- **Abstention** reports insufficient, unavailable, or unauthorized evidence
  instead of presenting a conclusion as supported.

## Planes

1. **Evidence plane**: indexed source text, stable IDs, and locators.
2. **Ranking plane**: lexical and embedding signals order candidate IDs.
3. **Decision plane**: filters, access rules, and abstention decide what to
   return.
4. **Optional interpretation plane**: a text model may explain the returned
   evidence after verification.

Only the Evidence plane's source-bounded renderer publishes evidence text. An
optional verifier or LLM may select or critique existing evidence IDs. It must
not author evidence.

## Limits and Error Types

Embedding and reranker scores are ranking signals. They are not calibrated
correctness probabilities. Determinism means reproducible ranking for the same
profile and inputs. It does not promise universal bitwise-identical results.

The safety model does not remove these error classes:

- **Relevance**: a ranked passage may not answer the question.
- **Attribution**: a passage or locator can be misunderstood or misapplied.
- **Ingest/OCR**: parsing can fail; scanned or image-only PDFs are rejected.
- **Lifecycle/applicability**: a source can be superseded, stale, or outside
  the decision context.
- **Authorization**: a user may not be allowed to see or use a source.
- **Completeness**: retrieval is best-effort, not an exhaustive search of all
  relevant documents.
- **Fabrication**: source-bounded rendering prevents fabricated evidence text;
  generated interpretation remains a separate risk unless verification passes.

`search` finds candidates. `retrieve` returns source-bounded evidence. A
locator identifies where a passage came from; it is not a substitute for
reading the surrounding source context.

PDF ingest uses native, deterministic text-layer parsing. Verbatim refuses OCR
output and vision captions as citable Evidence. It does not use them as
searchable Evidence. A born-digital PDF with a usable text layer can ingest;
a scanned or image-only PDF fails closed.

Version 1.0 supports Linux x86_64 hosts, Linux VMs, and Docker Linux. This does
not imply a native macOS or Windows port.

No GUI, TUI, or web help is shipped in Linux x86_64 1.0; CLI `--help` is the
user-facing path classification.

## Threat Examples

- **Fabrication — prompt injection in retrieved text:** a passage can contain
  instructions intended to steer a later generated interpretation. The
  source-bounded renderer returns the passage as source text; users must not
  treat instructions inside it as trusted directions.
- **Lifecycle/applicability — stale or superseded policy:** an older policy can
  remain indexed and rank highly. Mark or remove it from the active scope.
- **Ingest/OCR — OCR corruption:** Verbatim refuses OCR as Evidence. Treating
  external OCR output as citable would bypass that boundary.
- **Attribution — wrong PDF version:** a locator can identify a PDF passage but
  does not establish that the selected revision is the applicable one.
- **Authorization — ACL failure:** a missing or misapplied collection or source
  rule can expose evidence to a user who is not authorized to receive it.
- **Completeness — candidate-recall failure:** lexical or embedding candidates
  can miss relevant material. Retrieval is best-effort, not exhaustive.
- **Authorization — hosted reranker or embedding disclosure:** sending a query
  or context to a hosted endpoint can disclose it without authorization.

## Guarantees and Assumptions

- **Enforced guarantee — source-bounded evidence:** rendering resolves indexed
  evidence IDs to source text and does not author Evidence.
- **Enforced guarantee — OCR exclusion:** scanned and image-only PDFs fail
  closed; OCR output and vision captions are not citable Evidence.
- **Operational assumptions:** source selection and version status, ACL policy
  application, hosted-endpoint terms, and human review are managed correctly.
- **Not guarantees:** ranking quality, candidate recall, source correctness,
  and generated interpretation remain fallible.

## Guarantee-to-Test Map

| Documented claim | Coverage or status |
| --- | --- |
| Enforced guarantee — source-bounded evidence | `public_evidence_endpoint_revalidates_persisted_text` re-renders persisted text and verifies its hash. |
| Enforced guarantee — OCR exclusion | `scanned_image_only_pdf_ingest_rejects_without_ocr` and `scanned_image_only_pdf_rejects_before_configured_ocr` fail closed; `source_bounded_retrieval_omits_generated_captions_from_all_response_forms` excludes generated captions. |
| Source selection, ACLs, hosted endpoints, completeness, and human review | Operational assumptions; they are not runtime guarantees. |

## High-Risk Deployments

- Enforce collection and source access control before retrieval.
- Mark or remove superseded documents from the active decision scope.
- Keep abstention visible when no suitable evidence is available.
- Require human review for high-impact decisions and read the original locator.
- Review hosted-model privacy terms before sending queries or context to a
  remote endpoint.

## Copy Examples

| Safe | Unsafe |
| --- | --- |
| "These passages were retrieved from the selected sources." | "These passages prove the answer is correct." |
| "No supporting evidence was retrieved." | "No supporting document exists." |
| "The model summary cites retrieved evidence; verify the locator." | "This answer is hallucination-free." |
