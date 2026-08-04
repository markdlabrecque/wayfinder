# Issue #357 — `tests/online_snapshot.rs` flakes under parallel test load

Hardens the Unix-only online-snapshot contract test so the two intermittent
failure modes the issue names both stop firing when the machine is under CPU
contention. Test-only change; no production diff.

## Root cause

Both failure modes trace to one spot: `http_request` read the response with
`stream.read_to_string().expect("read HTTP response")`.

- **Mode 1 (line 88, `WouldBlock`).** `http_request` sets a 2s `read_timeout`.
  Under contention the live server replies slower than that, and on Unix a
  fired read-timeout is surfaced as `ErrorKind::WouldBlock`. `expect` turned a
  transient timeout into a fatal panic.

- **Mode 2 (line 318, merge-observation deadline).** The
  `repeated_snapshots_reopen_during_continuous_commits_and_merges` test drives
  its continuous updater thread through the *same* `http_request`. A `WouldBlock`
  panic there kills the updater mid-loop: `first_wave_tx` is dropped and commits
  stop, so the merge-observation loop never sees
  `first_wave_committed && committed_batches >= 8` and exhausts its deadline —
  the line-318 panic. I.e. mode 2 is largely a **cascade of mode 1**, not an
  independent timing assertion. Genuine slow-merge under contention is a
  secondary possibility, so the merge wait is made contention-tolerant too.

Note on the issue's premise: it asks to "make the merge-observation wait
deadline-based rather than iteration-count-based", but that wait was *already*
deadline-based (`loop` + `merge_deadline`, no iteration cap). The real lever was
the `WouldBlock` panic.

## What changed

`tests/online_snapshot.rs` only.

1. **`http_request` retries through transient reads.** Replaced the single
   `read_to_string().expect(...)` with a read loop that treats
   `WouldBlock`/`TimedOut` as retryable, bounded by a 10s overall deadline with a
   2ms backoff. `Connection: close` still terminates the read via `Ok(0)` (EOF);
   the deadline is only a safety bound. Connect timeout raised 200ms → 1s for the
   same contention reason. This is the fix for mode 1 *and* the cascade form of
   mode 2 (the updater no longer dies on a slow response).

2. **Merge-observation window measured from the first commit wave.** The merge
   deadline now starts when the first commit wave actually lands
   (`first_wave_rx`), not at loop start, so commit time spent under contention no
   longer eats the merge budget. A 45s hard outer cap (`outer_deadline`) keeps a
   stalled updater from hanging the test; the meaningful budget is the 20s
   `merge_window`. `unwrap_or(outer_deadline)` collapses both into the existing
   single deadline check.

## Verification

- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean
  (CI's exact commands).
- `cargo test --test online_snapshot`: 3/3 runs green (5.6–5.9s each), matching
  the issue's "passes 3/3 on an unloaded machine" baseline.
- `cargo test` (full suite) green.

The flake itself only manifests under parallel `cargo test` load, which this
environment can't reproduce deterministically, so "red for the right reason" is
taken from the issue's captured panics rather than a local reproduction; the fix
targets the documented panic sites directly.

## Mutation check

The `WouldBlock`/`TimedOut` arm is the whole value of change #1. Reverting it
(replacing the loop with `read_to_string().expect(...)`) restores the original
panic-on-timeout behavior the issue reproduces; reverting change #2 (deadline
from loop start) re-opens the commit-time-eats-budget contention window.
