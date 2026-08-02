# Issue #277: differential diagnostic message wrapping

## Scope

Four string literals in `tests/differential.rs` had been reflowed with their source
indentation left inside the literal, so a failing differential run printed 6- and
22-space runs mid-sentence (e.g. `Wayfinder has      no PDF extractor`). Purely
cosmetic — affects only diagnostic output on failure, not what anything asserts.

## What was built

- Reflowed all four literals (`DIVERGENT_STATUS_MULTIPART`'s reason, and the three
  `failures.push(format!(...))` messages in the multipart status-divergence runner)
  with backslash continuations so they render single-spaced.
- Added the guard the defect actually calls for, since every divergence entry is
  hand-wrapped by whoever adds it next:
  - **Test A** (`diagnostic_constant_tables_have_no_double_spaces`, runtime): fails
    if any reason string in the six diagnostic constant tables contains a run of
    2+ spaces.
  - **Test B** (`source_text_has_no_double_spaces_outside_leading_indentation`,
    source text): an `include_str!` scan of `tests/differential.rs` itself, since
    the three inline literals are only reachable by making the differential runner
    actually fail, which isn't hermetically triggerable.

Commits: `eea3ecb` (guard, red for the right reason), `1d5a663` (reflow +
narrowed exemption rule), `a79ffb3` (bound the exemption, name two ceilings).

## The wrinkle

Stage 1 built Test B's exemptions as a table of 12 bare line numbers. That doesn't
survive this repo: `tests/differential.rs` is a documented hot file every branch
appends to, and Test B recomputes line numbers from `include_str!`, so inserting a
line anywhere above the first exemption slides all twelve onto unrelated lines —
silently exempting arbitrary lines and flagging legitimate ones, with no signal.
Caught at orchestration, before stage 2.

The fix narrowed the *rule* instead of enumerating lines. Eleven of the twelve were
one construct — `eprintln!("  (...")`, a two-space run at the start of a string
literal marking a sub-message printed under the preceding line, i.e. deliberate
*output* indentation, structurally the same as source indentation. Skipping spaces
that immediately follow an opening quote (or a `\n` escape) removed all eleven. One
content-keyed exemption remains, for `keyorder_corpus`'s deliberately column-aligned
JSON fixture. Verified move-robust: prepending blank lines leaves both tests green.

Generalise: a lint whose exemptions are keyed on line numbers rots into
mislabelling on the next unrelated edit; key exemptions on content, or better,
narrow the rule until they are unnecessary.

## Review

Reviewer **approved, nothing must-fix**, after attacking the hand-rolled scanner
with constructed inputs — escaped quotes, `\\` before a closing quote, byte
strings, `//` inside a string, unterminated open-string lines, single-line raw
strings. Every failure mode found was in the loud direction (a `'"'` char literal
flips string-tracking state and false-positives) rather than the silent one. The
reviewer also verified all four reflowed literals render word-for-word identical to
`935aeb2` apart from collapsed whitespace, and reproduced the move-robustness claim.

Three non-blocking follow-ups came back. **Two were folded in directly by the
orchestrator in `a79ffb3` rather than filed as issues** — a deliberate, disclosed
skip of the pipeline for a validated one-liner:

1. The skip accepted a run of *any* length after an opening quote, so reflow
   debris landing in that position was swallowed. Bounded to exactly two spaces.
   Mutation-verified by the orchestrator: a 22-space run immediately after an
   opening quote **passes** under the old rule and **fails** under the new one —
   the tightening is load-bearing, not cosmetic. Zero new failures across the file.
2. Unstated ceilings now named in `ponytail:` comments: Test A enumerates its six
   tables by name, so a seventh is silently uncovered; Test B is blind to a
   literal wrapped without a `\` continuation, and to a run immediately after an
   escaped quote at the start of a continuation line.

The third follow-up — a contrived scanner miss the reviewer judged not worth code
— is closed **won't-fix**.

Only one review round ran (approved on the first pass), so this was not capped at
the 2-round default; it did not need the second round.

## Evidence

Re-ran the gates independently at `a79ffb3`, worktree clean, after `cargo clean`
to rule out stale-cache artifacts:

- `cargo fmt --check` — pass, no output
- `cargo clippy --all-targets -- -D warnings` — pass, no warnings
- `cargo test --no-fail-fast` — all green: 23 + 12 + 34 + 7 unit/integration tests,
  `tests/differential.rs`: **36 passed, 0 failed**, including both new guard tests
  and `extract_multipart_manifest_matches_captured_fixtures`.

One process note, not a code defect: mid-review I hand-perturbed
`DIVERGENT_STATUS_MULTIPART`'s status (415→416) to force the failure path for the
before/after check below, then restored the file with `mv` from a backup.
`cargo test` afterwards still reported `extract_multipart_manifest_matches_captured_fixtures`
FAILED even though `git diff` showed the file byte-identical to HEAD — cargo's
mtime-based fingerprint had kept the stale perturbed test binary because the
restored file's mtime wasn't newer than the cached build. `cargo clean` forced a
true rebuild, which passed cleanly and reproduced twice more. Recorded so the
"gates are green" claim above is verifiable rather than asserted.

Issue #277 also asked for verification "by actually triggering one of the failure
paths and reading the output rather than by inspection." Stage 2 did this: it
perturbed the recorded status to 500 so the "but they now agree" branch fires, and
temporarily stubbed the `ran` assertion that otherwise short-circuits before the
`failures` assertion is reached. Both perturbations were reverted before `1d5a663`.
The message as actually rendered:

- Before (`935aeb2`):
  `extract_corrupt_pdf: DIVERGENT_STATUS_MULTIPART says Wayfinder answers 500                      where the capture is 500, but they now agree (issue #258: Solr's Tika parses this malformed PDF and throws, which is a 500; Wayfinder has      no PDF extractor at all, ...) -- remove this entry`
- After (`a79ffb3`):
  `extract_corrupt_pdf: DIVERGENT_STATUS_MULTIPART says Wayfinder answers 500 where the capture is 500, but they now agree (issue #258: Solr's Tika parses this malformed PDF and throws, which is a 500; Wayfinder has no PDF extractor at all, ...) -- remove this entry`

A second message was rendered the same way (status perturbed to 418):
`extract_corrupt_pdf: recorded status divergence expects 418, got 415, body: {"responseHeader":...}` — single-spaced, no 22-space run.

Reproducing this requires stubbing the `ran` assertion as well as perturbing the
status; perturbing the status alone stops at the earlier guard, which is what the
reporting pass hit on its first attempt.

No production behavior changed; this is test-file-only.
