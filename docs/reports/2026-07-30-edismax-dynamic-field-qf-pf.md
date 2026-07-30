# edismax `qf`/`pf` never resolves dynamic-field names (issue #84)

## Bug

`resolve_field_weights` in `src/core_index.rs` resolved every `qf`/`pf` field
name with a literal `wf_schema.field(&name)` lookup, i.e. a declared
`[[fields]]` handle only. Any name that matched solely a `[[dynamic_fields]]`
pattern — e.g. `ts_title` under the `presets/search-api.toml` `ts_*`/`tm_*`
convention that `search_api_solr` clients rely on — was silently dropped from
the disjunction. This was already inconsistent with the query-*text* path
(`rewrite_dynamic_fields`), which does resolve dynamic names for `q` itself;
`qf`/`pf` were the one place edismax still only understood static schema
fields. The result: a `qf` naming *only* dynamic fields resolved to an empty
list and hard 400'd ("edismax `qf` names no field this core has"), while `pf`
(which tolerates an empty resolution by skipping the phrase-boost clause)
failed silently instead, with the boost simply missing.

Discovered via issue #80's integration harness running edismax against
`presets/search-api.toml`, where `ts_*`/`tm_*` dynamic names are exactly what
a schemaless Solr client sends in `qf`.

## Fix

`src/core_index.rs`:

- New `FieldTarget` enum: `Static(Field)` or `Dynamic { container: Field, path: String }`.
- `resolve_field_weights` now returns `Vec<(FieldTarget, f32)>` instead of
  `Vec<(Field, f32)>`.
- New `field_target(&self, name: &str) -> Option<FieldTarget>` applies the
  same static-before-dynamic precedence indexing already uses
  (`is_static`/`match_dynamic`): a declared field wins; otherwise
  `wf_schema.match_dynamic` + `dynamic_target` route the name to the
  catch-all container's JSON sub-path.
- New `term_for_target` builds the JSON-path term for a dynamic target the
  same way Tantivy's own `generate_literals_for_json_object` does
  (`Term::from_field_json_path` + `append_type_and_str`), matching the term
  shape `add_object` produced when the value was indexed.
- New `tokenize_for_target` (replacing `tokenize_for_field`) picks the
  analyzer off the catch-all container's `JsonObjectOptions` for a dynamic
  target (`_dynamic_text` = `text_en`, `_dynamic` = `raw`) instead of off a
  declared field's `Str` options.
- `build_field_disjunction` (the `qf` clause builder) and `build_pf_query`
  (the `pf` phrase-boost builder) updated to consume `FieldTarget` via these
  helpers instead of a raw `Field`.

### Known ceiling, documented not fixed

A `qf`/`pf` naming only a *non-text* dynamic field (e.g. a numeric `is_*`/
`ds_*` rule in the `_dynamic` container) now resolves and the request 200s
with `numFound: 0`, instead of the old 400 "names no field this core has".
`field_target` resolves a dynamic name to a string-typed JSON term
unconditionally, which is correct for `_dynamic_text` (the case `qf`/`pf`
exist for) but wrong for a numeric dynamic field, whose Tantivy term
encoding this code does not attempt to reproduce (`convert_to_fast_value_and_
append_to_json_term` is private to Tantivy's own query parser). This is a
loud-wrong-answer becoming a quiet-wrong-answer for that one case, called out
with a `ponytail:` comment on `field_target`. Raising it means encoding
numeric JSON terms here; out of scope for this issue.

## Tests

`tests/edismax.rs`, two new tests under a new "qf/pf: dynamic-field names,
not just static ones (issue #84)" section, with a schema fixture
(`DYNAMIC_QF_EDISMAX_SCHEMA_TOML`) declaring a `ts_*` dynamic pattern
mirroring `presets/search-api.toml`:

- `qf_naming_only_a_dynamic_field_matches_instead_of_dropping_it` — indexes
  two docs with identical unrelated `body` text and distinguishing
  `ts_title` values, then asserts `qf=ts_title` returns 200 (not the old 400)
  and matches only the doc whose `ts_title` contains the query term.
- `pf_naming_only_a_dynamic_field_still_boosts_the_adjacent_match` — asserts
  on relative scoring rather than status, since `pf`'s failure mode was
  silent (an empty resolution just skips the phrase-boost clause), so a 200
  alone would prove nothing; confirms the doc with the adjacent phrase in the
  dynamic `ts_phrase` field outranks the doc without it.

Both tests were confirmed to fail for the right reason (400 / missing boost)
against `main` before the `src/core_index.rs` fix, and pass with it. No test
changes were needed beyond these two additions — the fix is `src/` only.

## Gates

- `cargo test`: 490 passed, 0 failed (23 suites).
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- No entries exist in `EXPECTED_DIVERGENCES` / `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`
  (`tests/differential.rs`) for this bug, so none needed deleting on landing.

## Review

Two rounds (reviewer capped at 2 per the pipeline convention — this diff
would benefit from at least one more pass given its size and the JSON-term
encoding it touches):

- **Round 1**: three doc/comment-only nits bounced to the implementor — a
  stale doc comment misplaced on the wrong function, a `ponytail:` comment
  that understated the numeric-dynamic-field ceiling, and test doc comments
  still describing the bug in present tense after the fix landed. All three
  were fixed with comment-only edits; no logic or assertion changed.
- **Round 2**: approved clean, no further findings.

## Follow-ups (not filed as issues yet)

1. A dynamic field name containing a literal `.` produces a dead `qf`/`pf`
   clause: `Term::from_field_json_path` splits the path on `.` while
   `add_object` escaped a literal dot as part of one JSON-key segment at
   index time, so the term shapes diverge. This is pre-existing on other
   dynamic-field code paths too (not introduced by this fix), but is now
   also reachable through `qf`/`pf`.
2. No test currently pins the numeric-dynamic-field ceiling behavior
   described above (a `qf` naming only a numeric dynamic field returning
   200/`numFound: 0` rather than erroring) — it is documented in the
   `field_target` `ponytail:` comment but has no guard test. Per this repo's
   deliberate-skip convention, this should either get a fixture test that
   pins the current (quiet-wrong) behavior explicitly, or be raised to a
   filed issue tracking the fix.
