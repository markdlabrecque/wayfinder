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
  reindex error; pre-marker indexes without either changed analyzer path are adopted and marked.
  The marker is written before opening/creating Tantivy data, so a marker-write failure cannot
  leave a fresh versioned index looking legacy on retry.
- Reserved `wayfinder_text_en_v1` against custom `[[field_types]]` names, preventing an operator
  from replacing built-in `text_en`'s analyzer identity.
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
- Review-round targeted green: `cargo test --test schema_layer && cargo test --test edismax`
  passed 35 and 31 tests. The schema regressions cover `text_general`, non-English, and custom
  analyzed dynamic rules; raw static/dynamic legacy adoption; and rejection of a custom analyzer
  named `wayfinder_text_en_v1`.

## Full handoff gate

Initial gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
passed. After review-round fixes, the exact same full gate passed again: fmt clean, strict clippy
clean, and every suite passed with zero failures (including 43 unit, 31 edismax, 35 schema-layer,
and 27 differential tests).

## Review and CI

Review round 1 found the analyzed-dynamic migration gap, reserved-name hole, and provisioning
ordering risk; all were fixed and re-gated above. CI remains pending; no PR was opened or pushed.
