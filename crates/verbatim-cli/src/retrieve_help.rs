pub const RETRIEVE_AFTER_HELP: &str = r#"Examples:
  verbatim retrieve "What does the report conclude?"
  verbatim retrieve --format snippets "What supports it?"
  verbatim retrieve --text-only "What supports it?"
  verbatim retrieve --format tsv "What supports it?"
  verbatim retrieve --format csv "What supports it?"
  verbatim retrieve --source-id <source-id> --page-size 1 "What supports it?"
  verbatim retrieve --collection articles "What evidence is relevant?"
  verbatim retrieve --collection articles --collection areskapitalon "What changed?"
  verbatim retrieve --show-debug "What evidence is relevant?"
  verbatim retrieve --show-debug --verbose "What evidence is relevant?"
  verbatim retrieve --show-locator "What evidence is relevant?"
  verbatim retrieve --format json --show-debug "What evidence is relevant?"

Debugging:
  retrieve never invokes chat generation.
  It returns evidence context without invoking chat generation.
  Default retrieve output is source-bounded evidence; ranking is best-effort,
  not exhaustive.
  Default markdown is compact: rank, score, citation, stable evidence id, and snippet only.
  Without --source-id or --collection, retrieve uses the configured collection
  scope; an empty retrieval.default_collections intentionally searches all sources.
  Omitted pagination uses retrieval.default_limit and retrieval.default_page_size.
  Scores are ranked chunk-level scores; canonical multi-locator compact
  snippets show a chunk-internal support unit for the query.
  snippets/text-only omit headers and debug metadata; TSV/CSV emit fixed
  columns: rank, score, citation, collection, source, locator, snippet.
  --collection filters against materialized daemon membership and does not
  rescan collection roots during retrieve.
  --show-debug writes a compact JSON retrieval diagnostic summary with local
  stage spans to stderr.
  --show-debug --verbose writes the full task diagnostics, engine controls,
  timing, locators, internal evidence metadata, and deterministic
  dense/BM25/RRF/rerank ranking details and local stage spans to stderr.
  JSON output retains structured locator/provenance fields and full evidence
  identifiers for evidence lookups, but retrieval debug diagnostics stay on stderr.
"#;

pub(super) fn rerank_override(rerank: bool, no_rerank: bool) -> Option<bool> {
    match (rerank, no_rerank) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}
