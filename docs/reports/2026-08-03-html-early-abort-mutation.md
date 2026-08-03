# Issue #275 — mutation test for the html5ever early-abort check

Date: 2026-08-03
Branch: `markdlabrecque/review-issue-275`, HEAD on `origin/main` (`59aa927`)
Follow-up 4 from #258 (`docs/reports/2026-08-02-extract-only-tracer.md`).

## What shipped

One test — `budget_exhausted_mid_character_run_aborts_within_one_chunk` in
`tests/html_extractor.rs` — that is the dedicated mutation test for the
`|| tokenizer.sink.state.borrow().error.is_some()` arm in
`HtmlExtractor::extract` (`src/extract.rs:925`). That arm bounds the
tokenizer overshoot to a single chunk when the budget is exhausted
**mid-character-run with no following tag**, the one case html5ever's only
abort channel (`TokenSinkResult::Script`, reachable only from a tag token)
cannot signal. Reverting the arm left the full suite green, which is exactly
the "code whose whole value is failing correctly" class the working
agreement makes mutation-tested.

## A wrong premise in a landed test, corrected

PR #288 (follow-up #1, the `<title>` budgeting) added
`title_accumulation_is_charged_against_the_output_budget` to the same file,
and its doc comment claimed it "also exercises the html5ever early-abort
check … this test covers it too." That claim is wrong, verified directly:

- With the arm **deleted**, #288's title test stays **green** (mutation not
  caught). Its input is `<title>{1000 chars}</title>` with
  `max_output_bytes = 10` — small enough to fit in one 8 KiB decode chunk
  *and* carrying a following `</title>` tag. So `feed` returns `Script` off
  that tag before the per-chunk error probe is ever consulted, and the arm
  is redundant for that input. The comment's own premise ("no following tag
  token") is false for the input it actually uses.
- With the arm **deleted**, the new test in this PR **fails** (see below).

So #275 was genuinely still open after #288, and #288's comment was
corrected in this PR to state plainly that it does *not* cover the arm and
to point at this test for that coverage. Per the working agreement's "don't
paper over a wrong premise" — flagged and fixed, not silently built around.

## Why the test is a timing test (and why that is sound)

The overshoot the arm prevents is **purely extra lexer CPU**: with the arm
absent, html5ever's lexer runs over every remaining byte after the budget is
blown, calling `process_token` for each, where the sink's `error` guard
returns early before `charge_token`/`push_text`. Tracing every observable:

- **Returned error** — identical with or without the arm. The bottom-of-`extract`
  `if let Some(err) = error { return Err(err) }` returns the same
  `StructuralLimit(XmlEvents)` either way (confirmed: both runs return it).
- **Output text** — identical. The guard drops every post-violation token
  before any `push_text`, and `Tokenizer::end()` is a no-op on our sink
  (html5ever 0.39's `end` only asserts its empty in-buffer is drained, which
  it is; verified it does not panic).
- **No counter we own reflects it.** Once `error` is set, the guard
  short-circuits *before* `charge_token`, so the budget's event counter and
  the deadline clock are not consulted for the overshoot bytes — the work is
  all inside html5ever's lexer, behind our API.

So the amount of lexer work is the *only* observable, and the test asserts
promptness: it compares the extraction's wall time to the one piece of work
unavoidable in **both** paths — the `String::from_utf8_lossy(input.bytes).into_owned()`
copy `extract` performs up front — and requires the extraction to do little
more than that copy (`extract_time < 3.0 × copy_time`).

Measuring both on the same machine in the same run cancels runner speed to
first order (both are memory/compute-bound over the same bytes), so the
ratio — not an absolute millisecond count — is what is asserted. `min` over
5 runs rejects scheduling spikes without hiding real work. Measured on the
development machine, 16 MiB input:

| arm state | extract : copy ratio |
|---|---|
| present (green) | ~1.0× |
| deleted (mutation) | ~6.5–8× |

`3.0×` sits between: ~3× headroom on the green side, and the mutation fails
with ~2× to spare. It does not fight CI runner speed the way an absolute
millisecond bound would; the only residual machine-dependence is the
lex/copy cost ratio, which is one-directional (lexing is inherently costlier
than a validate+memcpy), so the mutation stays caught on any real machine.

## Mutation evidence

Recorded directly, on `origin/main` (`59aa927`, i.e. including #288):

```
$ # arm present:
$ cargo test --test html_extractor
test budget_exhausted_mid_character_run_aborts_within_one_chunk ... ok
test title_accumulation_is_charged_against_the_output_budget ... ok
test result: ok. 7 passed

$ # arm deleted (the mutation):
$ cargo test --test html_extractor
test budget_exhausted_mid_character_run_aborts_within_one_chunk ... FAILED
    # extract was ~8x the unavoidable lossy-copy cost (bound 3.0)
test title_accumulation_is_charged_against_the_output_budget ... ok   # never covered the arm
test result: FAILED. 6 passed; 1 failed
```

Deleting `|| tokenizer.sink.state.borrow().error.is_some()` makes exactly
this test fail, and no other. Reverted.

## Green evidence (re-run on the rebased HEAD, not copied from an earlier claim)

- `cargo test --no-fail-fast` — **1070 passed (55 suites)**, 0 failed,
  hermetic (no network, no Docker).
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact invocation).
