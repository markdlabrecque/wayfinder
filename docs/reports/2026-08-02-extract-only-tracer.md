# Issue #258 — /update/extract tracer: extractOnly plain text + HTML

Date: 2026-08-02
Branch: `258-extract-only`, HEAD `01fd1b2`
Follow-up to: `docs/reports/2026-08-02-extraction-phase-0.md` (#257, phase 0
budgets/contract) and `docs/reports/2026-08-01-text-extraction-exploration.md`
(#171 exploration)

## What shipped

The client-evidenced tracer bullet for Solr's `ExtractingRequestHandler`: a
real `/solr/{core}/update/extract?extractOnly=true` route, end to end, for
the two formats #257 was scoped to unblock — plain text and HTML — verified
against captured Solr ground truth rather than assumed.

- **Multipart intake.** First part with a non-empty filename is streamed
  through the existing `ChunkSource`/`stream_to_tempfile` pair from #257,
  now request-wide budgeted (see review fixes below).
- **Charset handling.** `chardetng` + `encoding_rs`, with precedence BOM >
  declared charset > detection, plus a Latin-1 label normalisation the
  captures require. Cooperatively chunked decode, consistent with #257's
  budget discipline.
- **Plain text extractor**, under #257's existing budgets (`Budget`,
  `BoundedCounter`, deadline).
- **HTML extractor**, built on `html5ever`'s incremental tokenizer with a
  Wayfinder-owned, budgeted `TokenSink`. No DOM is constructed anywhere —
  `scraper`/`html2text` were evaluated and rejected in the issue for pulling
  in a DOM or being unable to honor the budgets incrementally.
- **Both `extractFormat` values** (`xml`/XHTML default, `text`), matching the
  captured envelope shape byte-for-byte where the contract requires it.
- **Route registration**: `EXTRACT_PARAMS` allowlist (deliberately *not* a
  superset of `UPDATE_PARAMS` — no `commit`/indexing params, none of Tika's
  `literal.*`/`fmap.*`/`capture*`/`xpath` family), `[extraction]` config
  section wiring the four #257 budgets to real defaults, and a fourth column
  on `search_api_routes!` carrying a per-route body-limit policy so only this
  route disables the global `DefaultBodyLimit` in favour of its own
  budgeted ceiling.
- **Differential harness extended for multipart**: `manifest-multipart.tsv`,
  a loader, `normalize_extract` (with over-normalisation guard tests), a
  third hermetic runner, and a gated live counterpart
  (`WAYFINDER_DIFF_SOLR=1`).

Six commits, each carrying its own detailed rationale in the commit body —
read them directly for anything not summarised here:
`ffb09ef`, `6c437c1`, `a8b0ea2`, `0016fcd`, `6b88dcc`, `01fd1b2`.

## Evidence gap found and closed

#171 captured `extractOnly` for plain text only. Its HTML captures were the
*indexing* path (bare `responseHeader`), so #258's HTML half — and charset
precedence, which had no capture at all — had no ground truth to derive
tests from.

Five new fixtures were captured against real `solr:9.10.1` (container
`wayfinder-solr-258`, port 9020, core `extract258`, removed after capture):
`extract_html_only_xml`, `extract_html_only_text`, `extract_latin1_text`,
`extract_utf8_bom_text`, `extract_declared_charset_text`, plus two new
inputs, `sample-latin1.txt` (raw ISO-8859-1) and `sample-utf8-bom.txt`
(UTF-8 with a BOM). `solr-ref/responses/` was backed up before the capture
run, and `git status --short` afterward confirmed no existing fixture
churned — the CLAUDE.md rule against re-capturing as a side effect held.

`ffb09ef` deliberately left `src/extract.rs`'s
`budget_violation_statuses_have_no_captured_fixture_yet` (#257's
self-expiring trip-wire against a sixth `extract_*.json` fixture appearing)
red on landing, by design: #258 is the recheck that trip-wire exists to
force. It fired, and the recheck it forced is the corrected `UnsupportedFormat`
mapping described below.

## Findings 120–123 (`docs/solr-ref-findings.md`)

- **120** — `extractFormat=text` always opens with exactly thirteen
  newlines, independent of document, format, and metadata-key count (a
  plain-text fixture with 9 `meta` elements and an HTML fixture with 11 both
  produce `"\n" * 13`). Verified across 2 formats and 3 charsets. This is
  the finding that makes byte-exact reproduction of the text format
  achievable — the differing head sizes would otherwise suggest one newline
  per metadata key, and that premise is wrong.
- **121** — HTML `extractOnly` returns the same
  `{responseHeader, file, file_metadata}` envelope as plain text, with
  `title`/`author` promoted into metadata (`dc:title` + bare `title`, plus
  `author` from `<meta name="author">`).
- **122** — A declared charset beats detection, a BOM beats detection, and
  the resolved charset is echoed in three places (`Content-Encoding`
  metadata, the second `Content-Type` metadata value's `charset=`, and the
  XHTML head's `Content-Type` meta).
- **123** — `X-Parsed-By` (Java class names) is the only part of the
  envelope Wayfinder cannot honestly reproduce; ratified as PRD divergence
  10, not a to-do.

## A wrong premise in the issue, corrected

The issue asked for self-expiring `EXPECTED_DIVERGENCES` entries "removed as
the route lands." That is the wrong mechanism here. `X-Parsed-By` and Tika's
injected `shape="rect"` on the XHTML anchor rewrite can never match
byte-for-byte — they name a Java class and an implementation detail of
Tika's `captureAttr`, neither of which exists in a Rust server. These are
**permanent ratified divergences** (PRD divergence 10), handled by a narrow
comparison normaliser (`normalize_extract`) rather than a to-do list that
would sit forever waiting to "expire." Everything else in the envelope
compares exactly. Recorded here per CLAUDE.md's "don't paper over a wrong
ticket premise" — the correction is in `a8b0ea2`'s commit body and in
`docs/PRD.md`'s ratified-divergence list.

Separately, the *status* divergence for the corrupt-PDF fixture (Tika's
Java-side parse failure vs. Wayfinder's typed "no PDF extractor at all")
**is** handled the self-expiring way the issue asked for, correctly: it is
`DIVERGENT_STATUS_MULTIPART` in `tests/differential.rs`, asserted to still
diverge, and named to retire when the phase-2b PDF extractor lands. The two
mechanisms (permanent-ratified vs. self-expiring) were not interchangeable,
and the issue's text didn't distinguish them — the module now uses the
right one for each case.

## Round-1 review: two must-fix findings, both real

**1. Unbounded request body.** The route's `DefaultBodyLimit::disable()`
removed the only global cap, and the handler counted bytes only for the
first part with a non-empty filename — every part before it was drained by
`next_field()` uncounted (`file_name.is_empty() => continue`). The reviewer
demonstrated the defect directly: with `max_body_bytes = 40`, a
67,166,131-byte multipart body of non-file parts was read to completion in
256 ms and answered `400 MissingContentStream` — the server had already done
the unbounded work before rejecting anything.

Fixed in `0016fcd`: every part the handler consumes is now charged to one
request-wide `consumed` counter (`stream_to_tempfile_counted`/
`drain_counted`, both built over a private `copy_counted`), plus
`DefaultBodyLimit::max(route_body_ceiling())` restored as a transport-level
backstop — needed because `multer` consumes part *headers* before a `Field`
value exists at all, a window no handler-side byte counter can see.
`route_body_ceiling()` is `max_body_bytes` plus 1 MiB of framing headroom.
The reviewer's own re-verification of the fix (see below) is stronger
evidence than the implementor's report.

**2. `UnsupportedFormat` wrongly remapped 415 → 500.** Justified in the
first pass from `extract_corrupt_pdf.json` being a captured 500, but that
fixture is Tika throwing *while parsing* an already-recognized format
(`ExtractError::Parse`, which is correctly a 500 on its own). A
well-formed-but-unsupported format — a valid DOCX/RTF/JPEG, which real Solr
answers 200 for as a no-op extraction — was being turned into a 500,
contradicting the module's own client-caused-is-4xx rule. Worse, 415 had
been deleted from the trip-wire's uncaptured-status list, so it would never
have been rechecked against real Solr again. Restored to 415 in `0016fcd`;
the corrupt-PDF case is now the recorded, self-expiring status divergence
(`DIVERGENT_STATUS_MULTIPART`) described above.

## A flaky test caught by the orchestrator at the green gate

The implementor reported the suite green after `a8b0ea2`; an independent
re-run failed.
`extract_concurrency_over_configured_max_concurrency_is_503` failed
roughly 1 run in 5 — it fired two independent HTTP requests over a
31-byte input and hoped they would overlap in-flight, but request 1
routinely finished and released its permit before request 2 was admitted.
Stage 1 had flagged this exact gap in the test's own doc comment when it
wrote the test red.

Fixed in `6b88dcc` by making saturation a fact instead of a race:
`Permit` → `pub struct ExtractionPermit`, `try_acquire` →
`pub fn ExtractionRuntime::try_acquire_permit`, `AppState.extraction` →
`Arc<ExtractionRuntime>`, and an additive `AppServer::extraction()`
accessor. The test now holds the only permit directly, asserts the route
503s, drops the permit, and asserts the same request then 200s — no race
window at all.

Worth stating plainly as a process finding: a single green run from a
stage is not evidence of a green suite. The orchestrator's independent
re-run is what caught this, not the implementor's own report.

## Round 2: no production defect, three surviving mutations

Round 2 found no new production bug, but the reviewer's mutation testing
surfaced three gaps closed in `01fd1b2`, all "no production behaviour
changes" per that commit:

1. **The round-1 body-budget fix's central property was itself
   unguarded.** `copy_counted`'s request-wide `consumed` swapped for a
   per-call local counter — and the suite stayed green, because the
   existing non-file-parts test sends 5×100 bytes against a 40-byte cap,
   so each part alone busts the budget under *either* per-request or
   per-part accounting. `extract_body_budget_is_shared_across_parts` adds a
   60+60-against-100 case where only the sum exceeds the budget, closing
   the gap the round-1 fix itself left untested.
2. **The `> max_bytes` boundary was unguarded.** Relaxing
   `consumed + len > max_bytes` to `>=` still passed. `extract_body_
   exactly_at_max_body_bytes_is_accepted` pins the boundary from both
   sides: 99 and 100 bytes are 200, 101 is 413.
3. **An untested drive-by fix in `0016fcd` had fixed a real wire-visible
   bug.** `let ascii_only = bytes.is_ascii();` — the prior code's ASCII
   charset-label override read only the 64 KiB detection window instead of
   the whole input, which flipped `Content-Encoding` from `ISO-8859-1` to
   `UTF-8` for an all-ASCII upload at exactly 65,537 bytes.
   `charset_ascii_past_the_detection_window_keeps_the_iso_8859_1_label`
   covers it, taking its expected label from the fixture.

The reviewer also directly re-verified the round-1 flood fix rather than
trusting the fix commit's own description: a streamed 72,226,035-byte
framing-only flood against a 40-byte `max_body_bytes` limit is rejected
after the server pulls 1,056,768 bytes (1.46% of the flood) in 152 ms —
the transport-level `DefaultBodyLimit` backstop firing as designed, well
before the handler's own byte counter would have needed to.

`01fd1b2` also corrects two stale comments describing pre-fix behaviour:
`config::Extraction::max_body_bytes`'s doc comment had claimed the route
disables the global limit and `stream_to_tempfile` enforces it (true before
`0016fcd`, false after), and `tests/extract_route.rs`'s module doc still
announced the whole file as RED with no route in existence.

Two review rounds total — the stage's default cap. Given round 2 still
found real gaps (not just style), this substrate would benefit from a
further pass rather than treating two rounds as closing the question,
consistent with the #257 report's own closing note.

## The `&mut Budget` encapsulation gap: not closed, and correctly not attempted here

The #257 report's closing note asked for "a fresh look at the `&mut Budget`
encapsulation gap when the first real extractor lands." #258 lands the
first two real extractors (plain text, HTML) and — checked directly in
`src/extract.rs`'s `Budget` doc comment (`src/extract.rs:1711-1757`) — the
gap is **still open, by explicit decision, not oversight**. The doc comment
was extended (not the type), reasoning as follows: the six structural
counters were already made unforgeable in #257's round 2 (private,
`Cell`-backed, driven only through delegating methods that can't raise a
limit or reach an illegal `decrement`). What remains open is that
`Extractor::extract` still takes `&mut Budget`, so an in-tree extractor can
still reassign the *whole* budget wholesale
(`*budget = Budget::new(ExtractLimits { .. })`) and mint itself fresh
counters and a fresh deadline.

The comment states why this issue did not close it: switching the trait to
`&Budget` requires putting the output `String` behind interior mutability
(`RefCell`), which changes `output_text()`'s return type from `&str` to
something like `Ref<'_, String>` — a shape decision the authors did not
want to make from one call site's needs (`PlainTextExtractor`) before a
second, differently-shaped one (`HtmlSink`, which needed `&self` for
`TokenSink` anyway and drives its own interior-mutable state) existed to
design against. With HTML now landed, there are two real call sites, so
this decision is now unblocked in a way it wasn't at #257 — but #258 did
not spend its own scope on it. Documented as a `ponytail:` naming the exact
upgrade (`Extractor::extract(&self, input, budget: &Budget)`) and its
trigger ("the first extractor that does not need to read its own output
back"). This is a correct, disclosed deferral, not an omission — but it
means the #257 reviewer's ask is still open one issue later, now with two
real call sites available to design against, and should not be deferred a
third time without a decision.

## Follow-ups (deferred, not fixed)

1. **Unbounded `<title>` accumulation.** `HtmlSink`'s title text is
   accumulated via `state.title.get_or_insert_with(String::new).push_str(chars)`
   (`src/extract.rs:1160`) with no budget check — a document with an
   arbitrarily long `<title>` element can grow that buffer without limit
   even while every other budget is enforced.
2. **`max_body_bytes` × HTTP concurrency is a RAM multiplier.** Documented
   in-line at `src/lib.rs:2384-2397`: each concurrent extraction can hold up
   to `max_body_bytes` (32 MiB default) resident, and nothing bounds
   `max_body_bytes` × (concurrent HTTP connections) globally — only
   `ExtractionRuntime`'s own concurrency slots are capped, not the
   HTTP-layer intake ahead of them.
3. **`json.nl=map` is accepted and ignored** on the extract route
   (`EXTRACT_PARAMS` allows it per `src/lib.rs:308`, consistent with other
   routes, but the extract handler's own response shape never varies on
   it) — same pattern as other endpoints, not new to this issue, but worth
   naming since it's on a fresh param allowlist.
4. **The html5ever early-abort check is untested.**
   `|| tokenizer.sink.state.borrow().error.is_some()` at `src/extract.rs:885`
   is correct and load-bearing — reverting it leaves the full suite green,
   because it only matters for a budget exhausted mid-character-run with no
   following tag token to carry `TokenSinkResult::Script`. Needs a test
   that exhausts a budget inside a long character run with no subsequent
   tag.
5. **`AppServer::extraction()` (`src/lib.rs:139`) is now public surface**
   that hands back an `Arc<ExtractionRuntime>` that can outlive the
   `AppState` it came from, deferring `ExtractionRuntime::drop` and keeping
   its OS threads alive as long as any clone is held; a `mem::forget`-ed
   permit obtained through it permanently burns a concurrency slot.
   Harmless in-tree (it exists for exactly one test today) but it is public
   API now and wants a test-support note on the accessor saying so.
6. **Cosmetic line-wrapping.** `tests/differential.rs:126`
   (`DIVERGENT_STATUS_MULTIPART`'s reason string) and the
   `failures.push(format!(...))` strings around `:2578`/`:2584`/`:2594`
   line-wrap with leading indentation preserved, producing 5–6-space runs
   mid-sentence in diagnostic output (confirmed directly, e.g. "...is a
   500; Wayfinder has      no PDF extractor..."). Cosmetic only — does not
   affect what the strings assert.

## Green evidence (re-run at `01fd1b2`, not copied from an earlier claim)

- `cargo test --no-fail-fast` — **1050 passed (55 suites)**, 64.14s,
  hermetic (no network, no Docker).
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact
  invocation).
- `cargo test --test extract_route`, run 10× in direct succession after the
  `6b88dcc` determinism fix — 29 passed / 0 failed on every run. (Separately
  run 20× at an earlier point in the task per the task record; this
  report's own re-run is the 10×.)

## Review rounds

Two rounds, the stage's default cap:

- Round 1 (`0016fcd`) — two must-fix findings (unbounded request body,
  wrongly-remapped `UnsupportedFormat` status), both closed; a
  determinism defect in the concurrency test was separately caught by the
  orchestrator's own re-run (not the reviewer) and closed in `6b88dcc`.
- Round 2 (`01fd1b2`) — no new production defect, but three surviving
  mutations against the round-1 fix and one untested drive-by bug fix,
  all closed; two stale doc comments corrected.

Per the #257 precedent and this issue's own round 2 (which still found
real coverage gaps, not just polish), this substrate would benefit from
further review passes rather than treating two rounds as final — most
pointedly on the `&mut Budget` decision above, which is now unblocked but
still open.
