# Issue #129 — v2.5: index stats — doc/segment count, on-disk size, uptime

Worktree: `/Users/mark/Projects/wayfinder-129-index-stats`
Branch: `129-admin-ui-index-stats`

Delivers the fourth page of PRD §5 "v2.5 — Admin web UI," following the
tracer bullet in #94, the query tester in #127, and the schema view in
#128: `GET /ui/stats`, a read-only page showing the running core's doc
count, segment count, on-disk size, and process uptime, plus a resident-
memory honesty line restating the mmap/no-JVM-heap precedent PRD §6 already
set for the absent heap-tuning knob.

## What was built

- **`src/admin_ui.rs`**: `StatsPage<'a>` — an `askama::Template` deriving
  from `templates/stats.html` — plus `render_stats_page(core_name,
  doc_count, segment_count, size_bytes, uptime: Duration) -> Result<String,
  askama::Error>`, and `human_duration(total_secs: u64) -> String`, a
  `ponytail:`-marked cosmetic formatter (`3723` -> `"1h 2m 3s"`, whole
  seconds, largest-unit-first, no locale awareness, no units above days;
  the exact second count is rendered alongside it, mirroring the
  exact-plus-readable pairing `templates/core.html` already uses for size,
  so nothing downstream parses the human string). Five new unit tests:
  `human_duration_counts_up_from_seconds` (boundary values including a
  multi-day duration), `stats_page_renders_every_figure_it_is_given`,
  `stats_page_reports_no_resident_memory_figure` (pinning "mmap" present
  and no byte-unit substring near "resident"), `stats_page_has_no_form`,
  `stats_page_escapes_the_core_name`.
- **`templates/stats.html`** (new): a table with Documents/Segments/Size on
  disk/Process uptime rows (size and uptime each pair an exact figure with
  a human-readable one, same convention as `templates/core.html`), followed
  by a "Memory" section stating in prose that Wayfinder is mmap-based and
  reports no resident-memory figure.
- **`src/core_index.rs`**: new `CoreIndex::segment_count()`, reading
  `self.reader.searcher().segment_readers().len()` — the same searcher
  `doc_count()` already reads from, so it cannot drift from what the query
  pipeline sees. New unit test `segment_count_tracks_a_multi_segment_index`,
  built specifically to close a gap the implementor self-reported (see
  Pipeline below): builds a genuinely multi-segment index
  (`merge_policy = "no_merge"`, one commit per doc, 3 commits), asserting
  `0` before any commit and comparing against a fresh, independent
  `tantivy::Index::open_in_dir` read after every commit, ending with an
  explicit `> 1` assertion. The existing `disk_size_bytes()` doc comment was
  rewritten to record the caching decision (see Deliberate descopes).
- **`src/lib.rs`**: `AppState` gained `started_at: Instant`, set once in
  `build()` — the only piece of admin-UI state not derivable from the index
  itself. New `GET /ui/stats` route + `stats_ui` handler, reading
  `doc_count()`, `segment_count()`, `disk_size_bytes()` off the live
  `CoreIndex` and `started_at.elapsed()` for uptime; a render `Err`
  surfaces as a plain 500, matching `/ui`'s and `/ui/schema`'s existing
  convention.
- **`tests/admin_ui_index_stats.rs`** (new, 7 tests, written red-first):
  200 HTML response with the core's name present; the real doc count
  rendered (checked with `contains_standalone_number` against the known
  5-doc fixture); the real segment count rendered, checked against an
  independent oracle (`tantivy::Index::open_in_dir` opened fresh on the
  same data dir, never through any Wayfinder type); the real on-disk size
  rendered as an exact `(<N> bytes)` figure, checked against an independent
  iterative directory-walk oracle; uptime rendered as a number that
  strictly increases across two renders ~1.2s apart; a resident-memory
  honesty line containing both "mmap" and "resident" with no byte-unit
  substring ("kb"/"mb"/"gb"/" b)"/" bytes") in the window around
  "resident"; and a read-only guarantee (no `<form>`, and hitting the page
  twice does not change `/select`'s `numFound`).

## Deliberate descopes

- **`disk_size_bytes()` stays uncached** — this is issue #129's own
  acceptance criterion ("decide whether to keep it as-is or add caching,
  and record the decision either way"), and the decision made here is
  *keep it uncached*, now recorded as a real code comment in
  `src/core_index.rs` (lines 2298-2309), not just in chat: both callers
  (`/ui`, `/ui/stats`) are human-paced, single-click admin pages; there is
  no metrics endpoint or auto-refresh consumer yet; and a cache would need
  a genuinely hard invalidation rule, since the data directory changes on
  background merges and segment deletes, not only on commits — a stale
  cached figure would be worse than an untimed-but-correct walk. The
  revisit trigger is named explicitly: arrival of a polled consumer
  (a metrics endpoint or a self-refreshing page).
- **No resident-memory figure**, by design, mirroring the same PRD §6
  honesty precedent #94's and this issue's acceptance criteria both name
  for the absent heap-tuning knob: the page states in prose that Wayfinder
  is mmap-based and has no JVM-heap-shaped number to report, rather than
  fabricating one. Both the unit test (`stats_page_reports_no_resident_
  memory_figure`) and the integration test (`stats_page_states_mmap_
  honesty_for_resident_memory_with_no_fabricated_number`) pin this
  negatively — no digit-plus-byte-unit pattern near the word "resident" —
  not just that the word "mmap" appears somewhere on the page.
- No new stats-collection subsystem, matching the issue's explicit
  criterion: doc count, segment count, and on-disk size are all read
  directly off the live `CoreIndex`/searcher at request time; uptime is a
  single `Instant` captured once at process start. Confirmed directly by
  reading `src/lib.rs` and `src/admin_ui.rs`: no background thread, no
  polling, no cache.
- No create/edit/delete affordance, no auth — consistent with #94's,
  #127's, and #128's descope list; not re-litigated here.

## Pipeline (1 round — the first of the four v2.5 pages so far to approve
clean on round 1)

1. **test-writer** wrote `tests/admin_ui_index_stats.rs` (7 tests),
   confirmed red on a 404/missing route before any implementation existed.
2. **implementor** built `templates/stats.html`, the `StatsPage`/
   `render_stats_page`/`human_duration` additions in `src/admin_ui.rs`, the
   `segment_count()` addition in `src/core_index.rs`, and the
   `AppState.started_at` + `/ui/stats` route in `src/lib.rs`. Got the full
   suite green (590 passed). Rather than stopping there, self-reported a
   gap in its own coverage before it reached review: the integration test
   `stats_page_shows_the_real_segment_count` compares against a real
   independent oracle, but the shared fixture (`indexed_app()`, a single
   commit of 5 docs) produces exactly one segment — so a `segment_count()`
   hardcoded to return `1` would still pass that integration test
   unnoticed. Did not edit the integration test (per the "implementor does
   not edit disputed tests" rule); instead closed the gap on the
   production-code side with a new unit test in `src/core_index.rs`
   (`segment_count_tracks_a_multi_segment_index`, described above under
   "What was built") that builds a genuinely multi-segment index and would
   catch a hardcoded value.
3. **Reviewer (Opus, round 1) — approved.** Independently re-verified
   rather than trusting the self-report:
   - Hardcoded `segment_count()` to `1` in a scratch copy: confirmed the
     integration test still passed 7/7 (the gap is real, as claimed) while
     the new unit test failed on its pre-commit `0` assertion (the gap is
     closed, as claimed).
   - Judged this correctly was *not* a case calling for a further
     test-writer round: a black-box `/ui/stats` test can only observe two
     real segments by issuing two commits under the default merge policy,
     where an async merge could nondeterministically collapse them back to
     one — the deterministic `no_merge`-configured unit test is the
     *better* test here, not a workaround, and `tests/common/mod.rs` has no
     config-parameterized app helper today that would let a black-box test
     force multi-segment state flake-free.
   - Independently mutated the honesty line to a fabricated figure and
     confirmed both the unit test and the integration test catch it.
   - Confirmed the rendered prose contains no "mb"/"kb"/"gb"/"bytes"
     substring near "resident" as currently worded, while noting the scan
     itself is fragile in principle (see Follow-ups).
   - Mutated uptime to a constant and confirmed the integration test
     catches it.
   - Confirmed the `disk_size_bytes()` decision is recorded as a real code
     comment in `src/core_index.rs` (verified by this reporter at lines
     2298-2309, not just cited) and judged the reasoning sound.
   - Confirmed read-only: no `<form>`, `/select`'s `numFound` unchanged
     after hitting the stats page twice.
   - Confirmed no new stats-collection subsystem: no background collector
     thread; all four figures are per-request reads.
   - Ran the full gate itself: fmt/clippy/test all clean, 590 passed.

Per this repo's CLAUDE.md convention of flagging review-round count
honestly: this is the first of the three v2.5 milestones landed so far
(tracer bullet #94: 3 rounds; query tester #127: not covered by this
report but referenced by the reviewer as "non-blocking" precedent; schema
view #128: 1 implementor round self-repaired plus 1 reviewer round plus a
follow-up test-writer-only pass) to approve clean on round 1 with nothing
carried forward and no follow-up pass needed to close a gap. That is a
genuine first for this batch of pages, not a claim that the page needed no
scrutiny — see Follow-ups for what the reviewer still flagged as
non-blocking.

## Test evidence

Re-run directly by this reporter against the current committed tree, not
copied from an earlier stage's claim:

```
cargo fmt --check                                      -> clean (exit 0)
cargo clippy --all-targets --quiet -- -D warnings       -> No issues found (exit 0)
cargo test --quiet                                      -> 590 passed (30 suites, 38.17s)
cargo test --quiet --test admin_ui_index_stats          -> 7 passed (1 suite, 1.56s)
```

The 7 tests in `tests/admin_ui_index_stats.rs` (confirmed present by direct
read): `stats_page_returns_200_html_for_a_populated_core`,
`stats_page_shows_the_real_doc_count`,
`stats_page_shows_the_real_segment_count`,
`stats_page_shows_the_real_on_disk_size`,
`stats_page_shows_an_uptime_that_does_not_decrease`,
`stats_page_states_mmap_honesty_for_resident_memory_with_no_fabricated_number`,
`stats_page_is_read_only_with_no_form_and_does_not_mutate_the_core`.

`src/admin_ui.rs` also gained 5 unit tests and `src/core_index.rs` gained 1
unit test (counted in the 590 total, not a separate suite):
`human_duration_counts_up_from_seconds`,
`stats_page_renders_every_figure_it_is_given`,
`stats_page_reports_no_resident_memory_figure`, `stats_page_has_no_form`,
`stats_page_escapes_the_core_name`, and
`segment_count_tracks_a_multi_segment_index`.

## Mutation evidence

- **`segment_count()` hardcoded to `1`** — reviewer-applied on a scratch
  copy: the black-box integration test still passed (confirming the gap
  the implementor self-reported was real); the new
  `segment_count_tracks_a_multi_segment_index` unit test failed at its
  pre-commit `0` assertion (confirming the gap is closed at the unit
  level).
- **Honesty line mutated to a fabricated figure** — caught by both the
  unit test (`stats_page_reports_no_resident_memory_figure`) and the
  integration test (`stats_page_states_mmap_honesty_for_resident_memory_
  with_no_fabricated_number`).
- **Uptime mutated to a constant** — caught by
  `stats_page_shows_an_uptime_that_does_not_decrease`'s strict `second >
  first` assertion.

## Review outcome

Approved, round 1 (Opus). No must-fix items. This is the first of the
four v2.5 admin-UI pages so far to close on a single review round with no
carried-forward gap and no follow-up test-writer pass required — a
contrast worth naming plainly against #94 (3 rounds to close) and #128
(1 reviewer round plus a required follow-up test-writer-only pass to
close two vacuity classes and a header/column-binding gap). That said, one
review round is still one round, not the two-round cap being exhausted in
this pipeline's favor: the reviewer's own follow-up items below are
non-blocking precisely because a single Opus pass judged them so, not
because a second independent pass confirmed there was nothing further to
find.

## Follow-ups

1. **The honesty-line guard scans for the substring "mb" case-
   insensitively**, which would false-positive on ordinary prose words like
   "number" or "remember" if the resident-memory line is ever reworded.
   Suggested tightening: require a digit immediately before the unit
   (e.g. `\d(kb|mb|gb)`-shaped) rather than a bare substring match. Applies
   to both the unit test in `src/admin_ui.rs` and the integration test in
   `tests/admin_ui_index_stats.rs`.
2. **`tests/common/mod.rs` has no config-parameterized app helper** —
   `indexed_app()` is the only fixture builder, with no variant accepting a
   merge policy or other `ServerConfig` override. If a future issue wants
   genuine black-box multi-segment coverage (rather than the unit-test-level
   coverage this pipeline added), add an additive
   `indexed_app_with_config(...)` alongside the existing helper rather than
   changing `indexed_app()`'s signature underneath every other test file
   that calls it.
3. **None of the four admin-UI pages (`/ui`, `/ui/schema`, `/ui/query`,
   `/ui/stats`) link to each other.** Not a defect introduced here — #94's
   and #128's reports both already flag this as pre-existing across the
   earlier pages, and it remains true with a fourth page added. Worth its
   own small issue (a shared nav partial) now that there are four pages
   with zero cross-navigation between them, rather than folding into any
   one page's issue.
