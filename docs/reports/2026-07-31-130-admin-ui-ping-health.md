# Issue #130 — v2.5: ping/health, reusing the real `/admin/ping` handler

Worktree: `/Users/mark/Projects/wayfinder-130-ping-health`
Branch: `130-admin-ui-ping-health`

Delivers the fifth and last page of PRD §5 "v2.5 — Admin web UI," following
the tracer bullet in #94, the query tester in #127, the schema view in
#128, and index stats in #129: `GET /ui/ping`, a read-only page showing
this process's ping/health status by calling the existing
`/solr/{core}/admin/ping` handler directly and rendering its real `status`
value and HTTP status, rather than running a second health-check code path.

## What was built

- **`src/admin_ui.rs`**: `PingPage<'a>` — an `askama::Template` deriving
  from `templates/ping.html` — plus `render_ping_page(core_name, status,
  http_status) -> Result<String, askama::Error>`. Three new unit tests
  matching the sibling-page convention: `ping_page_renders_the_status_it_is_
  given`, `ping_page_has_no_form`, `ping_page_escapes_the_core_name_and_the_
  status` (confirms askama's default auto-escaping neutralizes both the
  core name and an injected status value).
- **`templates/ping.html`** (new): core name, a `Status: {{ status }}` line,
  and an explanatory note that the page runs the real `/admin/ping` handler
  rather than performing a health check of its own.
- **`src/lib.rs`**: new `GET /ui/ping` route + `ping_ui` handler. `ping_ui`
  calls `ping(State, AxPath(core_name), RawQuery(None))` directly — the
  same function `/solr/{core}/admin/ping` itself routes to — extracts the
  real `status` field from that call's JSON response body, and mirrors its
  HTTP status code on the rendered page. This is the same "call the real
  handler, don't reimplement it" shape `query_ui` (#127) already established
  for `/select`, extended to `ping`.
- **`tests/admin_ui_ping.rs`** (new, 4 tests, written red-first): 200 HTML
  for a healthy core with the core name and `status: OK` rendered; the same
  OK status for an empty core, since `/admin/ping`'s own status does not
  depend on doc count; the reuse guard
  (`ping_page_reflects_the_real_admin_ping_status_value` — independently
  calls the real `/admin/ping` endpoint via the test's own HTTP client and
  asserts the rendered page's status equals that real value, not an
  independently hardcoded string); and read-only/idempotent (two GETs
  render identically, no `<form>`, `numFound` unchanged).

## Deliberate descopes

- **Routing decision, made and documented in the test file's own module
  doc, not hidden**: a dedicated `GET /ui/ping` page rather than folding a
  status element into the existing `/ui` core page. Reasoning recorded in
  `tests/admin_ui_ping.rs`'s module doc: issue #130 itself names the
  dedicated route first as its own example, and all three prior v2.5
  flesh-out milestones (#127, #128, #129) each shipped as their own route,
  so this continues the established one-concern-per-page pattern; it is
  also the cleaner "reuse a real handler's `Response`" shape given
  `core_ui` doesn't take path/query args today.
- **No unhealthy-core scenario is producible or invented.** Confirmed
  directly by reading `ping()` in `src/lib.rs`: past its only failure path
  (`check_core`, a core-name mismatch — unreachable from any `/ui/*` route,
  since this process serves exactly one core and no `/ui/*` route takes a
  core segment), the handler is unconditional and always returns
  `{"status":"OK",...}`. There is no unhealthy-core scenario this codebase
  can produce today, so none was invented for the test suite; this is a
  stated descope, not a bug or a gap the tests paper over. The test file's
  module doc records this premise-check plainly rather than working around
  it. Per this repo's "deliberate skips must expire" rule, this descope has
  **no guard that fires when the premise stops holding** — there is no
  cheap, non-brittle assertion available today that would catch `ping()`
  becoming conditional (see Follow-ups).

## Pipeline (1 round — the fourth of the four v2.5 pages, and the second to
approve clean on round 1)

1. **test-writer** wrote `tests/admin_ui_ping.rs` (4 tests), confirmed red
   (route did not exist) before any implementation, and recorded both the
   routing decision and the premise finding in the file's module doc rather
   than deciding them silently.
2. **implementor** built `templates/ping.html`, the `PingPage`/
   `render_ping_page` additions in `src/admin_ui.rs`, and the `GET /ui/ping`
   route + `ping_ui` handler in `src/lib.rs`, calling `ping()` directly and
   mirroring its status/HTTP-status. Got the full suite green (597 passed).
   Ran mutation testing and self-reported a residual, unclosable gap rather
   than hiding it: mutating `ping_ui` to hardcode `status = "OK"` (bypassing
   the real `ping()` call entirely) is **not** caught by any test, because
   with `ping()` unconditionally `"OK"` today, no test input can distinguish
   "genuinely called `ping()`" from "hardcoded the same string `ping()`
   always returns." Confirmed two other mutations do bite: changing only
   `ping()`'s own literal to `"DEGRADED"` (real wiring intact) correctly
   fails the two "shows OK" tests, while the reflect test correctly tracks
   the new value — proving the call is genuinely live end-to-end.
   Explicitly did not invent an unhealthy-core mechanism, since that was out
   of scope for #130.
3. **Reviewer (Opus, round 1) — approved, first-round clean** (like #129 —
   this makes two of the four milestones clearing without a bounce, against
   #94's 3 rounds and #128's 1 implementor + 1 reviewer + a required
   follow-up test-writer pass). Independently reproduced both mutations the
   implementor reported: confirmed the hardcode mutation leaves the full
   suite green (0 failures — the gap is real) and the `ping()`-literal
   mutation correctly fails the two "shows OK" tests while the reflect test
   tracks the new value. Additionally ran its own mutation not proposed by
   the implementor: added `|safe` to `{{ status }}` and `{{ core_name }}` in
   the template and confirmed `ping_page_escapes_the_core_name_and_the_
   status` catches it, verifying askama auto-escaping is genuinely
   exercised rather than vacuously present. Explicitly investigated whether
   a cheaper "proves it called through" assertion existed (e.g. asserting on
   other fields of `ping()`'s real response shape — `responseHeader.status`,
   `QTime`, `params` echo — that a hand-rolled hardcode would plausibly
   omit) and concluded there is none available without a production change:
   `ping_ui` deliberately discards everything but the `status` field and
   HTTP code before rendering, and both of `ping()`'s real failure paths
   (`check_core` mismatch, `strict_params` violation) are structurally
   unreachable from `ping_ui`'s call (core name passed verbatim,
   `RawQuery(None)` never trips `strict_params`) — so no input exists today
   that could distinguish a real call from a hardcode. Judged the
   implementor's residual gap legitimate, not a defect, and did not bounce
   it.

Per this repo's CLAUDE.md convention of flagging review-round count
honestly: this closed on 1 review round with no carried-forward must-fix,
the second of the four v2.5 milestones (after #129) to do so — #94 needed 3
rounds and #128 needed 1 reviewer round plus a required follow-up
test-writer-only pass. This pipeline did not consume the two-round cap and
has no unresolved must-fix item.

## Test evidence

Re-run directly by this reporter against the current committed tree, not
copied from an earlier stage's claim:

```
cargo fmt --check                                       -> clean (exit 0)
cargo clippy --all-targets --quiet -- -D warnings        -> No issues found (exit 0)
cargo test --quiet                                       -> 597 passed (31 suites, 38.81s)
cargo test --quiet --test admin_ui_ping                  -> 4 passed (1 suite, 0.20s)
```

The 4 tests in `tests/admin_ui_ping.rs` (confirmed present by direct read):
`ping_page_returns_200_html_for_a_healthy_core`,
`ping_page_shows_ok_for_an_empty_core_too`,
`ping_page_reflects_the_real_admin_ping_status_value`,
`ping_page_is_read_only_and_idempotent`.

`src/admin_ui.rs` also gained 3 unit tests (counted in the 597 total, not a
separate suite): `ping_page_renders_the_status_it_is_given`,
`ping_page_has_no_form`, `ping_page_escapes_the_core_name_and_the_status`.

## Mutation evidence

- **`ping_ui` hardcoded to `status = "OK"`, bypassing the real `ping()`
  call** — self-reported by the implementor and independently reproduced by
  the reviewer: full suite stays green, 0 failures. Confirmed as a real,
  residual, unclosable gap, not a false alarm — see Follow-ups for the only
  known fix, which is a production change out of scope here.
- **`ping()`'s own literal changed to `"DEGRADED"`, real wiring left
  intact** — self-reported by the implementor and independently reproduced
  by the reviewer: the two "shows OK" tests correctly fail, and the reflect
  test (`ping_page_reflects_the_real_admin_ping_status_value`) correctly
  tracks the new value rather than failing — proving the call from
  `ping_ui` to `ping()` is genuinely live end-to-end today.
- **`|safe` added to `{{ status }}`/`{{ core_name }}` in the template,
  bypassing askama's auto-escaping** — reviewer's own mutation, not proposed
  by the implementor: caught by `ping_page_escapes_the_core_name_and_the_
  status`.

## Review outcome

Approved, round 1 (Opus). No must-fix items. This is the second of the
four v2.5 admin-UI pages to close on a single review round with no
carried-forward gap (after #129), against #94's 3 rounds and #128's
1 reviewer round plus a required follow-up test-writer pass. As with #129,
one review round is still one round, not the two-round cap being exhausted
in this pipeline's favor: the residual hardcode gap below is judged
non-blocking because a single Opus pass, having independently reproduced
both mutations and explored a cheaper alternative test, concluded there is
none available without a production change — not because a second
independent pass confirmed there was nothing further to find.

## Follow-ups

- If the hardcode gap is ever worth closing, the cheapest real fix is a
  **production change, not a test**: render the raw `/admin/ping` JSON
  envelope the way `query.html` already renders `result_json|safe` via
  `admin_ui::html_safe_json`, then assert the rendered JSON matches the
  real wire response byte-for-byte — a hardcode would then have to
  reproduce `responseHeader`/`params` too, not just the status string.
  Explicitly out of scope for #130; file against whatever future issue
  makes health genuinely conditional.
- The "no unhealthy-core scenario exists" descope is documented as prose
  only in the test file's module doc, with **no guard that fires when the
  premise stops holding** (this repo's "deliberate skips must expire"
  rule) — not cheaply guardable today, since there is no source-scraping
  assertion that wouldn't be brittle. Carried forward as a note for
  whenever health becomes conditional, rather than inventing something
  fragile now.
- Now that all five admin-UI pages exist (`/ui`, `/ui/query`, `/ui/schema`,
  `/ui/stats`, `/ui/ping`), **none link to each other** — `core.html` links
  to none of its siblings. Not a defect specific to #130 (pre-existing
  across all five, flagged by #94's, #128's, and #129's reports already),
  but the set is now complete, so a small five-link nav header on
  `core.html` (and ideally all five templates) is the obvious finishing
  touch — worth its own small follow-up issue.
- Positive observation, carried forward from the reviewer: the four
  milestones ended up stylistically uniform across the board — same
  `<h1>Wayfinder</h1>` + `<h2>… core {{ core_name }}</h2>` template shell,
  same `.note` grey-explainer idiom, same `render_*_page` signature shape,
  same per-page escaping/no-form unit test pattern. The pattern established
  in #94 held consistently through all three follow-on milestones without
  drift.

## v2.5 admin UI: scope complete

This closes out PRD §5 "v2.5 — Admin web UI" in full: core view (#94),
query tester (#127), schema view (#128), index stats (#129), and
ping/health (#130) are all landed. The one loose end that survives across
the whole set, named by three of the five reports now (#128, #129, #130),
is the missing cross-navigation between the five pages — worth its own
small follow-up issue rather than folding into any one page's work.
