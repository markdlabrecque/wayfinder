# Issue #128 — v2.5: schema view, read-only, from the running core's schema

Worktree: `/Users/mark/Projects/wayfinder-128-schema-view`
Branch: `128-admin-ui-schema-view`

Delivers the third page of PRD §5 "v2.5 — Admin web UI," following the
tracer bullet in #94 and the query tester in #127: `GET /ui/schema`, a
read-only view of the running core's schema — fields with their
`stored`/`fast`/`multi_valued`/`required` flags, dynamic-field patterns and
types, and copy-field source/dest pairs — rendered from the in-process
`WayfinderSchema` the core was opened with, never re-parsed from disk.

## What was built

- **`src/admin_ui.rs`**: `SchemaPage<'a>` — an `askama::Template` deriving
  from `templates/schema.html` — plus `render_schema_page(core_name, fields,
  dynamic_fields, copy_fields) -> Result<String, askama::Error>`. Takes
  `&[FieldConfig]`, `&[DynamicFieldConfig]`, `&[CopyFieldConfig]` straight
  from `crate::schema`, so there is no intermediate DTO that could drift from
  the real schema types. Ten unit tests: five originally, plus five added by
  the implementor after self-reporting mutation gaps (see Pipeline below) —
  `schema_page_renders_each_field_flag_in_its_own_column` (a single render,
  five fields differing in exactly one flag each, every flag cell pinned
  individually) and equivalents for dynamic-field rows and copy-field pairs,
  precisely because the integration tests, as first written, could pass
  without the flags/pairs actually being column-bound.
- **`templates/schema.html`** (new): three sections — a `Fields` table
  (always rendered, no empty state), and `Dynamic fields`/`Copy fields`
  tables that each fall back to a `<p class="none">none declared</p>` when
  empty.
- **`src/lib.rs`**: new `GET /ui/schema` route + `schema_ui` handler, reading
  `state.index.wf_schema.{fields,dynamic_fields,copy_fields}` directly — the
  same struct `schema::check_compatible` validates against at startup — and
  passing them straight to `render_schema_page`. A render `Err` (only
  reachable via a compile-time-checked template) surfaces as a plain 500,
  matching `/ui`'s and `/ui/query`'s existing error convention.
- **`tests/admin_ui_schema_view.rs`** (new, 12 tests, written red-first): 200
  HTML response; every field name and type rendered; the
  stored/fast/multi_valued/required flag reflected per field (four tests,
  one per flag); the fields table's `<thead>` labels bound to the correct
  column order; dynamic-field patterns and types rendered near their own row;
  copy-field source/dest pairs rendered as adjacent cells inside the
  copy-fields section specifically; no form or mutation affordance on the
  page; idempotence across repeated GETs; and the no-reparse guarantee —
  deletes the on-disk `schema.toml` after the app is built and asserts the
  page still renders the full, correct schema, which is only possible if the
  handler serves the in-process struct.

## Deliberate descopes

No new `ponytail:`-marked simplification in this diff (checked directly;
the only `ponytail:` comment in `src/admin_ui.rs` is #94's pre-existing
`human_size()` note, untouched here). Nothing from issue #128's acceptance
criteria was left out — the three PRD-named sections (fields, dynamic
fields, copy fields) are all rendered, all read-only, no create/edit/delete
affordance, matching #94's and #127's no-mutation posture. The `disk_size_
bytes` follow-up flagged in #94's report is about index-stats, not schema,
and is not touched by this change.

## Pipeline (1 implementor round, self-repaired before review; 1 reviewer
round; 1 follow-up test-writer-only pass)

1. **test-writer** wrote `tests/admin_ui_schema_view.rs` (11 tests at this
   point — the header-label test did not yet exist), confirmed red on a 404
   (the route did not exist yet) before any implementation.
2. **implementor** built `templates/schema.html`, the `SchemaPage`/
   `render_schema_page` additions in `src/admin_ui.rs`, and the `/ui/schema`
   route in `src/lib.rs`. Got the full suite green (576 passed at that
   point). Rather than stopping there, ran its own mutation pass against the
   11 integration tests and found 5 vacuous: hardcoding every flag cell to
   `"no"` still passed all four `schema_page_reflects_the_*_flag_per_field`
   tests, because they compared row slices starting at the differing field
   *name* — the assertion was already satisfied before any flag cell was
   reached. It self-reported this rather than silently shipping it, and
   closed the gap on the production-code side with 5 new unit tests in
   `src/admin_ui.rs` (column-indexed, not row-comparison), leaving the
   integration test file itself untouched per the "implementor does not edit
   disputed tests" rule.
3. **Reviewer (Opus, round 1)** — independently re-verified the vacuity claim
   by mutation rather than trusting the report: reproduced both the
   hardcoded-flag mutation and a second one of its own (deleting the entire
   copy-fields section from the template, which still passed
   `schema_page_renders_copy_field_source_dest_pairs` — a ~200-char
   proximity check on the whole page body, satisfiable by the unrelated,
   adjacent `body`/`body_copy` `[[fields]]` rows). Confirmed the
   implementor's 5 new unit tests caught both. Found one further gap of its
   own that the implementor had not: swapping the `<th>Stored</th>` and
   `<th>Required</th>` header labels, with cell data left untouched, passed
   the entire suite — nothing bound header text to column position.
   Approved the production diff as-is (quality, style, no-reparse
   guarantee, read-only-ness, HTML escaping all held up), but flagged the 5
   now-repaired-at-the-unit-level-but-still-vacuous integration tests plus
   the header/column-binding gap as must-fix before #128 is considered fully
   closed.
4. **Orchestrator** dispatched a follow-up test-writer-only pass (not a new
   pipeline issue, no production code touched) against
   `tests/admin_ui_schema_view.rs` in place: rewrote the four flag tests to
   extract the specific flag's cell via column-indexed lookup
   (`nth_cell(row, FIELD_STORED_COL)` etc.) instead of whole-row inequality;
   rewrote the copy-field test to scope its assertion to the copy-fields
   section specifically (`copy_fields_section()` + `has_adjacent_td_pair()`)
   rather than proximity across the whole page; and added
   `schema_page_fields_table_header_labels_match_their_columns`, asserting
   the `<thead>` labels equal `["Field", "Type", "Stored", "Fast",
   "Multi-valued", "Required"]` in order. Each fix was verified against the
   exact reviewer-named mutation on a scratch copy of the template (red),
   then reverted (green), before being folded into the committed test file.
   Final: 12 tests, all naming the mutation each one closes in a doc comment
   above it (verified directly by this reporter — see below).

Per this repo's CLAUDE.md, review took one round here (not the two-round
cap), and the leftover it raised was fully closed by the follow-up
test-writer pass rather than escalating unresolved — there is no
must-fix item carried forward from this pipeline.

## Test evidence

Re-run directly by this reporter against the current committed tree, not
copied from an earlier stage's claim:

```
cargo fmt --check                                       -> clean (exit 0)
cargo clippy --all-targets --quiet -- -D warnings        -> No issues found (exit 0)
cargo test --quiet                                       -> 577 passed (29 suites, 35.20s)
cargo test --quiet --test admin_ui_schema_view           -> 12 passed (1 suite, 0.13s)
```

The 12 tests in `tests/admin_ui_schema_view.rs` (confirmed present by direct
read): `schema_page_returns_200_html_for_the_running_core`,
`schema_page_lists_every_declared_field_name_and_type`,
`schema_page_reflects_the_stored_flag_per_field`,
`schema_page_reflects_the_fast_flag_per_field`,
`schema_page_reflects_the_required_flag_per_field`,
`schema_page_reflects_the_multi_valued_flag_per_field`,
`schema_page_fields_table_header_labels_match_their_columns`,
`schema_page_renders_dynamic_field_patterns_and_types`,
`schema_page_renders_copy_field_source_dest_pairs`,
`schema_page_has_no_form_or_mutation_affordance`,
`schema_page_is_idempotent_and_does_not_mutate_the_index`,
`schema_page_does_not_reread_the_schema_file_from_disk`.

`src/admin_ui.rs` also gained unit tests exercising the column-binding
directly (counted in the 577 total, not a separate suite):
`schema_page_renders_each_field_flag_in_its_own_column` and its equivalents
for dynamic-field rows and copy-field pairs.

## Mutation evidence

- **Hardcoded flag cells (`"no"` for every field)** — caught by the round-1
  implementor's new unit test; also independently re-applied and confirmed
  caught by the round-1 reviewer.
- **Deleted copy-fields section from the template** — independently applied
  by the round-1 reviewer; caught by the implementor's new unit test.
- **Swapped `Stored`/`Required` `<th>` labels, cell data untouched** — found
  by the round-1 reviewer; the pre-repair integration suite did not catch
  it. Closed by the follow-up test-writer's new
  `schema_page_fields_table_header_labels_match_their_columns`, verified
  red-then-green against this exact mutation before being committed.
- **Whole-row-inequality flag tests, whole-page-proximity copy-field test**
  (the two vacuity classes the review round targeted) — the follow-up
  test-writer applied the reviewer's named mutations to a scratch copy of
  the template for each rewritten assertion, confirmed red, reverted,
  confirmed green, before folding the rewrite into the committed file. This
  reporter did not re-run that red/green cycle a third time, but did confirm
  by direct read that the committed assertions are column-indexed
  (`nth_cell`) and section-scoped (`copy_fields_section` +
  `has_adjacent_td_pair`) rather than the whole-row/whole-page forms the
  reviewer flagged.

## Review outcome

Approved. One reviewer round (Opus), which bounced the diff on the
integration-test file rather than the production code — the production
diff (`src/admin_ui.rs`, `src/lib.rs`, `templates/schema.html`) was accepted
as-is in round 1. The three items round 1 raised (two implementor-side
vacuous tests plus the header-label gap the reviewer found itself) were all
closed by a single follow-up test-writer-only pass, with no production code
touched and no item deferred past this pipeline. Unlike #94, this did not
consume the two-round cap, and unlike #127, nothing was left non-blocking —
this pipeline closes clean.

## Follow-ups

1. **No navigation between `/ui`, `/ui/query`, and `/ui/schema`.** Confirmed
   by direct read: none of `templates/core.html`, `templates/query.html`, or
   `templates/schema.html` contain an `<a href=...>` anywhere. This is a
   pre-existing gap across all three admin-UI pages, not introduced by
   #128 — worth a small, separate follow-up (a shared nav partial) rather
   than folding into any one page's issue.
2. **The fields table has no "none declared" empty state**, unlike the
   dynamic-fields and copy-fields tables (`templates/schema.html` renders
   `{% if dynamic_fields.is_empty() %}`/`{% if copy_fields.is_empty() %}`
   branches, but the fields table is unconditional). Currently unreachable
   in practice — `schema::load` requires a unique-key field, so a schema
   with zero `[[fields]]` entries cannot exist — so this was left as-is
   rather than adding dead code for an unreachable state. Worth revisiting
   only if that invariant ever changes.
3. Per this repo's CLAUDE.md ("if the reviewer capped out at 2 rounds, the
   report must state the work could use more review passes") — that
   condition does not apply here: this pipeline used one review round, not
   two, and closed with no outstanding must-fix. No further review pass is
   owed on that basis. The one open item worth a future pass, if capacity
   allows, is the same one #94's and #127's reports both still list open:
   nav between the three admin-UI pages (item 1 above).
