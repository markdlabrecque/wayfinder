# Report: Drupal `search_api_solr` schema preset

- Branch: `58-search-api-toml`
- Issue: [#58](https://github.com/markdlabrecque/wayfinder/issues/58) — ship a hand-authored
  Wayfinder schema preset for the Drupal `search_api_solr` module's field-naming convention, so a
  Drupal site can point at Wayfinder with zero per-site schema authoring.
- Pipeline: test-writer -> implementor -> reviewer (round 1: found a real premise error, bounced)
  -> corrective test-writer -> corrective implementor -> reviewer (round 2: **APPROVED**, no
  must-fix) -> reporter (this report).

## What was built

`presets/search-api.toml` (new top-level `presets/` directory — no existing precedent in this
repo for schema-TOML placement, so this is a reasonable default, not an established convention).
It encodes the module's prefix table as Wayfinder `[[fields]]` + `[[dynamic_fields]]` rules:

- Single/multi cardinality x string/fulltext/int/long/float/double/date/boolean
  (`ss_`/`sm_`, `ts_`/`tm_`, `is_`/`im_`, `its_`/`itm_`, `fs_`/`fm_`, `ps_`/`pm_`, `ds_`/`dm_`,
  `bs_`/`bm_`).
- The module's six special static fields: `id`, `index_id`, `hash`, `site`, `timestamp`,
  `boost_document`.
- English fulltext variants (`ts_X3b_en_`/`tm_X3b_en_` — search_api_solr encodes the
  language-code separator `;` as `X3b` in the field name) mapped to Wayfinder's `text_en`.
- `spellcheck_*` and `sort_*` variants.

Derived from issue #55's captured, ground-truth `solr-ref/search-api/configset/schema.xml`, and
cross-checked against the raw HTTP trace (`solr-ref/search-api/trace/*.json`, 28 real
Drupal<->Solr request/response pairs) wherever the schema.xml alone was ambiguous or misleading
(see the "mid-pipeline correction" section below for the case where this mattered).

`docs/schema.md` gained a short "Presets" section pointing at the new file.

## Divergences from captured Solr behaviour (documented in the preset's own header comment)

Per project convention these are preset-scoped implementation notes rather than general Solr
facts, so they are **not** new numbered entries in `docs/solr-ref-findings.md` — that file was
left untouched.

1. **No boolean type.** Wayfinder's `ResolvedType` enum (`src/schema.rs`) has only
   `Str`/`Text`/`I64`/`F64`/`Date` — no boolean. `bs_*`/`bm_*` map to Wayfinder's `string` type.
   This divergence is output-only: Drupal already sends booleans as JSON strings (`"true"`/
   `"false"`) on input (confirmed in `trace/00001.json`); only Wayfinder's *output* differs from
   Solr's native JSON boolean.
2. **`sort_*` -> `string`** as a stand-in for Solr's `collated_en`/`collated_und` field type —
   Wayfinder has no collation equivalent.
3. **`ts_*`/`tm_*` (Solr `text_und`) and `spellcheck_*` (Solr `text_spell_und`) -> `text_general`**,
   the closest available Wayfinder tokenizer preset.
4. **Scope is the traffic actually captured, not the full configset.** The preset covers the
   prefixes real captured Drupal traffic uses, not the module's exhaustive prefix list — spatial
   types (`points_`/`locs_`/`geos_`/`bboxs_`/`rpts_`), binary (`xs_`/`xm_`), `random_*`,
   `access_*`, and hierarchy (`hs_`/`hm_`/`hts_`/`htm_`) are out of scope, as they fall outside
   the module's core string/fulltext/int/long/float/double/date/boolean contract this issue
   scoped.

## Mid-pipeline correction (round-1 review)

The round-1 reviewer (Opus, independent) found that the original preset — and its locked-in test
assertions — had `stored`/`fast` **backwards** for almost every field. The root cause: the
captured `schema.xml`'s `stored="false"` attribute is misleading read in isolation. Solr 7+
defaults `useDocValuesAsStored=true`, so any field with `docValues="true"` comes back on a plain
retrieval query even when marked `stored="false"`, unless a field explicitly opts out — only
`sort_*` does, via `useDocValuesAsStored="false"` in `schema_extra_fields.xml`. This was caught by
cross-referencing the actual captured HTTP trace (`trace/00010.json`, a real `fl=*,score` query)
rather than trusting the schema.xml declaration alone — exactly the kind of "ticket premise
contradicted by the captured trace" case this project's conventions call out for correction, not
silent build-to-spec.

The same review pass also found all six static fields were missing `fast = true` despite
`docValues="true"` in the capture — needed for facet/sort, which (as of issue #66's
`resolved_fast` fix, already merged to `main` and rebased into this branch) works on dynamic
fields, so the static fields needed the same treatment for parity.

A corrective test-writer + implementor pass fixed both `stored`/`fast` values across the affected
fields and their test assertions. The round-2 reviewer independently re-derived the fix from the
trace/configset (rather than trusting the correction as given) and approved it with no further
must-fix items.

## Test evidence

New suite: `tests/search_api_preset.rs`, 32 tests — loads the real preset TOML, asserts field
resolution (type, `stored`, `fast`, `multi_valued`, `required`) for every static field and dynamic
prefix rule, plus at least one smoke-level end-to-end indexing/retrieval/facet/sort check per
field family.

Gates re-run by the reporter, independently, on the current branch state (not copied from the
implementor's or reviewer's earlier runs):

```
$ cargo fmt --check
(clean, no output)

$ cargo clippy --all-targets -- -D warnings
cargo clippy: No issues found

$ cargo test
cargo test: 485 passed (22 suites, 26.13s)

$ cargo test --test search_api_preset
cargo test: 32 passed (1 suite, 1.71s)
```

All green.

## Pipeline stages

- **test-writer** wrote `tests/search_api_preset.rs` first, confirmed red for the right reason
  (preset file did not exist yet) before any production/preset content existed.
- **implementor** built `presets/search-api.toml` until the suite went green. Hard gate confirmed
  before handoff: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean,
  `cargo test` green.
- **reviewer round 1**: bounced with a real, not cosmetic, defect — the `stored`/`fast` inversion
  described above, plus the missing `fast = true` on static fields. Not a must-fix nit; a
  correctness bug that would have shipped a preset returning wrong retrieval/facet/sort behaviour
  for nearly every field.
- **corrective test-writer + implementor**: fixed the assertions and the preset values, re-ran the
  full gate green.
- **reviewer round 2**: **APPROVED**, no must-fix items. Independently re-derived the fix from
  `trace/00010.json` and the configset rather than trusting the correction as reported — this
  round did not hit the 2-round cap; it resolved cleanly within it.

## Open follow-ups (reviewer's, non-blocking, deferred — not resolved by this change)

1. **`is_*`/`im_*` labelled `int`, captured Solr type is `plong`.** Harmless in practice — Wayfinder's
   `resolve_type` collapses `"int"`/`"long"` to the same `I64` internally — but the TOML's `type =
   "int"` label is cosmetic drift from what the capture actually shows. Worth a follow-up cleanup
   for label accuracy, no behavioural fix needed.
2. **`fast_fields_are_sortable` is a single-document smoke test.** It asserts a 200 response, not
   real multi-document ordering — the weakest assertion in the suite, and honestly labelled as a
   smoke test in its own doc comment rather than oversold as a real ordering check. A follow-up
   could strengthen it to a genuine multi-doc sort-order assertion.
3. **`sm_context_tags` is technically a static field in Solr's captured schema, not dynamic.** The
   preset's `sm_*` dynamic rule already covers it correctly, since Wayfinder's dynamic matching
   applies regardless of whether Solr modelled the equivalent field as static or dynamic — noted
   as the one field where the preset's declared flags are a strict superset of Solr's own. Harmless,
   but worth knowing if a future Solr capture disagrees.
4. **No prior README/docs pointer to preset files existed before this PR** — now fixed by the
   `docs/schema.md` "Presets" section added in this change; noted here only because it was called
   out as a gap during review before the fix landed in the same PR.

None of these are blocking; all are explicitly deferred, not silently dropped. The pipeline
resolved within the reviewer's 2-round cap (round 1 bounce, round 2 approved), so this work does
**not** need to record "could use more review passes" per the pipeline's own rule for a capped-out
review — the cap was not exhausted here.

## Pointers

- Preset: `presets/search-api.toml`.
- Tests: `tests/search_api_preset.rs` (32 tests).
- Docs: `docs/schema.md` ("Presets" section).
- Source material: `solr-ref/search-api/configset/schema.xml`,
  `solr-ref/search-api/trace/*.json` (from issue #55).
- Issue: [#58](https://github.com/markdlabrecque/wayfinder/issues/58).
