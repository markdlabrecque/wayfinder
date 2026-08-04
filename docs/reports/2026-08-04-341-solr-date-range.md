# Issue #341 -- `solr.DateRangeField`, server half

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-341-solr-date-range`.
**Spec:** findings **165-172** in `docs/solr-ref-findings.md` (captured from a
real `solr:9` core running `solr.DateRangeField`), plus the ratified scope
decisions below.

`search_api_solr`'s `solr_date_range` type was the one non-default data type
#300 explicitly could not add (`docs/reports/2026-08-08-300-non-default-data-types.md:117`):
Wayfinder's `date` field holds a single instant, and a date-range type needs a
server-side start/end interval with Solr's range-query semantics over it.
This issue builds that field type. It is the server half only; the Drupal
half (`drupal/search_api_wayfinder/**`) is a deliberate second pass -- the
issue itself says "Server half first, Drupal half after."

## Scope delivered

A new `date_range` field type, reported as class `wayfinder.DateRangeField`
(pinned in `tests/schema_fieldtypes.rs`'s `EXPECTED_CLASSES`), with:

- `Intersects` (default when `op` is absent), `Contains`, `Within`, and
  `IsWithin` (an alias of `Within`) interval predicates via
  `{!field f=<field> op=<op>}<interval>` local params, case-insensitive `op`.
- Millisecond-precision, end-inclusive literal expansion: `2020` denotes
  `[2020-01-01T00:00:00.000Z, 2020-12-31T23:59:59.999Z]`, and this applies to
  interval endpoints too (`[2020-03 TO 2020-09]` ends at
  `2020-09-30T23:59:59.999Z`).
- multiValued union semantics: a multiValued field is one point set, the
  union of its members including holes, and `Intersects`/`Contains` are
  hole-sensitive against the doc's actual member-by-member interval set
  (not "any member matches"), while `Within` reduces to
  `min(start) >= qStart AND max(end) <= qEnd`.
- `NOW` date math (`NOW/DAY`, `NOW/DAY+1MONTH`, `NOW-2YEARS`,
  `NOW/YEAR+1YEAR`, etc.), alongside truncated-date literals.
- The 400/500 error split by failure kind: unparseable value is 400
  (`Couldn't parse date because: ...`, `Invalid Date Math String:'...'`);
  valid-but-unimplemented op or a reversed interval is 500 (`Unknown
  Operation: ...`, `Wrong order: ... TO ...`).
- Exclusive-brace syntax (`{a TO b}`) accepted and silently treated as
  identical to `[a TO b]`.
- Values round-trip verbatim: `"2020"` comes back as `"2020"`, never
  normalised to a full instant.
- Interval querying on **both** declared static fields (two synthetic
  `<name>__start`/`<name>__end` date fast columns, the same shape as
  `ResolvedType::Location`'s `__lat`/`__lon`) and dynamic `drs_*`/`drm_*`
  fields (nested JSON sub-paths `_dynamic.<name>.start`/`.end` inside the
  existing `_dynamic` catch-all).
- `facet.field` on a `date_range` field returns HTTP 200 with an empty
  bucket list; `sort` and `stats.field` on one both 400 with Solr's exact
  messages (finding 186).

### Files touched (production)

`src/date_range.rs` (new, ~1050 lines across all commits), `src/schema.rs`,
`src/core_index.rs`, `src/facet.rs`, `src/stats.rs`, `src/collector.rs`,
`src/lib.rs`, `presets/search-api.toml`.

### The Drupal half -- explicitly out of scope, file-and-line inventory for the next pass

- `drupal/search_api_wayfinder/src/FieldMapper.php` -- `'solr_date_range' =>
  'dr'` in `TYPE_PREFIXES` (`:47-62`), a `formatValue()` branch (`:123-145`)
  producing `[start TO end]`, `filterValue()` (`:152-168`) leaving it bare.
  The descope note in the class docblock (`:34-37`) needs removing.
- `DocumentBuilder.php` -- the interval-shape indexing branch.
- `Plugin/search_api/backend/WayfinderBackend.php` -- add `solr_date_range`
  to the `$supported` allow-list (`:246`); delete the descope comment naming
  it (`:239-241`).
- `README.md:121-123`, and the four PHPUnit test files.

## The two ratified user scope decisions

Both were settled by the user during this work (both recorded in the now-
deleted `SPEC-341.md`, preserved here so they survive):

1. **Query reach: both static and dynamic.** A declared static `date_range`
   field gets the two synthetic `__start`/`__end` columns; a dynamic
   `drs_*`/`drm_*` field is *also* interval-queryable, via nested JSON
   sub-paths in `_dynamic`. This was not the minimal option (static-only
   would have covered less code) but was chosen explicitly.
2. **Date syntax: truncated dates *and* date math**, i.e. both `2020-06-15`
   forms and `NOW-2YEARS`/`NOW/YEAR+1YEAR` forms. This is a larger surface
   than was recommended -- the user was shown the cost (date math is the
   largest part of the surface with the thinnest fixture coverage, only two
   fixtures pin it directly) and chose it anyway. Settled, not reopened.

## A corrected ticket premise

Issue #341 describes the interval predicates as what "the Search API
date-range type relies on." Grepping `search_api_solr` 4.4.0 for
`Intersects|Contains|Within|op=` returns nothing: the Drupal client only ever
emits the default `Intersects` query shape (a bare `[start TO end]`), never
sets `op`. All three ops (`Contains`, `Within`, `IsWithin`) were still built,
because the issue named them as explicit scope for the server-side type --
but the correction for whoever plans the Drupal half is that its actual reach
into this surface is narrower than the ticket implies.

## Test evidence

Final gate, verified independently twice by the orchestrator (per the
standing convention that a single green run once hid a 1-in-5 flake):

```
cargo test --no-fail-fast              # 1387 passed, 65 suites, 0 failed (both runs)
cargo fmt --check                      # clean
cargo clippy --all-targets -- -D warnings   # clean (CI's exact command)
```

Ground truth: 36 `dr341_*` fixtures captured from a real `solr:9` core
(`solr-ref/capture.sh`'s `#341` block, `solr-ref/responses/dr341_*.json`),
indexed in `solr-ref/manifest-errors.tsv`. Roughly 107 tests directly exercise
this feature: 59 in `tests/date_range.rs` (static path) and 48 in
`tests/date_range_dynamic.rs` (dynamic path), plus additions to
`tests/schema_fieldtypes.rs` and `tests/search_api_preset.rs`, and a
`tests/differential.rs` wiring block for the manifest-errors runner.

One fixture, `dr341_fieldtypes` (a whole-endpoint `GET /schema/fieldtypes`),
can never byte-for-byte match: Solr returns its entire `_default` configset's
field types, Wayfinder reports its own builtin list -- the same permanent
self-description category as `admin_info_system`. It is listed in
`EXPECTED_DIVERGENCES_MANIFEST_ERRORS` (not `ACCEPTED_DIVERGENCES`), so the
differ still runs against it and it still counts toward `diffed`; the row
self-expires if it ever starts matching. The one fact the fixture exists to
prove -- that solr:9 declares `date_range`/`date_ranges` as
`solr.DateRangeField` -- is asserted for real in `tests/schema_fieldtypes.rs`.

## Review outcome

Four review rounds. Each found real defects in a suite the previous round
had left green -- this is the most load-bearing part of the record, not a
formality.

- **Round 1** (against `d5eb872`) bounced three must-fix items: a
  `rest.split_at(1)` panic on a non-ASCII byte following `NOW` in
  `src/date_range.rs:393`, firing while the index-writer lock was held and
  bricking all further writes to the core (should have been a 400 per
  finding 184); `try_date_range` (`src/core_index.rs`) being a
  whole-query-string special case, so any non-bare clause was either a
  spurious 400 (`AND`/`OR`) or a *silently wrong* term query against the
  raw verbatim-string field (`(...)`, `+`/`-`, edismax, `df`, dynamic
  `fq=+dys_*`); and a `ponytail:` comment on `MIN_MS`/`MAX_MS`
  (`src/date_range.rs:36-48`) that claimed a clamp the code did not
  actually do. It cleared the multiValued union pairing (across forced
  6-segment merges, reversed insertion order, 3-member/2-hole docs),
  millisecond end-inclusivity, the 400/500 split, brace equivalence, and
  synthetic-endpoint leakage.
- **Round 2** (against `b0c2898`/`0bdabe2`, which fixed round 1) bounced
  three more, and named the pattern behind them explicitly: **the round-1
  fixes had closed the named instance, not the class.** The write-poisoning
  panic was still live via arithmetic overflow -- `time`'s
  `Duration::days`/`weeks`/`hours`/`minutes` are `checked_mul(..).expect(..)`
  internally, so they panic in release builds too, and
  `NOW+9223372036854775807DAYS` reproduced the same bricked core. The new
  `MIN_MS`/`MAX_MS` clamp guarded only the low side, so every literal from
  2263 to 9998 produced an inverted interval that reported `Wrong order` for
  a correctly-ordered query and rejected documents Solr accepts (`9999`
  escaped by luck, which is exactly why round 2's own test missed the class).
  And the silently-wrong term query survived in the edismax `qf` path, which
  never reaches `build_leaf`.
- **Round 3** (`d00870c`) was done by the orchestrator directly -- the
  2-round reviewer/implementor cap for that round had been spent -- and
  fixed all three as classes rather than instances: every fixed-length
  date-math unit routed through `Duration::milliseconds` (the one
  constructor that divides rather than multiplies) plus an explicit
  `checked_mul`; the `MONTHS` arm through `checked_add`; the clamp made
  symmetric (`.clamp(start_ms, MAX_MS)`); and the `qf` disjunction taught to
  build the real interval query instead of a term query.
- **Round 4** (`3e32952`) reviewed round 3 alone and found that the `qf` fix
  had introduced a regression: a whole request now 400d as soon as one `qf`
  target was a `date_range` field and the query literal was not a parseable
  date (e.g. `qf=title drs_x&q=hello` went from returning hits to a 400).
  This contradicted a ceiling issue **#84** had already ratified for other
  typed `qf` fields -- `int` and `date` fields silently drop their disjunct
  so the text fields alongside can still answer -- so `date_range` was made
  to match that existing behaviour. Round 4 also caught a comment claiming
  `qf`/`pf` coverage while `build_pf_query` remained untouched (the round-1
  defect class recurring verbatim), and that the symmetric clamp had an
  undocumented false negative: when *both* endpoints land past `MAX_MS` they
  collapse to the same clamped value, so `[9999 TO 2263]` silently stops
  being the `Wrong order` 500 it should be. Both are now fixed and pinned by
  tests.

The lesson, stated plainly because it is the reusable part: **a green suite
plus a fixed named row is not a fixed defect class.** Three of the four
rounds found something the prior green run had missed, and the independent
reviewer earned its keep every time. Every fix in rounds 2-4 was
mutation-tested: revert the fix, confirm the named test fails for the right
reason, restore.

Note also that this pipeline spent its 2-round reviewer/implementor default
cap (round 1 -> round 2) and needed a third and fourth pass beyond it. The
work went through review four times total, more than the default, and still
surfaced a real regression on the final pass -- record that as-is rather than
implying two rounds would have sufficed here.

## Known ceilings and follow-ups

Each is marked in the code with a `ponytail:` comment naming its limit; none
of these are done, they are documented boundaries.

- **Date math anchor and rounding.** `NOW` is the only anchor supported (no
  `<literal>Z+1DAY`), and rounding is UTC-only regardless of `TZ`.
  `NOW+300YEARS` now clamps (round 3's symmetric fix) where `NOW+9999YEARS`
  still 400s on overflow before reaching the clamp -- a real inconsistency,
  worth a sentence of its own if anyone revisits date math.
- **`MIN_MS`/`MAX_MS` clamping.** `tantivy::DateTime` is i64 nanoseconds, so
  the representable range is roughly 1678..2261. Out-of-range endpoints
  clamp rather than 400 -- deliberate, because `9999-12-31T23:59:59Z` is the
  standard Solr/Search API open-ended sentinel. Consequence: two distinct
  far-future instants can compare equal, and a reversed far-future interval
  (both endpoints past `MAX_MS`) collapses instead of erroring, per round
  4's finding.
- `pf` naming a `date_range` field contributes no phrase boost (harmless,
  not wrong hits, but unimplemented).
- `json.facet` `type:terms`, `facet.interval`, and `facet.range` on a
  `date_range` field are unpinned by any fixture; `facet.range` currently
  400s with a generic message rather than a bespoke one.
- The 400 message for a wildcard on a `date_range` field
  (`src/core_index.rs`) is an invented string, not captured from Solr.
- The value/index-path half of the mixed-brace argument (whether
  `{a TO b}` behaves identically on the *indexing* side, not just the query
  side) is inference, not capture; the query-path half was verified
  directly against the grammar.
- **A bare full-instant literal is unqueryable on the query path**:
  `drs_x:2020-06-15T12:00:00Z` is a 400 from tantivy's grammar before
  `build_leaf` ever sees it, even though a document indexed with exactly
  that value exists and both `drs_x:[a TO b]` and `{!field}` work against
  it. This is pre-existing and **not a Solr divergence** -- Solr's own
  classic parser needs the colons escaped too
  (`2020-06-15T12\:00\:00Z`), which is why no capture ever sends the bare
  form. Worth its own issue if it needs closing.
- `defType=dismax` is not implemented anywhere in this repo (only
  `edismax` is, `src/lib.rs:3213`), so a `dismax` request answers `q`
  against `df` regardless of field type. Pre-existing, unrelated to #341,
  and the reason a `dismax` row was deliberately left out of this issue's
  tests.
- `dr341_fieldtypes` is listed in `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`
  (see Test evidence above) -- a permanent self-description category, not
  an unbuilt feature, and it self-expires if it ever starts matching.

## The verified tantivy premise

Probed empirically before implementation (not assumed), because it is what
makes the dynamic path possible at all: tantivy 0.26.1's JSON
array-of-objects flattening preserves per-member ordinal pairing between
`_dynamic.<name>.start` and `_dynamic.<name>.end` in document insertion
order, deliberately *not* sorted -- so the hole-sensitive `Intersects`/
`Contains` predicates can be evaluated member-by-member on the dynamic path
exactly as on the static path. Also established: RFC3339 strings inside a
JSON object are auto-detected as dates and become `Column<tantivy::DateTime>`,
**not** `Column<i64>`, so `FieldColumns::open` (which at the time probed only
`i64`/`f64`) would have silently returned `Missing` for them had this not
been checked and the opener extended.

## A captured-Solr correction to a working model

During development, `dr341_multi_within` was predicted to match d8 (whose
`2020` member fits the query window). Real captured Solr returned 0 hits.
The actual rule: operations are set relations against the **union** of a
document's intervals, so `Within` requires *every* member to fit inside the
query window, not just one. A ninth corpus document (d9, a single-member
field that does fit) was added specifically so the fixtures can falsify both
the wrong "any member" reading and a pairing-blind reading at once.

## Commands

```
cargo test --no-fail-fast                    # 1387 passed / 65 suites / 0 failed
cargo fmt --check                            # clean
cargo clippy --all-targets -- -D warnings    # clean
```
