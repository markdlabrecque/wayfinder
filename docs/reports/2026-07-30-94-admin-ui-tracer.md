# Issue #94 — v2.5 admin-UI tracer bullet

Worktree: `/Users/mark/Projects/wayfinder-94-admin-ui`
Branch: `94-admin-ui-tracer`

Delivers the first vertical slice of PRD §5 "v2.5 — Admin web UI": one
server-rendered page, `GET /ui`, showing the single core this Wayfinder
process serves.

## What was built

- **`Cargo.toml`**: new dependency `askama = "0.16.0"` (and its transitive
  additions in `Cargo.lock`).
- **`src/admin_ui.rs`** (new): `CorePage<'a>` — an `askama::Template` deriving
  from `templates/core.html` — plus `render_core_page(core_name, doc_count,
  size_bytes) -> Result<String, askama::Error>`, and `human_size()`, a
  `ponytail:`-marked cosmetic formatter (`1536` -> `"1.5 KB"`, one decimal
  place, binary 1024 steps, no locale awareness; the exact byte count is
  rendered alongside it, so nothing downstream parses the human string). Six
  unit tests cover byte/KB/MB boundaries, full-render content, and HTML
  escaping of the core name (`<script>` -> `&#60;script&#62;`, confirming
  askama's default auto-escaping is in effect).
- **`templates/core.html`** (new): the askama template — core name, doc
  count, and both `{{ size_human }}` and an exact `({{ size_bytes }} bytes)`
  figure.
- **`src/core_index.rs`**: `CoreIndex` gained `data_dir: PathBuf`. New
  `doc_count()` reuses `searcher().num_docs()` — the same `IndexReader`
  instance every query path already reads from, so this figure cannot drift
  from `/select`'s `numFound`. New `disk_size_bytes()`, backed by a new free
  function `dir_size_bytes(dir: &Path) -> u64`: a `ponytail:`-marked
  uncached, synchronous recursive `std::fs` walk — apparent (logical) size,
  not blocks-on-disk; skips unreadable entries and entries that vanish
  mid-walk (e.g. a segment deleted by a concurrent merge) rather than
  erroring the whole page. Five new unit tests, using an independently
  hand-written oracle (`walk_size_oracle()`, a second, differently-shaped
  recursive walk) rather than re-deriving the same logic under test:
  nested-subdirectory summing, exact byte-length counting for a
  known-content file, an unreadable directory returning `0` rather than
  panicking, a symlink not double-counted (including a symlink pointing at
  itself), and `disk_size_bytes()` measuring the real core data dir
  end-to-end.
- **`src/lib.rs`**: new `GET /ui` route wired into the existing axum
  `Router`, calling `render_core_page()` with the live `CoreIndex`'s name,
  `doc_count()`, and `disk_size_bytes()`, returning `text/html`.
- **`tests/admin_ui.rs`** (new, 3 tests) and a `get_text()` addition to
  `tests/common/mod.rs` (status + headers + body string, alongside the
  existing JSON-returning `get()` helper).

## Scope correction mid-pipeline

Issue #94's original acceptance criteria and the PRD's "v2.5 — Admin web UI"
section both described a "core list" / "listing all configured cores,"
implying multi-core browsing. The test-writer stage caught that this
conflicts with the real, already-implemented architecture: `src/lib.rs`'s own
module doc confirms single-core-per-process is current reality (`app()`
takes exactly one schema/data-dir; no `CoreRegistry` type exists anywhere in
the crate), not merely something PRD open question 1 leaned toward. A test
file (`tests/admin_ui_multi_core.rs`) was written against a placeholder
`wayfinder::app_for_cores()` API that doesn't exist, and deliberately left
non-compiling rather than silently building multi-core infrastructure to
satisfy the ticket as written.

Resolution: both `docs/PRD.md`'s v2.5 section and issue #94's acceptance
criteria were corrected in place to describe a single-core *view*, not a
multi-core *list*. This landed as its own doc-only PR (**#97**,
"docs(prd): correct v2.5 wording to single-core-per-process") ahead of this
implementation PR, and the placeholder multi-core test file was deleted
rather than built toward. This is another instance of this repo's
CLAUDE.md convention — "don't paper over a wrong ticket premise" — three v1
issues have now hit the same class of problem.

## Deliberate descopes

All `ponytail:`-marked in code and/or listed in the PRD's v2.5 out-of-scope
list:

- No multi-core support (see scope correction above).
- No core create/delete/schema editing/config mutation from the browser.
- No document edit/delete from the UI.
- No auth — matches Solr's own default admin-UI posture; a deployment
  responsibility, not arbitrated here.
- No multi-instance/cluster views.
- `disk_size_bytes()` is an uncached, synchronous O(files) walk on every
  request. Acceptable at tracer-bullet scale; flagged in code and here for
  revisit when the PRD's later index-stats milestone lands, since that phase
  already plans to report doc/segment/size stats more richly.

## Pipeline (3 rounds — flagged honestly, not a failure to hide)

1. **test-writer** wrote `tests/admin_ui.rs` (3 tests), the now-deleted
   multi-core test, and the `get_text()` helper in `tests/common/mod.rs`.
2. **implementor (round 1)** built the full feature: `Cargo.toml`/askama
   dependency, `src/admin_ui.rs`, `templates/core.html`, the `CoreIndex`
   additions, and the `/ui` route. All 3 `tests/admin_ui.rs` tests plus the
   full suite went green (497 passed at that point).
3. **Reviewer, round 1 — bounced, 2 must-fix items:**
   - No test coverage for the `dir_size_bytes` walk itself — the only
     integration-level assertion was loose enough (`contains_plausible_size_
     indication`, a "size" + byte-unit-word substring check) that a
     permanently-zero implementation could still pass.
   - `src/admin_ui.rs` and `templates/` were untracked in git, which would
     have silently dropped the feature from a commit.
4. **implementor (round 2, fresh agent** — the round-1 implementor session
   was unresponsive/unreachable) fixed both: added the 5 unit tests listed
   above under "What was built" (with the independently-implemented
   `walk_size_oracle()` helper), staged the previously-untracked files, and
   mutation-tested 4 ways — zero-out `disk_size_bytes`, skip subdirectory
   recursion, follow symlinks instead of skipping them, remove the
   unreadable-directory guard — all 4 mutations were caught by the new
   tests, then reverted. Suite grew to 502 passed.
5. **Reviewer, round 2 (Opus-pinned per this repo's convention) —
   independently re-verified rather than trusting the round-1 fix report:**
   re-ran all four gates itself, spot-checked staging completeness directly
   against `git status`, read the 5 new tests, and independently
   re-applied 3 of the 4 mutations itself (not just trusting the
   implementor's claim) — all reproduced red-then-green correctly. It then
   found **one more gap**: the `/ui` handler in `src/lib.rs` could still
   pass a hardcoded `0` for the on-disk size and the full suite would stay
   green, because nothing end-to-end asserted the rendered *page* shows a
   real, non-zero size — the walk itself was now well-tested in isolation,
   but the wire from walk to rendered HTML wasn't. The reviewer called this
   the literal round-1 defect "moved up a layer," and — since a tracer
   bullet's entire value is the real end-to-end wire — treated it as still
   blocking, small as the gap was. Per the "max 2 rounds" rule this
   escalated to the orchestrator rather than consuming a third review round.
6. **implementor (round 3, narrowly scoped, test-only — `tests/admin_ui.rs`
   alone):** added `parse_rendered_size_bytes()` (same hand-rolled-scan
   style as the existing `contains_standalone_number`, no new dependency)
   and a new assertion in
   `core_list_page_renders_name_doc_count_and_size_for_a_populated_core`
   that the rendered `(<N> bytes)` figure is `> 0` — justified as never
   legitimately flaky, since an indexed core's data dir always leaves at
   least a `meta.json`/segment file on disk. Verified its own red/green:
   mutated the handler to pass a literal `0`, confirmed the new assertion
   failed, reverted, confirmed green.
7. **Orchestrator** (not a further reviewer round) independently re-ran all
   four gates from scratch and confirmed: `cargo fmt --check` clean,
   `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 502
   passed (24 suites), `cargo test --test admin_ui` 3 passed.

This round-2-to-round-3 handoff is exactly the case this repo's CLAUDE.md
flags: review took more than one round, and the leftover after round 2
escalated to the orchestrator rather than being absorbed into a third
reviewer pass. The reporter reconfirms this is not softened here — the
implementation genuinely needed three passes to get an end-to-end-honest
assertion in place.

## Test evidence

Re-run directly by this reporter against the current staged tree, not
copied from an earlier agent's claim:

```
cargo fmt --check                              -> clean
cargo clippy --all-targets --quiet -- -D warnings -> No issues found
cargo test --quiet                             -> 502 passed (24 suites, 28.00s)
cargo test --quiet --test admin_ui             -> 3 passed (1 suite, 0.16s)
```

`src/core_index.rs` gained 5 new unit tests
(`dir_size_bytes_sums_nested_subdirectories`,
`dir_size_bytes_counts_a_file_of_known_length_exactly`,
`dir_size_bytes_returns_zero_for_an_unreadable_directory`,
`dir_size_bytes_does_not_double_count_symlinks`,
`disk_size_bytes_measures_the_core_data_dir`), plus the pre-existing 6 unit
tests in `src/admin_ui.rs`.

Mutation testing performed across the pipeline, both rounds independently
reproduced rather than taken on trust:

- **Round-2 fix's 4 mutations** (zero out `disk_size_bytes`, skip subdir
  recursion, follow symlinks instead of skipping, remove the
  unreadable-directory guard) — all 4 caught by the new unit tests, all
  reverted by the implementor, then 3 of the 4 independently re-applied and
  re-verified by the round-2 reviewer itself.
- **Round-3 fix's 1 mutation** (handler passes a literal `0` for
  `size_bytes`) — self-verified red-then-green by the round-3 implementor,
  spot-checked by the orchestrator's final from-scratch gate run.

## Review outcome

Approved after round 2's one remaining blocking item was closed by the
round-3, test-only implementor pass, and the orchestrator's independent
from-scratch re-run of all four gates. Per this repo's "max 2 rounds"
convention, the round-2-to-round-3 gap escalated to the orchestrator rather
than consuming a third reviewer round — this work went through the
maximum review capacity the pipeline allows and could still use a fresh
pass focused specifically on the `/ui` route's HTTP-layer concerns (error
handling if `render_core_page()` ever returns `Err`, response headers beyond
`Content-Type`) that neither review round scoped in.

## Follow-ups

- The PRD's v2.5 follow-up milestones — schema view, index stats, query
  tester, ping/health — each get their own issue once this tracer bullet
  lands, per issue #94's own body text.
- `disk_size_bytes()`'s uncached, synchronous per-request walk (the
  `ponytail:` ceiling in `src/core_index.rs`) is worth revisiting
  specifically at the index-stats milestone, since that phase already plans
  to report doc/segment/size stats more richly and may want a cached or
  incrementally-maintained figure instead.
- No third review round was available under the "max 2 rounds" rule; a
  fresh review pass targeting the `/ui` route's HTTP-layer behavior (error
  paths, headers) beyond what rounds 1-2 covered would still be useful
  before this is considered fully hardened.
