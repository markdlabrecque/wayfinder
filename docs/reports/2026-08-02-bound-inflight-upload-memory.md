# Issue #273 — bound total resident upload memory on `/update/extract`

Date: 2026-08-02
Branch: `markdlabrecque/fix-issue-273`
Follow-up 2 from #258 (`docs/reports/2026-08-02-extract-only-tracer.md`),
whose closing "Follow-ups (deferred, not fixed)" item 2 named this exact gap.

## The gap

`ExtractionRuntime::max_concurrency` capped only the **parse** slots. The
HTTP-layer intake ahead of them was uncapped, and that is where the memory is
held: the upload body is streamed to a temp file, read back whole into a
resident `Vec<u8>` (`bytes`), and only *then* handed to the extraction pool.
So the real resident-RAM ceiling was set by the **connection count**, not by
`max_concurrency` — `max_body_bytes × HTTP concurrency`, unbounded. 1000
concurrent 32 MiB POSTs were 32 GiB of resident upload memory before the
extraction pool was ever asked for a slot. This is precisely what the
`max_body_bytes` doc comment's "route-side design, point 1" had flagged as the
required fix when the route landed.

## What shipped

A **separate in-flight-upload admission count**, distinct from the parse pool,
acquired *before* the body is streamed and released as soon as the uploaded
bytes are consumed — the shape point 1 prescribed, and deliberately *not* the
parse permit (point 2's slowloris trap: holding a parse permit across the body
read lets four dribbling connections take extraction offline).

- **`ExtractLimits::max_inflight_uploads`** (default 8). Bounds total resident
  upload memory to `max_inflight_uploads × max_body_bytes` (8 × 32 MiB = 256 MiB
  at the defaults). Sized above `max_concurrency` (4): the intake phase is far
  cheaper than the parse, so an intake budget equal to the parse pool would
  starve it. Configurable via `[extraction] max_inflight_uploads`. `0` is the
  blunt shutoff — every upload 503s at intake; there is no "unlimited"
  spelling, because an unlimited in-flight count is exactly the unbounded-RAM
  condition this exists to prevent.
- **`ExtractionRuntime::available_inflight`** — an independent `Arc<AtomicUsize>`,
  a sibling to the parse-pool `available` counter. No shared state: a held
  in-flight slot burns no parse slot and vice versa (pinned by a runtime unit
  test, below).
- **`try_acquire_inflight`** / **`InflightUploadPermit`** — the admission path
  and its Drop-based slot, mirroring the existing `try_acquire_permit` /
  `ExtractionPermit` pair. (The Drop body is deliberately duplicated across the
  two permit types rather than shared through a wrapper `Slot` type: a newtype
  around a `Drop` carrier never *reads* its field — it owns it only for its drop
  — and so trips `dead_code` under `-D warnings`; the three-line duplication is
  the lesser cost than two `#[allow]`s.)
- **`ExtractError::InflightUploadsBusy`** — a distinct 503 variant (code
  `extraction-inflight-busy`), sibling to `TooBusy` (parse-pool saturation).
  Both are "capacity, retry later" 503s, but a distinct code lets an operator
  tell intake saturation apart from parse-pool saturation in logs. No captured
  Solr fixture (Solr has no such bound; this is Wayfinder's containment), so the
  existing budget-status trip-wire's diagnostic now lists it alongside the other
  uncaptured-budget statuses.
- **The handler** (`update_extract`) acquires the in-flight slot *after* the
  cheap param/core validation, *before* the multipart intake loop, and drops it
  explicitly once `spawn_extraction` resolves — the moment `bytes` (moved into
  the parse closure) is freed. That explicit release sits *before* the
  indexing-path early return, so an intake slot is not held across the
  (potentially slow) commit while the upload's bytes are already gone. RAII
  still covers every `?` and any panic between acquire and the explicit drop.

## Done-when, met

The issue's "done when": *total in-flight upload bytes are bounded by something
other than the connection count*; *a test should demonstrate that N concurrent
uploads cannot exceed the configured total*. Met: total resident upload bytes
are bounded to `max_inflight_uploads × max_body_bytes`, and
`extract_inflight_uploads_over_configured_max_is_503` demonstrates that with
`max_inflight_uploads = 1`, the single configured slot held, the (N+1)th upload
is rejected at intake (503) even though the parse pool is completely free, then
succeeds once the slot is returned.

A count cap rather than a finer bytes-counter: the count cap fully bounds the
multiplier the issue names (every admission is ≤ `max_body_bytes`), and is the
simpler shape. A bytes-counter that reserves the *actual* streamed size rather
than the per-request ceiling — admitting more small uploads — remains a
`ponytail:` on `max_body_bytes`'s point 1, recorded as a possible refinement,
not a gap.

## Tests

All red for the right reason before the handler was wired, then green — and
mutation-tested.

- `extract_inflight_uploads_over_configured_max_is_503`
  (`tests/extract_route.rs`) — the route-level spec. Holds the single configured
  in-flight slot directly via `try_acquire_inflight` on the route's own runtime
  (deterministic, no race — the lesson from #258's `6b88dcc`, where a two-
  overlapping-requests concurrency test flaked ~1 run in 5). With
  `max_concurrency` left at its default the parse pool is free, so the 503 can
  only be the intake budget, proving the two are independent at the route. Drops
  the slot and asserts the same upload then 200s. Mutation-tested: disabling the
  handler's intake check fails it (`left: 200, right: 503`); reverted.
- `extract_inflight_uploads_zero_rejects_every_upload_at_intake`
  (`tests/extract_route.rs`) — pins the documented `max_inflight_uploads = 0`
  shutoff at the route.
- `inflight_upload_budget_is_independent_of_the_parse_pool`
  (`tests/extraction.rs`) — the runtime-level independence invariant, purely
  synchronous (no async, no race). **Strengthened after review round 1** to
  acquire the *entire* parse pool (all `max_concurrency` permits) while holding
  the in-flight slot, plus assert a `max_concurrency+1`th permit fails. The
  first draft acquired only two of four permits, which left a coupled
  implementation that quietly burned one parse slot still passing; a mutation
  that makes `try_acquire_inflight` also `fetch_sub(1)` the parse counter now
  panics at "parse permit 4/4 must be free" (and would not have under the old
  shape). Mutation applied, confirmed caught, reverted.

## Review

One independent read-only review round (Reviewer subagent) ran the full gate
(PASS) and scored the six named suspected weaknesses. Verdict CHANGES
REQUESTED on one must-fix (the independence-test mutation-resistance gap above,
now closed) plus the `max_inflight_uploads = 0` nice-to-have (added). The other
five (slot-leak, bound-holds, determinism, error-mapping, the `0` edge) passed.

## Green evidence (re-run after the review fixes, not copied from an earlier claim)

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact invocation).
- `cargo test --no-fail-fast` — **1192 passed, 0 failed**, hermetic (no network,
  no Docker).
- `cargo test --test extract_route extract_inflight_uploads_over_configured_max_is_503`,
  run 10× then 5× after the explicit-release change — 1 passed / 0 failed every
  run (deterministic by construction).

## Pre-existing flake noted, not introduced

`tests/online_snapshot.rs::repeated_snapshots_reopen_during_continuous_commits_and_merges`
flaked once across the full-suite runs (a timing-based "continuous commits and
merges" test in a file this branch does not touch — confirmed passing 4/4 in
isolation). Unrelated to this change; flagged here so it is not mistaken for a
regression.

## What this does *not* close

- The `bytes` read-back is still a full copy in RAM (`Extractor::extract` takes
  `&[u8]`). The inflight cap bounds *how many* such copies are resident, not
  whether each is a copy. Retired when the first incremental extractor lands
  (the phase-2a ZIP walker), per the updated `ponytail:` on the read-back.
- The finer bytes-counter intake budget named above (`ponytail:` on point 1).
