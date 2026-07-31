# Issue #51: align `text_en` stopwords with Solr

## Goal

Remove the known edismax ranking and phrase-boost divergence by making Wayfinder's built-in `text_en` preset strip English stopwords before stemming, matching the captured Solr `text_en` behavior. Existing indexes built with the old analyzer must fail clearly and require reindexing rather than mixing old index-time tokens with new query-time analysis.

## Scope and contracts

- Existing committed edismax fixtures and findings 68-75 are the Solr ground truth; do not recapture or normalize them.
- Change only the built-in `text_en` preset and the internal dynamic-text catch-all that uses it. Custom analyzer chains remain operator-controlled and all other language presets retain their current behavior.
- Give the new analyzer a Wayfinder-owned, versioned tokenizer identity rather than overriding Tantivy's `en_stem`. The Tantivy schema and tokenizer registry must use the same identity.
- Persist an internal analyzer-contract marker beside newly created/opened indexes. If a pre-marker index can contain `text_en`-analyzed data (a static/dynamic `text_en` field or the dynamic-text catch-all), startup must fail with an explicit reindex message. Pre-marker indexes unaffected by `text_en` may be adopted safely.
- This is an analyzer semantic change, not a wire-format divergence waiver. Update the PRD and schema documentation to state that `text_en` is Solr-compatible while other language presets remain stem-only unless configured otherwise.
- Do not address #108-#114 or change score-magnitude divergence policy.

## Executable evidence

1. A schema-layer test first proves the old `text_en` output incorrectly retains `The`; after implementation it must produce only the Solr-compatible stemmed non-stopword tokens.
2. Startup tests first prove a legacy pre-marker index using `text_en` opens silently; after implementation it must refuse with a named reindex error. Add the complementary unaffected-index adoption test so the guard is no broader than necessary.
3. Replace #51's self-expiring edismax order guards with exact committed-fixture assertions and restore the `pf` equal-score assertion.
4. Remove the local #51 known-divergence manifest exceptions so all affected fixture rows compare normally.
5. Mutation-check both critical protections: temporarily omit the stopword filter and prove an analyzer/fixture test fails; temporarily bypass the legacy marker refusal and prove its startup test fails; then restore both.
6. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.

## Implementation steps

1. Add focused red tests for `text_en` tokenization, legacy-index refusal/adoption, exact edismax fixture order, and `pf` equality; confirm the failures are caused by retained stopwords and missing migration metadata.
2. Register a versioned Solr-compatible English analyzer (`SimpleTokenizer`, long-token removal, lowercase, English stopword removal, English stemming) and route all built-in `text_en` schema uses through it.
3. Add the analyzer-contract marker and narrowly gate pre-marker indexes that can contain old `text_en` analysis, with an explicit fresh-data-directory/reindex error.
4. Remove #51 divergence guards, update MLT comments or redundant assumptions without broadening behavior, and update `docs/PRD.md`, `docs/schema.md`, findings/report text where it describes `text_en` as stopword-free.
5. Perform mutation checks and full repository gates.
6. Record behavior, migration semantics, evidence, commands/results, review verdict, and follow-ups in `docs/reports/2026-07-30-51-text-en-stopwords.md`.
7. Commit conventionally, push `51-text-en-stopwords`, open a PR with `Closes #51` and the report link, wait for green CI, rebase if main advanced, rerun gates, and merge.

## Acceptance criteria

- `text_en` removes English stopwords and stems remaining tokens in the same order as captured Solr behavior.
- The four guarded edismax fixture rows and `pf` equality use normal fixture/property assertions and pass.
- Existing potentially affected pre-marker indexes refuse startup with a clear reindex error; unaffected indexes are not needlessly rejected.
- Custom analyzers and non-English presets are unchanged.
- Documentation no longer claims `text_en` retains stopwords.
- Mutation evidence and all standard gates pass, with independent reviewer approval.
