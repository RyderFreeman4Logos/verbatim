# Test fixtures

`canonical_bible.jsonl` is synthetic test data released under CC0-1.0. It is
not a Bible translation and is only used to exercise canonical JSONL parsing
and chunking without a licensed local corpus.

## Private full-corpus checks

Licensed full-corpus checks are opt-in and never add corpus bytes to this
repository. Set the local corpus path and its expected SHA-256, then run:

```sh
VERBATIM_PRIVATE_CANONICAL_CORPUS=/secure/path/corpus.jsonl \
VERBATIM_PRIVATE_CANONICAL_CORPUS_SHA256="$(sha256sum /secure/path/corpus.jsonl | cut -d ' ' -f1)" \
just test-private-canonical-corpus
```

With no readable corpus path, the recipe prints
`SKIPPED: private canonical corpus not configured` and exits successfully. A
missing, malformed, or mismatched digest refuses to run the private tests.
The private suite requires the pinned corpus to contain 31,102 records and
Genesis 1:1, John 3:16, and Revelation 22:21 canonical locators.
