# #397 — suggest language dictionaries

**Date:** 2026-08-06. **Issue:** #397. **Status:** complete, independently reviewed and approved.

## Approved spec / decision

Shipped Search API language codes without Tantivy stemmers receive distinct, unstemmed suggest analyzers. Their requested language code remains the `suggest.dictionary` response key; they are not rejected and do not silently use `und`. The frozen unknown dictionary `xx` remains a 400.

This is recorded as PRD §2 divergence 14. No frozen fixture changed and no new Solr capture was made.

## TDD and implementation

The public `/suggest` test `suggest_q_non_stemming_drupal_language_is_served_with_results` was red before implementation: `suggest.dictionary=zh-hans&suggest.q=qui` returned the expected rejection, HTTP 400, rather than a `zh-hans` result. It passed after the unstemmed dictionary registration was added. The existing public `suggest_q_dict_unknown_matches_fixture` regression continues to require `suggest.dictionary=xx` to return HTTP 400.

- `src/schema.rs` adds the shipped Search API codes that lack Tantivy stemmers, registers a separate unstemmed suggest chain for each, and admits them only as configured suggesters.
- `src/lib.rs` retains validation before lookup, so configured unstemmed codes are served while `xx` remains rejected.
- `tests/suggest.rs` adds the public `zh-hans` request test, asserting its own response key and three prefix results.
- `docs/PRD.md` records divergence 14.

`src/schema.rs` also contains the authoritative architecture guard: it derives language codes from the vendored Search API field-type YAML files and asserts every shipped code is classified exactly once as Tantivy-stemmed or unstemmed. This prevents the hand-maintained unstemmed registration from drifting from the YAML source.

## Mutation and review evidence

Mutation testing deliberately removed admission of the unstemmed language set; the public `zh-hans` test failed, and the change was restored. The architecture drift guard was also requested and added after review.

Review round 1 found that numbered historical finding 195 had been rewritten; its original body was restored. It also requested the YAML-derived drift guard. Round 2 approved with no findings. No review cap was reached.

## Final gates

```text
cargo fmt --check                                  # pass
cargo clippy --all-targets -- -D warnings          # pass
cargo test                                         # pass; one pre-existing #362 measurement ignored
```

No accepted deviations, deferred follow-ups, or unresolved risks.
