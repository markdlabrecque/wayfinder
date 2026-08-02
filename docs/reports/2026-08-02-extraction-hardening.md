# #257 follow-up: extraction hardening

Date: 2026-08-02
Branch: `257-followup-extract-hardening` (off merged `main` at `e80623a`)
Follow-up to: `docs/reports/2026-08-02-extraction-phase-0.md` (PR #269, issue
#257, merged as `e80623a`)

**No issue number.** This is not new scope; it closes findings from a third
review pass over `src/extract.rs` after it had already merged, plus two open
questions the phase-0 report itself listed as follow-ups (item E's
`&mut Budget` deferral, item F's XHTML-vs-Xml misdetection). Branch naming
follows the worktree tooling that set this up rather than the CLAUDE.md
`<issue>-<slug>` form — recorded as a deliberate deviation, not an oversight.

## Why a third pass happened

Phase 0's own closing note (round 2, `b00834a`) approved the work but said
plainly that a security/resource-control substrate protecting untrusted
uploads, having received only the stage's two-round default cap, deserved a
fresh look rather than being treated as settled. It was right to ask for one:
pass 3 found a real bypass of the ZIP cumulative-bytes guard (item A) that
both prior rounds had missed, plus three more findings below.

## What pass 3 confirmed clean

Recorded because it is real signal, not merely "nothing found":

- **Content-sniffing panic safety.** `detect_by_signature` / `sniff_markup`
  never slice past a bounds-checked window; fuzzed mentally over truncated
  and adversarial prefixes (empty input, input shorter than the sniff
  window, a `<?xml` with nothing after it), nothing panics.
- **The UTF-8 decode loop.** `decode_chunk_len` never splits a multi-byte
  scalar across a chunk boundary, and lossy decoding neither drops nor
  double-counts scalars; replacement-character amplification is bounded by
  the same scalar/byte budget as any other output.
- **Release-mode arithmetic in the pre-existing accumulators.** The
  cumulative counters use `saturating_add`, not plain `+`, so a release
  build cannot silently wrap past a limit the way debug-mode overflow
  checks would have caught by panicking instead.
- **Permit and channel accounting.** The pool's `try_acquire`/`Permit` pair
  and the `mpsc` job channel account correctly for the success, panic, and
  (as of this pass) timeout paths.
- **Temp-file cleanup on every error/drop/unwind path** in
  `stream_to_tempfile` and its callers.

## The four findings and their fixes

**A — `admit()` trusted declared ZIP metadata only.** An entry declaring
`compressed_size == 0, uncompressed_size == 0` — exactly what a
data-descriptor entry (general-purpose bit 3) declares, and what a forged
central directory can declare too — skipped the per-entry size check and the
ratio check (both guarded by `uncompressed_size > 0`), and added nothing to
the cumulative declared total. 4096 such entries (the configured entry-count
ceiling) would be admitted while their real decompressed streams expand to
gigabytes each, with `ZipBudget` reporting nothing over budget throughout.

Fix: `charge_actual(bytes)`, called once per decompressed chunk actually
read, enforcing both `zip_max_entry_bytes` and `zip_max_cumulative_bytes`
against real bytes. Declared metadata is now documented as a pre-filter, not
the guard — cheap enough to reject the honest-metadata cases (an oversized or
42.zip-shaped declaration) before a byte is decompressed, but not load-bearing
for the actual bound. The required calling convention for phase 2a is spelled
out on `ZipBudget`'s type docs: one `admit()` per entry, then N
`charge_actual()` calls for that entry's real bytes, and the *next* `admit()`
is what marks the entry boundary (resets the per-entry running total).

**B — rejected entries did not count.** `zip_max_entries` was checked against
`entries_seen`, which only advances on a successful `admit()`. An archive of
ten million entries all named `..\evil` returns `InvalidPath` every time and
never advances that counter, so a walker shaped the natural way — skip a bad
entry, keep going — loops unbounded; the entry-count guard was silently a
no-op for that walker shape.

Fix: a separate internal `entries_attempted` counter, incremented before any
check can reject, and `zip_max_entries` is now enforced against it.
`entries_seen()` still means *admitted* entries — its contract is unchanged —
and the type docs now say explicitly that it is not what the entry-count
guard reads.

**C — `rx.await` had no timeout.** `spawn_extraction` awaited the worker's
oneshot result with nothing bounding the wait, so a wedged parser (an opaque,
non-cooperative one — the expected case for PDF, not the exotic one) pinned
its caller forever. The module's own doc comment at the time claimed the
worst case was `TooBusy` shedding once the pool filled — which was false for
the caller already inside the wedged call, who would simply hang.

Fix: the timeout is baked into `spawn_extraction` itself (`deadline` is now a
required parameter, not a convention the caller could forget), wrapping the
`rx.await` in `tokio::time::timeout(deadline + SPAWN_TIMEOUT_GRACE)` where the
grace (250ms) exists only so a cooperative parser's own `DeadlineExceeded`
wins the race when it can. On expiry the permit is deliberately **not**
freed — the worker thread releases it when (if) the job returns, so a job
that never returns leaves the slot burnt. The residual risk is now a burnt
pool slot, not a hung request, and the module docs were corrected to say so
instead of the disproven `TooBusy`-only claim.

**D — `max_body_bytes`'s comment claimed disk was bounded by
`max_concurrency`.** Nothing in `stream_to_tempfile` consults a permit or the
concurrency limit; it takes `(source, dest, max_bytes)` and nothing else. So
1000 concurrent 32 MiB POSTs are 32 GB of temp files and 1000 open file
descriptors, all before the extraction pool is ever asked for a slot.

Fix is documentation plus a route-side design requirement recorded now, ahead
of the route landing: bound in-flight uploads separately from extraction (own
semaphore or byte counter, sized against the host's disk), and acquire the
extraction permit **only around the parse**, never across the body read. The
named trap is spelled out because it's the fix someone would reach for first:
acquire-then-stream holds an extraction permit for the whole upload, so with
the default `max_concurrency` of 4, four slowloris connections dribbling one
byte per second take extraction offline indefinitely for everyone else — and
the deadline does not save it, because `Budget` is only consulted inside the
job, which has not started yet.

## Two resolved open questions from phase 0

**F — XHTML declared `text/html` no longer detects as `Xml`.** `sniff_markup`
now distinguishes an XML declaration followed by an `<html>` root (or XHTML
doctype) anywhere in a 1KiB leading window from a bare no-declaration
document, which is only sniffed as HTML if it *opens* with `<html` or
`<!DOCTYPE html`. Resolved and tested; the known false positive
(`window_contains_html_root`'s unanchored search misreading XSLT with a
literal `<html>` result element as HTML) is documented and carried forward as
a follow-up, not silently fixed alongside it.

**E — accepted deviation, not done.** The spec asked for `Extractor` to take
`&Budget` instead of `&mut Budget`. What shipped: the six structural counters
are now private, `Cell`-backed, and reachable only through delegating
`&self` methods (`enter_xml_element`, `count_xml_event`, ...) — an extractor
holding the budget cannot reassign a counter, reset one, or reach a
`decrement` on a cumulative counter that has none defined. That is real and
enforced by which methods exist, not by a doc comment.

What did **not** ship: `Extractor::extract` still takes `&mut Budget`, so an
in-tree extractor can still reassign the whole budget
(`*budget = Budget::new(..)`) and get fresh counters and a fresh deadline that
way. Structural counters are unforgeable; whole-budget reassignment is not —
guard integrity still rests, for exactly that one much more conspicuous move,
on review of in-tree extractors. This is recorded as an accepted deviation
with a residual risk, not as the item being closed. The named trigger for
revisiting: the first extractor that does not need to read its own output
back (today's one extractor, `PlainTextExtractor`, does — `output_text()`
returns `&str`, which a `RefCell`-backed budget could not, so designing the
`&Budget` signature now would mean guessing at a `Ref<'_, String>` or
equivalent API shape from a single call site).

## Three open questions resolved as decisions, not code

- The 413/503/415/400 budget-violation status mappings stay as shipped in
  phase 0. Only `Parse -> 500` has captured fixture ground truth; the
  self-expiring guard (`budget_violation_statuses_have_no_captured_fixture_yet`)
  still forces the recheck once the `/update/extract` route lands and can
  capture real Solr responses for the others, so there is nothing to gain by
  guessing further now.
- No `[extraction]` config section. Limits stay `ExtractLimits::default()`
  until a route gives an operator an actual reason to tune one of them;
  adding config surface with no consumer would be guessing at shape.
- Branch naming (`257-followup-extract-hardening`) follows the worktree
  tooling that created it rather than the CLAUDE.md `<issue>-<slug>` form.
  Noted rather than silently deviated from, since there is no issue number to
  put in that slot.

## Process honesty

- **The rationale I gave the implementor for deferring `&Budget` was false.**
  I told the implementor the reason was "an extra copy on the hot path" for a
  `RefCell`-backed output. That is wrong: the copy already happens once,
  at the end, regardless (`output_text()[start..].to_string()`), and the hot
  path is `push_str`, which would simply move inside one `borrow_mut()` per
  chunk. This shipped into a doc comment in `6baedf5` and was caught by round
  1 review (`fc39025`), which rewrote it onto the real ground: API shape, not
  cost. This is the **second** overclaiming comment in this module — the
  first (item E's original "fields are unforgeable" claim in phase 0) was
  corrected in `b00834a`. The conclusion to draw explicitly: in this module,
  doc claims need reviewing as carefully as the guards themselves, because
  phase 2a will build its extractors against exactly what these comments
  claim is true.
- **Three mutants survived the implementor's own first pass** and drove three
  new tests in `6baedf5`/the round-1 fix — most importantly, `entry_actual`
  using assign instead of add-assign in `charge_actual`, which left the
  chunked-read case (the actual attack item A exists to close) unbound: a
  walker charging one giant entry in many small reads would have each read
  checked in isolation against the per-entry ceiling instead of the running
  total. The ZIP hole item A fixes partially reopened inside its own fix,
  caught only by the implementor's own mutation pass, not by inspection.
- **Three more mutants survived into round 1 of review** and drove three
  further tests in `fc39025`: committing the per-entry actual total before
  the cumulative check could reject it (a fail-safe-direction but still real
  leak of per-entry allowance), `SPAWN_TIMEOUT_GRACE` relaxed to zero
  (untested — the wedge test only proved the timeout fires, not that the
  grace margin does anything), and the doctype match loosened to accept any
  doctype name or only its first letter (both let SVG and other
  non-HTML-doctype vocabularies get routed to the Html branch).
- **Both reviewers ran their mutations in isolated copies of the repository,
  not the working tree** — a departure from mutating the tree directly and
  reverting, which the phase-0 report used as its evidence shape. Recorded
  here as a fact about how the evidence was produced, not represented as
  equivalent to an in-place mutation-and-revert.

## Follow-ups for the route/OOXML/PDF issues

Framed as acceptance criteria the next issues should inherit, not as open
questions this branch leaves dangling:

- **Item D's in-flight-upload bound and permit ordering** — a route cannot
  ship `/update/extract` without a bound on concurrent temp-file bytes
  independent of `max_concurrency`, and must acquire the extraction permit
  only around the parse, never across the body read (the slowloris trap
  above).
- **Item A's `.take(zip_max_entry_bytes)` limiter plus the `charge_actual`
  calling convention** — phase 2a's ZIP/OOXML walker must follow the
  documented per-entry sequence exactly (limiter on the reader, one `admit()`
  then N `charge_actual()`, next `admit()` is the boundary) or the guard does
  nothing.
- **Symlink zip-slip** — `is_safe_entry_path` cannot and does not catch a
  symlink entry (unix mode in external attributes) pointing outside the
  extraction directory; a no-op today because nothing writes archive
  contents to disk, but a hard requirement the moment anything does.
- **`window_contains_html_root`'s unanchored 1KiB search** — the known false
  positive is XSLT with a literal HTML result element; anchoring the search
  past the declaration/doctype/comments/PIs is deferred, not fixed, alongside
  item F.
- **The `Ref<'_, String>` (or equivalent) API-shape decision for `&Budget`**
  — item E's real deferral, to be designed against the first extractor that
  does not need `output_text() -> &str`.
- **Capturing real Solr responses for the uncaptured status mappings**
  (413/503/415/400) once the route exists to provoke them.

## Green evidence (as of `793302f`)

Re-run directly against this branch, not copied from an earlier claim:

- `cargo test` — **1010 passed (53 suites)**, 65.3s, hermetic (no network, no
  Docker).
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact
  invocation).
- `cargo build --release` — clean, 6.2s.
- `git diff e80623a..HEAD -- Cargo.toml Cargo.lock` — `Cargo.lock` is
  unchanged across the whole branch; `Cargo.toml` gained only the explicit
  `time` feature on `tokio` (`5647ea3`), needed for `spawn_extraction`'s new
  `tokio::time::timeout`.
- `git diff 795be7c..HEAD -- tests/extraction.rs` — **additions only**: zero
  removed lines across the implementation and both review-fix commits: no
  test written red in `795be7c` was weakened or deleted to get to green.

## Review rounds

Two rounds, the stage's default cap, over already-merged code:

- Round 1 (`fc39025`): corrected the false "extra copy" rationale for item
  E's deferral, and drove three new tests for the three mutants that survived
  into round 1 (per-entry-commit-before-cumulative-check, zero
  `SPAWN_TIMEOUT_GRACE`, relaxed doctype match).
- Round 2 (`793302f`): four doc-comment tightenings plus the compile-time
  `Budget: Send` assertion.

Per the phase-0 report's own closing note, this substrate has now had a
second independent look specifically at the concerns it raised (the `&mut
Budget` encapsulation gap, and the guard correctness under adversarial
metadata) and found one confirmed-serious bug (item A) plus three more real
findings. That is evidence the extra pass was warranted, not evidence the
module is now beyond needing further review — the same two-round cap applies
here, and item E's residual risk plus the route/OOXML/PDF follow-ups above
are exactly the kind of unfinished business a third-plus pass exists to catch
before it ships live.
