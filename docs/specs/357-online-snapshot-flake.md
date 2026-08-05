> **Historical implementation record.** This completed spec does not define current requirements or future work.

# #357 — `online_snapshot` flakes under parallel test load

Branch: `357-online-snapshot-flake`. **Lands on `main` immediately after
PREP-1 (#368), before any feature branch in this batch fans out.**

## Why this is first

The rest of the batch runs as concurrent worktrees, which means several
`cargo test` runs competing for CPU. That is precisely the condition under which
this test fails. If it is still flaky when the batch fans out, every branch's
gate becomes untrustworthy — and an unreliable gate is worse than a red one,
because green stops carrying information.

## The two failure modes

Both observed in a single run on a loaded machine; both pass 3/3 unloaded.
Pre-existing, unrelated to the change that surfaced them.

**1. `tests/online_snapshot.rs:88` — `WouldBlock` treated as fatal.**

```
read HTTP response: Os { code: 35, kind: WouldBlock, message: "Resource temporarily unavailable" }
```

A non-blocking socket read returning `WouldBlock` means "nothing available
right now, try again" — it is not an error. The test currently propagates it as
one. Under load the scheduler delays the response past the read, and the test
panics on a healthy connection.

**2. `tests/online_snapshot.rs:318` — iteration-count timing assertion.**

```
a snapshot did not observe Tantivy's first-wave merge before ...
```

The wait is bounded by a number of iterations rather than elapsed time. On a
starved process each iteration takes longer in wall-clock terms but the count
runs out just as fast, so the test gives up before the merge it is waiting for
has had a real chance to happen.

## Scope

**Fix the root cause in both places; do not paper over either with a retry
count that happens to be large enough on your machine.**

- For `WouldBlock`: retry the read rather than failing. Bound the retry loop by
  a wall-clock deadline so a genuinely dead connection still fails the test
  instead of hanging forever. Distinguish `WouldBlock` (retry) from every other
  error kind (still fatal) — do not blanket-retry all IO errors, which would
  hide real regressions.
- For the merge observation: make the wait deadline-based, not
  iteration-based. Poll on an interval until a wall-clock deadline expires.
  Pick the deadline generously — this test's job is to observe that a merge
  eventually happens, not to assert it happens quickly.

Check whether either pattern appears elsewhere in the suite. If the same
non-blocking read helper or the same count-bounded wait is used by other tests,
fix it once in the shared place rather than patching this file only. Say in the
PR what you found.

## Verify before you start

Reproduce both failures. A fix for a flake you have not seen fail is a guess.
Load the machine deliberately — run several `cargo test` invocations
concurrently, or use a CPU stressor — and confirm each failure mode appears at
the cited line. If you cannot reproduce one of them, say so explicitly rather
than fixing it blind.

## Testing

The evidence that this worked is **the test passing repeatedly under load**, not
passing once. Run the suite under the same contention you used to reproduce, at
least 10 consecutive times, and report the results in the PR body. A single
green run proves nothing here — that is the entire nature of the bug.

## Files

`tests/online_snapshot.rs`, plus any shared test helper the same patterns live
in (check `tests/common/mod.rs`). **Note the standing rule for that file: the
`dead_code` allow lives there as an inner attribute — do not add a second one
on `mod common;`.**

## Definition of done

- Both failure modes fixed at the root, with the `WouldBlock` retry bounded by a
  deadline and the merge wait deadline-based.
- 10+ consecutive green runs under deliberate CPU contention, reported in the PR.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean.
