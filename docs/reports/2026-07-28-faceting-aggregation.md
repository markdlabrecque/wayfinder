# Report: faceting completion + real fast-field aggregation (issue #3)

- Branch: `3-faceting-aggregation` (worktree off `main`)
- Issue: #3, v1 milestone. Spec: orchestrator task spec (scratchpad, not committed). Surfaced by
  issue #1's differential harness, which found the tracer bullet's facet counting counted the
  *hit set* rather than Solr's whole-term-dictionary enumeration — this issue's real scope was
  larger than its issue text implied.
- Work is **uncommitted** in this worktree; the orchestrator commits it.

## Pipeline deviation

The sub-orchestrator running this pipeline **stalled twice mid-flight**, ending its turn while a
stage was outstanding. The parent orchestrator verified the worktree state directly and took over
relaying from review round 2 onward. Stages 1 (test-writer) and 2 (implementor) ran normally — red
tests confirmed first, green gate held. Round 1 review was run by a Sonnet reviewer; round 2 was a
fresh, independent Opus reviewer, because the `reviewer` agent definition was switched to Opus
mid-flight. Round 2 could not be the same agent as round 1 — the stalled sub-orchestrator held that
agent's context, so it was unreachable. Recording this as a deviation from the pipeline's normal
"same agent bounces back to itself" pattern, though the outcome was a stronger final pass (a fresh
Opus review against the Tantivy source) rather than a weaker one.

## What was built

Real fast-field aggregation, replacing the tracer bullet's stored-field counting. New
`src/facet.rs`; `CoreIndex::facet_counts` deleted; `CoreIndex::term_facet` added.

Params (ten new entries in `SELECT_PARAMS`): `facet.query` (repeatable), `facet.range` +
`.start`/`.end`/`.gap`, `facet.limit`, `facet.mincount`, `facet.sort`, `facet.missing`,
`json.nl=map`, and support for multiple repeated `facet.field`s. Two named ceilings:
`MAX_FACET_TERMS` and `MAX_RANGE_BUCKETS` (65,536 range buckets, `src/facet.rs:57`).

16 new fixtures. `facet.range` needed a numeric/date field the 5-doc reference corpus lacks, so it
was captured against a **second core** (`facets`, 4 docs, `views`/`created`/`note`) rather than by
changing that corpus — changing it would have rewritten ground truth project-wide.

**All seven `facet_*` entries removed from `EXPECTED_DIVERGENCES`** in `tests/differential.rs`,
leaving only `select_sort` (issue #2, in flight) and `ping`. Verified directly in the diff:

```
const EXPECTED_DIVERGENCES: &[(&str, &str)] = &[
    (
        "select_sort",
        "sort *ordering* unbuilt, issue #2 ...",
    ),
    (
        "ping",
        "`responseHeader.params` carries Solr ping-handler artifacts ...",
    ),
];
```

This is the second time that self-expiring guard has driven cleanup on its own (the first was
issue #11's sort-validation landing).

## Round 1 (Sonnet) — two real findings

1. **A panic on user input.** `src/facet.rs`'s date-bucket walk did `lower + gap`, and `time`
   0.3's `Add` impl unwraps `checked_add` internally; both ends of that addition come from the
   request. Fixed with `checked_add` + `bail!` -> a 400 instead of a panic (`src/facet.rs:330-337`).
   Pinned by `facet_range_overflowing_the_date_range_is_a_400_not_a_panic`; the implementor
   confirmed the test bites by reverting the fix and reproducing the panic at `src/facet.rs:332`.
2. **A guard whose mutation test didn't bite.** `assert_facet_400` asserted only the field name,
   so deleting the `Some(field) if !field.fast` arm from `check_facetable` left all 36 tests
   green — the exact "silently return empty counts" failure this issue exists to prevent.
   Strengthened with a `Refusal` enum that asserts Wayfinder's own refusal wording; the re-run
   mutation is now caught by three tests. This also surfaced that
   `facet_range_on_a_non_fast_field_is_a_400` had been passing through the `ValueKind::Text`
   bail path rather than the intended non-`fast` path.
3. An overclaim corrected: term-dictionary enumeration is genuine for **string** fields only
   (numeric/date facet fields walk a bucketed range, not the term dictionary).

## Round 2 (Opus) — approved, no must-fix

Verified rather than accepted, against the actual worktree and the Tantivy source:

- **Enumeration is real, and the round-1 correction is accurate.** Checked against
  `tantivy-0.26.1/src/aggregation/bucket/term_agg.rs`: the `ColumnType::Str` branch opens at
  line 991, the `min_doc_count == 0` dictionary stream is lines 1024-1053, `DateTime` at 1054,
  `Bool` at 1061, `IpAddr` at 1067, the numeric `else` branch at 1088, block closes at 1114. No
  hardcoded or derived term list; `min_doc_count: Some(0)` with `segment_size` as the walk
  budget, bounded by the `if dict.len() >= term_req.req.segment_size` break at line 1030.
- **The divergence's scope is not wider than string-vs-numeric**, so the write-up doesn't
  understate it: `facet.missing` is counted via `ExistsQuery` + `Count`
  (`src/facet.rs:180-182`), so it is column-type-independent; multi-valued string fields are
  unaffected since the zero-fill is a term-dictionary stream; `Bool`/`IpAddr` are unreachable
  because Wayfinder's `ValueKind` has only `I64`/`F64`/`Date`/`Text`.
- **Stored-field path genuinely deleted, not orphaned** — no stub left behind,
  `use tantivy::schema::Value as _` removed with it, `DocSetCollector` is still legitimately used
  for `fq` at `src/core_index.rs:366`, and no param combination reaches stored values.
- **Reference corpus untouched** — `git diff main --name-status -- solr-ref/responses/` is empty,
  all 16 fixtures are untracked adds, `capture.sh` is +99/-0 appended after line 180, and the
  5-doc corpus plus `tests/common/mod.rs::corpus()` are unchanged.
- Leaving the i64/f64 range walks unguarded by an explicit overflow check was correct: both are
  bounded by `guard_bucket_count` (max 65,537 iterations), the i64 walk uses `saturating_add`,
  and the f64 walk rejects `NaN` gaps.
- `git diff main -- src/collector.rs` is empty; `core_index.rs::search` is untouched.
  `src/lib.rs::select` is restructured (the `parsed` binding hoisted so facets can rebuild the
  base query) — unavoidable for this feature, and left mechanical for issue #2's rebase.

This closed the review at the 2-round cap with no must-fix items outstanding. Round 2 was a
verification pass that approved the work, not a second bounce — but per the pipeline's own rule,
the work has now had its full two permitted rounds; there was no capacity for a third pass had one
been needed, so **this work could use more review passes** if anything else surfaces later.

## Test evidence

Verified independently by this reporter.

`command cargo test` — full output:

```
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.11s
     Running unittests src/lib.rs (target/debug/deps/wayfinder-f5cb2155803ff508)

running 8 tests
test config::tests::empty_config_is_all_defaults ... ok
test config::tests::a_partial_section_keeps_the_other_defaults ... ok
test config::tests::unknown_key_is_rejected_by_name ... ok
test config::tests::missing_file_is_all_defaults ... ok
test config::tests::bad_enum_values_are_rejected_by_value ... ok
test config::tests::zero_writer_threads_is_rejected ... ok
test config::tests::index_settings_reflect_the_resource_knobs ... ok
test config::tests::unknown_key_in_a_section_is_rejected_by_name ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/main.rs (target/debug/deps/wayfinder-80041d8a7da54b80)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/differential.rs (target/debug/deps/differential-22e815937c8c1582)

running 18 tests
test doc_ids_extracts_ordered_id_list_from_an_envelope ... ok
test normalize_drops_error_msg_and_metadata_but_keeps_code ... ok
test differing_error_code_is_still_a_diff ... ok
test differing_error_msg_and_metadata_do_not_appear_as_a_diff ... ok
test diff_fails_on_doc_reordered ... ok
test diff_fails_on_numfound_off_by_one ... ok
test live_solr_matches_committed_query_set ... ok
test differing_qtime_does_not_appear_as_a_diff ... ok
test diff_fails_on_facet_count_changed ... ok
test normalize_drops_qtime_and_logs_touched_path ... ok
test params_object_equality_is_key_order_insensitive_by_construction ... ok
test ranked_id_order_difference_fails_even_with_identical_membership ... ok
test ranked_id_order_matching_passes ... ok
test score_outside_tolerance_fails ... ok
test score_within_tolerance_passes_and_is_logged ... ok
test load_manifest_parses_every_line_of_the_real_manifest ... ok
test load_manifest_skips_blanks_and_comments_and_tolerates_trailing_columns ... ok
test hermetic_whole_query_set_matches_committed_fixtures ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s

     Running tests/error_shapes.rs (target/debug/deps/error_shapes-f84616a535c97fd2)

running 12 tests
test bad_query_syntax_matches_solr_error_shape ... ok
test sort_on_a_non_fast_field_matches_solr_error_shape ... ok
test unknown_field_in_q_matches_solr_error_shape ... ok
test unknown_request_params_are_still_ignored ... ok
test update_with_an_unsupported_method_matches_solr_error_shape ... ok
test select_serves_non_get_methods_like_solr ... ok
test unknown_field_in_fq_is_a_400_error_envelope ... ok
test update_with_malformed_json_matches_solr_error_shape ... ok
test unknown_core_is_404_with_a_json_error_envelope ... ok
test update_with_a_non_array_body_is_a_400_error_envelope ... ok
test missing_q_returns_zero_results_like_solr ... ok
test sort_on_an_unknown_field_is_an_error ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s

     Running tests/faceting.rs (target/debug/deps/faceting-055b702ae61f23f8)

running 37 tests
test facet_limit_unlimited_is_also_capped_by_facet_limit_max ... ok
test facet_limit_zero_returns_an_empty_array ... ok
test facet_basic_matches_fixture ... ok
test facet_all_filtered_matches_fixture ... ok
test facet_limit_matches_fixture ... ok
test counts_come_from_the_term_dictionary_not_the_hit_set_one_doc ... ok
test facet_counts_always_carries_all_five_sub_objects ... ok
test facet_limit_minus_one_is_unlimited ... ok
test facet_counts_is_absent_unless_facet_is_true ... ok
test counts_come_from_the_term_dictionary_not_the_hit_set_two_docs ... ok
test facet_limit_above_facet_limit_max_is_capped ... ok
test facet_json_nl_map_matches_fixture ... ok
test facet_mincount_matches_fixture ... ok
test facet_mincount_defaults_to_zero_and_keeps_zero_count_terms ... ok
test facet_missing_counts_only_docs_in_the_hit_set ... ok
test facet_mincount_above_every_count_leaves_an_empty_array ... ok
test facet_on_a_stored_only_field_is_a_400_not_an_empty_array ... ok
test facet_mincount_one_drops_the_zero_count_terms ... ok
test facet_on_a_non_fast_field_is_a_400_not_an_empty_array ... ok
test facet_missing_matches_fixture ... ok
test facet_query_is_intersected_with_q_and_every_fq ... ok
test facet_query_matches_fixture ... ok
test facet_query_matching_nothing_is_zero_not_an_omitted_key ... ok
test facet_on_an_undefined_field_is_a_400_not_an_empty_array ... ok
test facet_range_on_a_non_fast_field_is_a_400 ... ok
test facet_range_over_a_date_field_matches_fixture ... ok
test repeated_facet_query_is_keyed_by_the_verbatim_query_string ... ok
test json_nl_map_switches_every_facet_field_to_an_object ... ok
test repeated_facet_field_gives_each_field_its_own_key ... ok
test facet_range_overflowing_the_date_range_is_a_400_not_a_panic ... ok
test facet_zero_matches_fixture ... ok
test facet_sort_index_matches_fixture ... ok
test facet_sort_count_breaks_ties_on_term_ascending ... ok
test facet_sort_index_is_term_ascending_regardless_of_count ... ok
test json_nl_map_switches_range_counts_to_an_object ... ok
test facet_range_over_a_numeric_field_matches_fixture ... ok
test strict_params_accepts_every_implemented_facet_param ... ok

test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.14s

     Running tests/schema_layer.rs (target/debug/deps/schema_layer-7ef10674c6ce6122)

running 25 tests
test dynamic_field_with_no_matching_pattern_is_none ... ok
test dynamic_field_matching_is_longest_pattern_wins ... ok
test a_pattern_with_stars_at_both_ends_is_rejected_at_load_time ... ok
test copy_field_with_unknown_source_or_dest_errors_naming_it ... ok
test schema_compatibility_check_allows_editing_dynamic_rules_without_emptying_them ... ok
test schema_compatibility_check_allows_toggling_required ... ok
test schema_compatibility_check_accepts_an_identical_schema ... ok
test schema_compatibility_check_refuses_a_removed_field_naming_it ... ok
test schema_compatibility_check_refuses_a_retyped_field_naming_it ... ok
test schema_compatibility_check_refuses_a_changed_field_option_naming_it ... ok
test schema_compatibility_check_reports_an_added_field_as_needing_a_reindex ... ok
test schema_compatibility_check_refuses_adding_the_first_or_removing_the_last_dynamic_rule ... ok
test a_language_preset_ships_for_every_tantivy_stemmer_language ... ok
test custom_field_type_applies_filters_in_declared_order ... ok
test static_field_takes_precedence_over_dynamic_pattern ... ok
test text_presets_tokenize_as_expected ... ok
test unknown_filter_kind_errors_naming_the_field_type ... ok
test unsupported_field_type_errors_naming_the_field ... ok
test doc_with_unknown_field_is_rejected_like_strict_solr ... ok
test wrong_json_type_for_a_typed_field_is_rejected ... ok
test reopening_a_data_dir_with_a_changed_schema_refuses_with_a_clear_error ... ok
test copy_field_makes_source_text_searchable_on_dest ... ok
test doc_field_matching_a_dynamic_pattern_is_indexed_and_returned ... ok
test numeric_and_date_values_round_trip ... ok
test reopening_a_data_dir_after_toggling_dynamic_fields_refuses_both_ways ... ok

test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.45s

     Running tests/server_config.rs (target/debug/deps/server_config-9e197fc5ab923508)

running 18 tests
test doc_store_knobs_reach_the_tantivy_index_settings ... ok
test no_merge_policy_is_accepted ... ok
test rows_limit_clamps_a_larger_requested_rows ... ok
test strict_params_rejects_unknown_param_on_update ... ok
test strict_params_allows_every_implemented_param ... ok
test empty_config_file_means_all_defaults ... ok
test missing_config_file_means_all_defaults ... ok
test strict_params_rejects_unknown_param_with_solr_error_envelope ... ok
test unknown_merge_policy_is_rejected ... ok
test unknown_top_level_key_is_rejected_by_name ... ok
test unknown_doc_store_compression_is_rejected ... ok
test commit_and_budget_knobs_parse_and_are_exposed ... ok
test unknown_key_inside_a_section_is_rejected_by_name ... ok
test unknown_section_is_rejected_by_name ... ok
test rows_below_the_limit_is_untouched ... ok
test strict_params_still_accepts_the_commit_param_on_update ... ok
test writer_heap_below_tantivys_minimum_is_a_startup_error ... ok
test multiple_writer_threads_still_index_and_search ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.78s

     Running tests/tracer_bullet.rs (target/debug/deps/tracer_bullet-1415712e72de5d58)

running 12 tests
test select_unknown_param_is_ignored_but_echoed ... ok
test facet_on_multi_valued_field_matches_flat_alternating_array_shape ... ok
test select_zero_results_has_correct_envelope ... ok
test select_all_returns_all_docs_with_default_fl_and_no_internal_fields ... ok
test select_pagination_start_and_rows ... ok
test ping_reports_ok ... ok
test select_rows_zero_returns_empty_docs_but_correct_num_found ... ok
test select_doc_with_no_value_for_optional_multi_valued_field_omits_key ... ok
test select_without_facet_param_has_no_facet_counts_key ... ok
test select_with_fq_filters_results ... ok
test select_unknown_fl_field_is_silently_dropped ... ok
test select_pagination_past_the_end_returns_empty_docs ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s

   Doc-tests wayfinder

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

130 passed / 0 failed across the crate (8 lib + 18 differential + 12 error_shapes + 37 faceting +
25 schema_layer + 18 server_config + 12 tracer_bullet). `tests/faceting.rs` is the new suite for
this issue (37 tests, all passing, including the two mutation-guard tests from round 1).

`command cargo fmt --check` — clean, no output, exit 0.

`command cargo clippy --all-targets -- -D warnings` — CI's exact command:

```
    Checking wayfinder v0.1.0 (/Users/mark/Projects/wayfinder/.claude/worktrees/agent-aef7ffd2e9f6e69f1)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.76s
```

Clean, zero warnings, exit 0.

## Follow-ups to record

- **Object key order is not Solr's, and `json.nl=map` makes this newly load-bearing.**
  `serde_json` is built without `preserve_order` (no `indexmap` in `Cargo.lock:936-946`), so every
  emitted object is alphabetised by `BTreeMap`. Solr's map preserves count-descending term order,
  and `facet_ranges.counts` under `json.nl=map` would emit `"0","10","100","110"` for a 0-200/10
  range instead of numeric order. Both current fixtures coincidentally sort the same way, and
  `assert_matches_fixture` compares parsed `Value`s so it cannot catch this. Capture a wider range
  before any client depends on map ordering. **This is the most consequential follow-up on the
  branch.**
- `src/facet.rs:348-351` — the `ValueKind::Text` range-facet bail is unreachable from any test
  (both text fields in `RANGE_SCHEMA_TOML` are non-`fast`, so `check_facetable` fires first). A
  `fast` text field is the only way in; a cheap test would have caught round 1's mislabelled case
  earlier.
- Unpinned Solr semantics needing fixtures rather than code changes: `facet.missing` combined
  with `facet.limit`/`facet.mincount` (implementation exempts the `null` bucket from both); a
  `facet.range` span not divisible by `gap` (implementation echoes the requested `end` verbatim;
  Solr may echo a gap-aligned end when `hardend` is unset — unconfirmed); `json.nl=arrarr`/`arrmap`
  accepted but rendered flat; `json.nl=map` + `facet.missing` empty-string key.
- **Finding 16's unfacetable-field 400 is "deliberate by findings-note only"** —
  `docs/PRD.md` is not touched by this diff, and this project's `CLAUDE.md` says a divergence is
  a bug unless the PRD documents it as deliberate. Finding 15 (unknown-core JSON-vs-HTML, from
  issue #11) has the identical status. Two divergences now await PRD ratification; that is an
  orchestrator/user decision, not implementor work, and is recorded here as needing the user's
  call.
- Applied by the orchestrator directly (trivial, no pipeline stage needed): `src/config.rs`'s
  `facet_limit_max` doc comment said "applied when `facet.limit` lands (issue #3 owns that
  param)" — now corrected, since it is live.
- Not a finding, recorded so it isn't re-raised: lenient parsing of
  `facet.limit`/`facet.mincount`/`facet.sort` (a bad value falls back to the default rather than
  400) matches the established `rows`/`start` convention at `src/lib.rs:339-350`.
- The three unfacetable-field captures are core-relative GETs but were parked in
  `manifest-errors.tsv` deliberately: a `manifest.tsv` row would need a permanent
  `EXPECTED_DIVERGENCES` entry, contradicting that list being self-expiring. Recorded so a later
  reader doesn't "fix" it.

## Review depth

Round 2 was the second and final round permitted by the pipeline's 2-round cap. It closed as an
approval with no must-fix items outstanding, but per the pipeline's own rule this means the work
has now used its full allotment of review passes — if anything else surfaces later there is no
built-in headroom left for a third round without escalating to the orchestrator.

## Pointers

- Aggregation logic: `src/facet.rs` (new)
- Deleted: `CoreIndex::facet_counts` (stored-field counting); added `CoreIndex::term_facet`
- Params wiring: `src/lib.rs` (`SELECT_PARAMS`, ten new entries; `select` restructured to hoist
  the `parsed` query binding so facets can rebuild the base query)
- Config doc-comment fix: `src/config.rs` (`facet_limit_max`)
- Tests: `tests/faceting.rs` (new, 37 tests)
- New fixtures: `solr-ref/responses/facet_*.json` (16 files), captured against a second core
  (`facets`) appended to `solr-ref/capture.sh`
- Descope list: `tests/differential.rs::EXPECTED_DIVERGENCES` — all seven `facet_*` entries
  removed, leaving only `select_sort` (#2) and `ping`
- Error captures for unfacetable fields: `solr-ref/manifest-errors.tsv` (parked deliberately, not
  `manifest.tsv`)
- Findings: `docs/solr-ref-findings.md` finding 16 (unfacetable-field 400), pending PRD
  ratification alongside finding 15 from issue #11
