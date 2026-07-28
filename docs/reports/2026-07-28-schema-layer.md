# Report: schema layer completion (issue #10)

- Branch: `schema-layer` (worktree off `main`)
- Issue: #10, v1 milestone. Spec: orchestrator task spec (scratchpad, not committed).
- Scope: PRD §3 — `[[dynamic_fields]]`, `[[copy_fields]]`, `[[field_types]]` analyzer
  chains, language presets (open question 5), numeric/date field types, startup
  schema-compatibility check (open question 4).

## Pipeline deviation

The four-stage pipeline (`test-writer → implementor → reviewer → reporter`) was **run by a
single agent, not four**. This agent was launched as a worker fork, whose hard rules forbid
spawning subagents; that overrides the directive's instruction to spawn each stage. Stages were
still executed in order — tests written and confirmed red before any implementation, green gate
enforced, then review, then this report — but with no fresh-context separation between them, so
the review is a self-review and carries less independence than a `reviewer` agent would. Worth a
second pass by an independent reviewer before merge.

## What was built

`src/schema.rs` (rewritten, 470 lines) and `src/core_index.rs` (extended).

**Field types.** `string`/`keyword` (unanalyzed), `text_general`, `text_en`, `text_<code>` for
all 18 `tantivy::tokenizer::Language` variants, `int`/`long` (i64), `float`/`double` (f64),
`date` (RFC3339 UTC). `text_general` and `text_en` map onto Tantivy's own `default` and `en_stem`
analyzers rather than hand-built equivalents, so the tracer bullet's captured relevance
behaviour is untouched — confirmed by the 12 tracer-bullet tests still passing unchanged.
Language presets stem but do not strip stopwords, matching `en_stem`'s shape; stopword removal
is available through a custom chain.

**Custom analyzer chains.** `[[field_types]]` with `tokenizer = "simple"` and ordered
`lowercase` / `stopwords` / `stemmer` filters, registered in a `TokenizerManager` that the index
is opened with. `language` accepts a name (`english`) or an ISO-639-1 code (`en`). Tantivy ships
no stopword list for Arabic, Greek, Romanian, Tamil or Turkish; a `stopwords` filter in those
languages is a load-time error, not a silent no-op.

**Dynamic fields.** Solr-style globs (leading and/or trailing `*`), longest-pattern-wins, static
fields taking precedence. Tantivy schemas are fixed at index creation, so dynamic fields cannot
each become a Tantivy field the way Solr's do. Their values go into two catch-all JSON fields —
`_dynamic` (unanalyzed types) and `_dynamic_text` (analyzed types), split because one JSON field
carries a single tokenizer and a `*_s` pattern must not be stemmed like a `*_txt` one. Queries
naming a dynamic field are rewritten to the JSON path (`count_i:7` → `_dynamic.count_i:7`); the
containers never appear in a response, stored dynamic fields come back as top-level keys.

**Copy fields.** Applied at index time; the destination analyzes the source's raw value with its
own field type. Both endpoints validated at load.

**Startup compatibility check.** The schema an index was built with is persisted as
`<data_dir>/wayfinder-schema.toml`; on open, a changed `[[fields]]` block refuses to start,
naming the field. This closes a live bug: the previous code did
`create_in_dir(...).or_else(open_in_dir)`, so a changed schema silently opened the index with
the *old* schema.

`docs/schema.md` documents the whole format.

## Solr capture — the issue's premise was wrong

The issue said "unknown field in a doc is an error in Solr — verify and match". Verified: **it
is not.** The `_default` configset used for all existing fixtures is *schemaless*
(`update.autoCreateFields` defaults to true), so an unknown document field is silently added to
the schema as `text_general` and the update returns HTTP 200.

Both behaviours are now captured, via a block appended to `solr-ref/capture.sh`:

| Fixture | Status | What it shows |
|---|---|---|
| `update_unknown_field_schemaless.json` | 200 | `_default` configset auto-adds the field |
| `update_unknown_field_strict.json` | 400 | `-Dupdate.autoCreateFields=false`: `ERROR: [doc=…] unknown field 'nosuchfield'` |

The strict capture needed a second container, because `update.autoCreateFields` is a JVM-wide
system property; `capture.sh` now starts `wayfinder-solr-ref-strict` on port 8984 for it.

Wayfinder matches the **strict** side. Schemaless auto-add is runtime schema mutation, which PRD
§3 rules out — and it is exactly the gap `[[dynamic_fields]]` fills.

## Deviation from PRD open question 4

PRD open question 4 and the issue both say "adding a field is compatible". **It is not, under
Tantivy**: a schema is fixed when an index is created and cannot be extended in place
(`IndexBuilder::open_or_create` rejects any schema difference). So an added field still requires
a reindex. Rather than let Tantivy fail later with its opaque "An index exists but the schema
does not match", the check refuses up front, names the field, and says a reindex into a fresh
data directory is needed. The PRD wording should be corrected, or the behaviour revisited if
in-place schema extension becomes a goal (it would mean rewriting `meta.json` and accepting that
old segments lack the field — deliberately not attempted).

## Test evidence

25 new tests in `tests/schema_layer.rs` (21 in the first pass, 4 more from review round 1); two
helpers appended to `tests/common/mod.rs`
(`app_with_schema`, `post_docs`). Confirmed red before implementation (compile error on the
then-private `wayfinder::schema` module and the missing API).

`command cargo test` — **37 passed, 0 failed** after review round 1 (25 schema-layer + 12
tracer-bullet). The listing below is the first-pass run; round 1 added
`schema_compatibility_check_refuses_adding_the_first_or_removing_the_last_dynamic_rule`,
`schema_compatibility_check_allows_editing_dynamic_rules_without_emptying_them`,
`reopening_a_data_dir_after_toggling_dynamic_fields_refuses_both_ways` and
`a_pattern_with_stars_at_both_ends_is_rejected_at_load_time`.

```
running 21 tests   (tests/schema_layer.rs)
test schema_compatibility_check_allows_toggling_required ... ok
test schema_compatibility_check_accepts_an_identical_schema ... ok
test schema_compatibility_check_refuses_a_changed_field_option_naming_it ... ok
test schema_compatibility_check_refuses_a_removed_field_naming_it ... ok
test schema_compatibility_check_refuses_a_retyped_field_naming_it ... ok
test schema_compatibility_check_reports_an_added_field_as_needing_a_reindex ... ok
test a_language_preset_ships_for_every_tantivy_stemmer_language ... ok
test dynamic_field_with_no_matching_pattern_is_none ... ok
test static_field_takes_precedence_over_dynamic_pattern ... ok
test text_presets_tokenize_as_expected ... ok
test dynamic_field_matching_is_longest_pattern_wins ... ok
test custom_field_type_applies_filters_in_declared_order ... ok
test copy_field_with_unknown_source_or_dest_errors_naming_it ... ok
test unsupported_field_type_errors_naming_the_field ... ok
test unknown_filter_kind_errors_naming_the_field_type ... ok
test doc_with_unknown_field_is_rejected_like_strict_solr ... ok
test wrong_json_type_for_a_typed_field_is_rejected ... ok
test copy_field_makes_source_text_searchable_on_dest ... ok
test numeric_and_date_values_round_trip ... ok
test doc_field_matching_a_dynamic_pattern_is_indexed_and_returned ... ok
test reopening_a_data_dir_with_a_changed_schema_refuses_with_a_clear_error ... ok
test result: ok. 21 passed; 0 failed; 0 ignored

running 12 tests   (tests/tracer_bullet.rs)  — all pre-existing, unchanged
test result: ok. 12 passed; 0 failed; 0 ignored
```

`cargo fmt --check` clean. `cargo clippy --all-targets -- -D warnings` **clean** — note this is
CI's exact command, and the two warnings the tracer-bullet report listed as outstanding
(`result_large_err` at `src/lib.rs:68`, `collapsible_if` at `src/lib.rs:131`) do not reproduce
on this toolchain; nothing in this branch touched them.

**Test strength check.** Because stages were not independent, the two riskiest paths were
mutation-tested rather than trusted: disabling the dynamic-field query rewrite failed
`doc_field_matching_a_dynamic_pattern_is_indexed_and_returned` (400 vs 200), and flipping
longest-pattern-wins to shortest failed two tests. Both mutations reverted.

## Review outcome

**Two rounds: a self-review, then an independent review that bounced the work.**

### Round 0 — self-review (see pipeline deviation above)

One must-fix found and fixed: `check_compatible` treated a `required` toggle as needing a
reindex, but `required` is input validation and not part of the Tantivy schema — it now compares
only `type`/`stored`/`fast`/`multi_valued`, with a test each way
(`..._allows_toggling_required`, `..._refuses_a_changed_field_option_naming_it`).

### Round 1 — independent review: BOUNCE, one must-fix

The reviewer verified both premise-challenging findings above against the Tantivy 0.26.1 source
and confirmed them, along with the 18-entry `Language` table and the absent `StopWordFilter`
arms for Arabic/Greek/Romanian/Tamil/Turkish. Approved: copy-fields don't recurse, the `required`
narrowing, numeric/date mapping for #3/#5, no over-engineering.

**Must-fix: `check_compatible` had a hole in exactly the case it exists to catch.** The check
compared only `[[fields]]`, on the documented reasoning that `[[dynamic_fields]]` never alters
the Tantivy schema. That was false at the empty↔non-empty boundary: `parse()` adds the
`_dynamic`/`_dynamic_text` JSON fields only when at least one rule exists, so adding the *first*
rule or removing the *last* one on an existing data dir changed the Tantivy schema while the
check reported "compatible" — and `CoreIndex::open`'s
`create_in_dir(...).or_else(|_| open_in_dir(...))` then silently opened the index with its old
schema. The exact silent-stale-schema failure this feature was built to prevent, through a door
the check wasn't watching. No test toggled dynamic fields on reopen, so nothing caught it.

Fixed by giving the decision one owner: `catch_all_fields(rules)` returns the catch-all field
names a rule set causes to exist, and both `parse()` (which adds them) and `check_compatible()`
(which refuses a change to the set) now call it, so the two cannot drift again. Message:

```
[[dynamic_fields]] went from 0 rule(s) to 1; the existing index has no catch-all field to hold
their values — reindex into a fresh data directory
```

and, the other way, `... went from 1 rule(s) to 0; the existing index still carries the catch-all
fields they created — ...`.

Three new tests, and the false premise corrected in the `check_compatible` doc comment, in the
`parse()` comment that asserted the catch-all fields are "always present", and in
`docs/schema.md`:

- `schema_compatibility_check_refuses_adding_the_first_or_removing_the_last_dynamic_rule`
- `reopening_a_data_dir_after_toggling_dynamic_fields_refuses_both_ways` — the end-to-end case,
  both directions
- `schema_compatibility_check_allows_editing_dynamic_rules_without_emptying_them` — the boundary
  is emptiness, not any rule edit

Mutation-verified: disabling the new check fails both
`reopening_a_data_dir_after_toggling_dynamic_fields_refuses_both_ways` and
`schema_compatibility_check_refuses_adding_the_first_or_removing_the_last_dynamic_rule`, the
former by returning `Ok(Router)` — i.e. reproducing the silent open the reviewer described.

**Cosmetic items, both fixed.** The two-star glob arm implemented substring matching, a form Solr
never produces and whose semantics aren't Solr's anyway; patterns are now validated at load time
(`validate_pattern`) so only `*suffix`, `prefix*` and bare `*` are accepted, and the arm is gone
along with the dead `(Some("*"), _)` half of the first arm. Covered by
`a_pattern_with_stars_at_both_ends_is_rejected_at_load_time`.

## Follow-ups

1. **Single-valued field given a JSON array** is accepted, indexing every value and returning
   only the first. Solr errors ("multiple values encountered for non multiValued field"). Needs
   a fixture and a 400.
2. **Copy-field into a single-valued destination** has the same shape of problem: the
   destination silently keeps one value.
3. **No test for a dynamic `date` pattern.** Dynamic date values are validated then handed to
   Tantivy's JSON date detection; the round trip is untested.
4. **Pre-#10 indexes have no schema snapshot**, so the startup check cannot run for them and the
   old silent-`open_in_dir` behaviour still applies to those directories only.
5. **Query rewriting is a `<ident>:` scan, not a parser** (`ponytail:` comment in
   `core_index.rs`): a dynamic field name inside a quoted phrase would also be rewritten.
   Revisit when #8 gives the query layer a real parser.
6. **Only the `simple` tokenizer** is exposed to `[[field_types]]`. `whitespace`, `ngram` and
   `regex` are all available in Tantivy and cheap to add when something needs them.
7. **Reference container hygiene:** probing the schemaless behaviour permanently added a
   `nosuchfield` field to the running `wayfinder-solr-ref` container's schema (Solr refuses to
   delete it — an auto-generated copy-field directive references it). The 5-doc corpus was
   verified back at `numFound: 5`, and no captured fixture is affected, but the container should
   be recreated (`docker rm -f wayfinder-solr-ref && solr-ref/capture.sh`) before trusting a
   live differential run from issue #1.

## Pointers

- Schema layer: `src/schema.rs`
- Indexing/query/render changes: `src/core_index.rs`
- Format reference: `docs/schema.md`
- Tests: `tests/schema_layer.rs`, helpers in `tests/common/mod.rs`
- New fixtures: `solr-ref/responses/update_unknown_field_{strict,schemaless}.json`,
  capture block appended to `solr-ref/capture.sh`
- `src/lib.rs` change is one word: `mod schema` → `pub mod schema` (tests need the module).
  No routing or envelope code touched, per the sibling-branch constraint.
