# #308 — `terms.prefix` / `terms.limit` (and `json.nl` shapes)

**Date:** 2026-08-03. **Branch:** `markdlabrecque/issue-308-accept-terms.prefix-terms.limit`.
**Spec:** findings **141** and **142** in `docs/solr-ref-findings.md` (the issue body points
at these as the specification). Wave 1, branch B of the `#289-#302` parity batch.

Stock `search_api_solr` autocomplete (`setAutocompleteTermQuery()`) sends `terms.prefix`
and `terms.limit` on every request; neither was in `TERMS_PARAMS`, so under
`strict_params = true` the request **400d**, and without it `terms.prefix` was silently
dropped (silent wrong answer — the #232 shape). This fixes both and retires the placeholder
`json.nl` guard along the way.

## Behaviour changes

- **`terms.prefix`** (finding 141): literal, **case-sensitive** `str::starts_with` on the
  indexed term — no analyzer runs over the prefix. Applied per field, **before** the
  count-descending sort. Absent or empty means no filter.
- **`terms.limit`** (finding 142): per-field truncation **after** the sort. Default 10
  (`TERMS_DEFAULT_LIMIT`) when absent. **Negative is the "unlimited" sentinel** (only `-1`
  is captured; any negative is treated as unlimited, named in a comment). `0` means zero,
  not "default". A non-integer (`abc`) is the one error case: HTTP 400 carrying an
  **empty-but-present `terms:{}` sibling** alongside `error`.
- **`json.nl`**: `/terms` is a Solr NamedList, so it now honours `map`/`arrarr`/`arrmap`
  through the existing `facet::render_named_list` + `JsonNl::from_params` (finding 142 /
  `terms_prefix_json_nl_map`). The outer `terms` object stays keyed by field name under
  every shape; only the inner `(term, count)` list reshapes. This **removes
  `check_terms_json_nl`**, the placeholder guard whose own doc comment said "Rendering these
  shapes for real (issue #153's named-list machinery) is what replaces this check."
- **Undefined `terms.fl`** (finding 141 / `terms_prefix_unknown_field`): now answers
  **200** with `{field: []}` instead of a 400 — stock `search_api_autocomplete` names
  fulltext fields an index may not have. The **defined-but-non-text** 400 is unchanged
  (`terms_non_text_field_is_rejected...`).

## Error-sibling mechanism

`ErrorExtra::terms: Option<Value>` + `WfError::with_terms` (the analogue of `with_response`),
rendered immediately before `error` and **absent by default** so no other error path grows a
`terms` key. `into_response` was refactored to Map-based assembly so the optional sibling
blocks (`response`, `terms`) slot into their fixed position only when set; every existing
envelope branch (Bare / NoParams / WithParams × omit_header × response × trace) is preserved.

## Tests (TDD)

Red tests committed first (`7d79577`), confirmed failing for the right reasons (prefix
ignored → full list; undefined → 400; `json.nl=map` → 400), then implementation (`6783464`).

- `tests/terms.rs`: hermetic coverage of every fixture row, on corpora whose analyzed terms
  are already pinned by the trace tests (not against `solr-ref/` values that depend on
  Solr's `dai` stemming — finding 103 / #205; the differential core produces `dai` via
  Porter and is unaffected). Four existing tests whose premises the #310 capture settled the
  other way were **inverted**, not deleted:
  - `terms_undefined_field_errors_with_solr_envelope` → `..._yields_an_empty_list`
  - `terms_dynamic_name_matching_no_rule_is_still_a_400` → `..._yields_an_empty_list`
  - `terms_json_nl_map_is_rejected...` → `..._renders_an_object`
  - `terms_json_nl_arrarr_is_rejected...` → `..._renders_nested_arrays`
- **Mutation test**: breaking the `terms.limit` integer parse (treat invalid as default)
  fails `terms_limit_invalid_returns_400_with_empty_terms_sibling` (got 200, expected 400);
  reverted.
- `tests/differential.rs`: the **15 `terms_*` `EXPECTED_DIVERGENCES` entries are deleted**
  — the guard confirmed each "now matches". The 16 `facet_extag_*` entries for **#295** are
  untouched (that feature has not landed).
- `tests/search_api_coverage.rs`: the strict-param guard moved `terms.limit` →
  `terms.mincount` (still deliberately omitted), exactly like the `hl.maxAnalyzedChars`
  move above it.

## Scope judgement (flagged for the reviewer)

The issue's "two existing tests that must change" note anticipated the undefined-field and
`json.nl=map` inversions. Honoring **all four** `json.nl` shapes (rather than map-only)
flips a third (`arrarr`). The reviewer confirmed this is **defensible and preferable**:
terms are NamedLists, the shared `render_named_list` is already fixture-backed for facets
and extract, `map` output exactly matches `terms_prefix_json_nl_map.json`, and honoring all
four is less code than a map-only special case plus a residual guard. `map` is
fixture-pinned; `arrarr`/`arrmap` ride the same shared renderer.

## Gates (CI runs exactly these)

```
cargo fmt --check            # clean
cargo clippy --all-targets -- -D warnings   # clean
cargo test                   # all green, hermetic (no network/Docker)
```

The differential harness (`cargo test --test differential`) is the compatibility evidence:
41 passed, the 15 deleted `terms_*` rows match their fixtures, the 16 `facet_extag_*` rows
for #295 still diverge as expected.

## Review

One read-only reviewer round (`openai-codex/gpt-5.6-sol`): verdict **approve on the code**
(json.nl scope sound; error envelope branch-for-branch preserved; `terms` absent by
default; full gate re-confirmed PASS), with two quick-fix doc-drift items, both addressed in
`35456f5` (stale "handler does not exist" / "only flat" / "map is a 400" wording in
`tests/terms.rs`; the "31 rows, both features unbuilt" comment in `tests/differential.rs`).

## Batch coordination (per the issue's wave-1 plan)

- **No `capture.sh` run** — all 16 fixtures were committed by #310; a stray run rewrites
  400+ files and dirties the sibling diffs.
- **Rebase + re-run gates before merge**: #295 and #308 both delete from the same
  `EXPECTED_DIVERGENCES` array, so a green branch + green `main` does not imply a green
  merge. (This branch's deletion range is contiguous with #295's, the exact conflict site.)

## Follow-ups

- `arrarr`/`arrmap` for `/terms` have no fixture (only `map`); captured if a client ever
  sends them.
- The wider `terms.*` set (`terms.sort`, `terms.lower`/`upper`, `terms.mincount`/`maxcount`,
  `terms.regex`, `terms.raw`, `terms.ttf`) remains deliberately absent from `TERMS_PARAMS`
  and documented as such — add when a capture needs one.
