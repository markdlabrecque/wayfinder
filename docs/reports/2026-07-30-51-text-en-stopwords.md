# Report: Solr-compatible `text_en` stopwords (issue #51)

## Scope

Built-in `text_en` now removes English stopwords before stemming, matching the captured Solr
analyzer order. Custom chains and non-English presets are unchanged.

## Implementation

- Registered `wayfinder_text_en_v1` as a Wayfinder-owned analyzer: simple tokenizer,
  40-character token limit, lowercase, English stopwords, English stemmer. Tantivy's `en_stem`
  remains untouched.
- Routed both static `text_en` fields and `_dynamic_text` through that identity, including the
  fast-field tokenizer manager required by Tantivy JSON fields.
- Added `wayfinder-analyzer-contract`. A pre-marker index with static built-in `text_en`, or any
  analyzed dynamic rule sharing `_dynamic_text`, refuses startup with a fresh-data-directory
  reindex error. A raw-only legacy dynamic schema remains adoptable but receives a distinct
  legacy-dynamic state, rather than full v1 certification; a later compatible rule edit that
  starts using its old `en_stem` catch-all therefore requires reindexing. The marker is written
  before opening/creating Tantivy data, so a marker-write failure cannot leave a fresh versioned
  index looking legacy on retry.
- Reserved both `text_en` and `wayfinder_text_en_v1` against custom `[[field_types]]` names,
  preventing operators from shadowing the built-in preset or replacing its analyzer identity.
- Removed the now-obsolete `hl_fragsize_small_truncated` analyzer waiver so it uses the ordinary
  differential assertion.

## Fixture correction

The initial implementation exposed an inconsistent one-off `edismax_term_boost` fixture. On
2026-07-30, a clean isolated local `solr:9` container was created with the exact 10-document
edismax schema/corpus; baseline and `rocket^5` were requested from that same container. Temporary
captures were written outside the repository at
`/tmp/wayfinder-51-edismax.jWlR1N/`.

| Request | Order and scores |
|---|---|
| `q=rocket` | `eC` 0.871532, `eD` 0.72299594, `eB` 0.71525735, `eA` 0.5274755 |
| `q=rocket^5` | `eC` 4.3576603, `eD` 3.6149795, `eB` 3.5762868, `eA` 2.6373773 |

The term-boost response is exactly five times the same-container baseline, so only
`solr-ref/responses/edismax_term_boost.json` was corrected. Finding 82 and the capture comment
now record that provenance. No status sidecar exists.

## Orchestrator scope amendment and fixture provenance

The approved plan remains unchanged at `f4922160a8fd`. During implementation, the reviewer
identified that #109's one-off term-boost fixture contradicted both its paired committed baseline
and its own stated five-times relationship. The Orchestrator authorized this narrow correction so
#51 could retain normal fixture comparison rather than carrying a false failure or waiver.

The verification used a clean local `solr:9` container with a randomly published loopback port,
then added `title`/`body` as `text_en` and indexed exactly the schema/corpus in
`solr-ref/capture.sh`'s edismax block. From the same container/core, the commands were:

```sh
curl -fsSG "$BASE/select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json"
curl -fsSG "$BASE/select?q=rocket^5&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json"
```

The evidence is the order/score table above: every boosted score is exactly five times its
same-container baseline. Temporary JSON outputs remain at
`/tmp/wayfinder-51-edismax.jWlR1N/`; only `solr-ref/responses/edismax_term_boost.json` was
replaced. `capture.sh` and finding 82 retain the same reproducible provenance.

## Test evidence

- Initial red: `cargo test --test schema_layer --test edismax` failed for retained `the`, legacy
  startup acceptance, four guarded fixture orders, and `pf` equality.
- Targeted green: `cargo test --test schema_layer && cargo test --test edismax && cargo test --test differential`
  passed: 33, 31, and 27 tests respectively.
- Mutation: omitted `StopWordFilter`; `text_presets_tokenize_as_expected` failed with actual
  `['the', 'quick', 'runner']` versus expected `['quick', 'runner']`; restored.
- Mutation: bypassed the pre-marker refusal; `pre_analyzer_contract_text_en_index_refuses_startup_requiring_reindex`
  failed because startup returned an app; restored.
- Review-round targeted green: the two focused regressions
  `legacy_dynamic_text_identity_cannot_be_adopted_then_reused_for_analyzed_rules` and
  `custom_field_type_cannot_shadow_the_builtin_text_en_preset` passed; then
  `cargo test --test schema_layer` passed all 37 tests. The schema regressions cover
  `text_general`, non-English, and custom analyzed dynamic rules; raw static/dynamic legacy
  adoption; and rejection of custom analyzers named `text_en` or `wayfinder_text_en_v1`.

## Full handoff gate

Initial gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
passed. The final review-round full gate is blocked as recorded below after the narrowed
marker-state and built-in-name-reservation fixes.

## Review follow-up evidence

- `cargo test --test schema_layer legacy_dynamic_text_identity_cannot_be_adopted_then_reused_for_analyzed_rules`
  passed (1 test), and
  `cargo test --test schema_layer custom_field_type_cannot_shadow_the_builtin_text_en_preset`
  passed (1 test).
- `cargo test --test schema_layer` passed all 37 tests.
- The required full command, `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`,
  is currently blocked by `clippy::redundant_closure_call` in the checkpointed test at
  `tests/schema_layer.rs:335-338`; no production change can affect that lint and no test was edited.

## Review and CI

Review round 1 found the analyzed-dynamic migration gap, built-in-name shadowing hole, and
provisioning ordering risk. The narrowed follow-up persists a legacy-dynamic marker state and
reserves the built-in name; final gate evidence is recorded below. CI remains pending; no PR was
opened or pushed.
