# Issue #257 — phase 0: extraction budgets + internal extractor contract

Date: 2026-08-02
Branch: `markdlabrecque/issue-257-phase-0-extraction`
Follow-up to: `docs/reports/2026-08-01-text-extraction-exploration.md` (#171 exploration)

## What shipped

`src/extract.rs` (new) plus `tests/extraction.rs` (new) and one line in
`src/lib.rs` (`pub mod extract;`). **No HTTP route, no format parser beyond
plain text, no new dependency.** Verified directly rather than assumed:
`git diff main -- Cargo.toml Cargo.lock` is empty across every commit on this
branch (`8c55098`, `0d70c72`, `ce43a24`, `b00834a`) — the module is built
entirely on `std`, `tokio`, `axum`, and `tempfile`, all already
non-dev-dependencies.

**The extractor contract** (deliverable 1):

- `ContentType` — `PlainText`, `Html`, `Xml`, `Zip` (one variant for the whole
  OOXML/ODF family — a ZIP container's specific flavour cannot be told apart
  from magic bytes alone, only from `[Content_Types].xml`/`mimetype` inside
  the archive, which phase 0 cannot open), `Rtf`, `Pdf`, `LegacyOle` (binary
  DOC/PPT/XLS), `Unknown`.
- `detect(declared_type, resource_name, leading_bytes) -> ContentType` —
  signature-first dispatch. Required signatures implemented: `%PDF-`,
  `PK\x03\x04`, `{\rtf`, OLE2 CFB (`D0 CF 11 E0 A1 B1 1A E1`), plus a `<?xml`
  extra. OLE2 is checked *before* the ZIP signature so a legacy binary file
  can never be misrouted to a ZIP/OOXML path. Declared MIME type and filename
  extension are weaker fallbacks, checked in that order.
- `dispatch(ContentType) -> Option<&'static dyn Extractor>` — only
  `PlainText` resolves to a real extractor; every other variant, including
  `LegacyOle`, is `None` and surfaces as a typed
  `ExtractError::UnsupportedFormat { content_type }`, never a misparse.
- `Extractor` trait — object-safe, one method:
  `fn extract(&self, &ExtractInput, &mut Budget) -> Result<Extracted, ExtractError>`.
- `ExtractMetadata` — the narrow set the spec asked for and nothing else:
  `resource_name`, `content_type`, `title: Option<String>`,
  `author: Option<String>`.
- The one real extractor, `PlainTextExtractor`, decodes UTF-8 cooperatively in
  8 KiB chunks (`DECODE_CHUNK_BYTES`), checking the deadline before the loop
  and between every chunk, never `String::from_utf8(whole_thing)`. Invalid
  sequences use `from_utf8_lossy` semantics (replacement characters), and a
  chunk boundary is never allowed to split a multi-byte scalar
  (`decode_chunk_len`).

**The budgets** (deliverable 2), all under one `ExtractLimits` with a
documented per-field default and one `Default` impl:

- Max HTTP body bytes (32 MiB default), enforced by `stream_to_tempfile`
  while streaming into a `NamedTempFile` — checked before every write, so the
  temp file never exceeds the limit even transiently, and the function never
  consults a declared `Content-Length` at all (so a dishonest or absent one
  cannot get around it — it is architecturally irrelevant to this guard).
- Concurrency (4 slots default) + a dedicated bounded blocking pool
  (`ExtractionRuntime`): reject-when-full (`ExtractError::TooBusy`), never an
  unbounded queue.
- Max extracted output, both Unicode scalar count (10M default) and byte
  count (40MB default), checked incrementally in `Budget::push_str` — a
  rejected push leaves the accumulated output untouched.
- A cooperative wall-clock deadline (30s default), `Budget::check_deadline`/
  `remaining`, backed by an injectable `Clock` so cooperative cancellation is
  testable deterministically rather than raced against a real sleep.
- Five ZIP guards over entry *metadata* only (no zip reader — phase 0 has no
  new dependency, and the `zip` crate arrives in phase 2a): entry count
  (4096), path validation (rejects absolute paths, `..` traversal anywhere in
  the path, backslash separators, drive letters, embedded NUL), per-entry
  uncompressed bytes (128 MiB), cumulative uncompressed bytes across entries
  (512 MiB), and compression ratio (200:1), judged from declared metadata
  before any byte is decompressed.
- Six structural counters sharing one `BoundedCounter` type (XML depth, XML
  events, sheets, cells, RTF group nesting, PDF pages), each with its own
  default and its own `StructuralLimitKind` identity in the rendered error.

**The error taxonomy** (deliverable 3): `ExtractError` with variants
`UnsupportedFormat`, `TooBusy`, `BodyTooLarge`, `OutputTooLarge`,
`DeadlineExceeded`, `ZipBudget`, `StructuralLimit`, `Parse`, `Io`, and a
`From<ExtractError> for WfError` mapping each onto an HTTP status, an error
class, and `Envelope::NoParams`.

## Two design decisions worth recording

**1. The blocking pool is hand-rolled, not `tokio::task::spawn_blocking` /
`tokio::sync::Semaphore`.** `ExtractionRuntime` spawns one *named* OS thread
per configured concurrency slot (`wayfinder-extract-{i}`), feeds jobs through
an `mpsc::Sender<Job>`, and gates admission with an `AtomicUsize` try-acquire
(`try_acquire`/`Permit`) rather than a semaphore. Both departures follow from
the same requirement: isolation from tokio's *shared* blocking pool is the
entire point, and `spawn_blocking` would put a wedged parser right back on
the pool `/select`, `/update`, and everything else depends on. And the policy
is reject-when-full, not wait-in-line — `tokio::sync::Semaphore`'s async
waiting machinery would never run under that policy, so a plain
compare-exchange in `std` is the whole behavior needed, with no extra tokio
feature to depend on. A hand-rolled `std`-only oneshot
(`OneshotTx`/`OneshotRx`, `Waker`-based) hands the worker-thread result back
to the async caller for the same reason: no extra tokio feature, no new
dependency.

**2. Cancellation is cooperative only; `tokio::time::timeout(spawn_blocking(...))` is deliberately absent.** Per the #171 exploration report,
dropping a join handle does not stop the thread behind it — that shape would
report a timeout to the caller while the runaway parser keeps burning a
pool thread forever. Instead, `Budget::check_deadline`/`remaining` report
elapsed wall-clock time to extractors that choose to consult it; the
plain-text extractor does, between every decode chunk. **Residual risk,
stated in the module doc comment**: an opaque parser (the case PDF will be)
that never checks a deadline cannot be hard-killed in-process. Containment
is structural, not a cancellation guarantee: `ExtractionRuntime` owns fixed
dedicated threads, so a wedged parser can at worst fill the extraction pool
and cause new work to be rejected with `TooBusy`, while tokio's runtime and
shared blocking pool keep running. The named upgrade path (in the module
doc comment): either a parser that exposes per-page/per-checkpoint progress,
or moving extraction into a separate OS process that can actually be killed
— revisit when PDF (a #171 follow-up) lands, not before.

## Guard -> test evidence table

20 guards named in the spec's acceptance bar (10 named budgets + 6 structural
counters + 4 review-round-1 additions), each mutation-tested: break the
guard deliberately (flip a comparison, raise a limit, remove a
check/`catch_unwind`), confirm the named test fails, revert.

| # | Guard | Test (file) | Mutation performed |
|---|---|---|---|
| 1 | Body bytes, streamed | `stream_to_tempfile_rejects_once_running_total_exceeds_the_limit` (tests/extraction.rs) | Raised the `> max_bytes` comparison to `>=`-tolerant / removed the check |
| 2 | Body bytes never buffers past limit+1 chunk | `stream_to_tempfile_never_writes_past_limit_plus_one_chunk` (tests/extraction.rs) | Moved the check to after draining the whole source |
| 3 | Concurrency reject (`TooBusy`) | `extraction_runtime_rejects_the_n_plus_first_concurrent_extraction` (tests/extraction.rs) | Raised the permit count / removed `try_acquire`'s zero check |
| 4 | Concurrency permit release on success | `extraction_runtime_returns_every_slot_once_the_extractions_complete` (tests/extraction.rs) | Removed the `Drop for Permit` increment |
| 5 | Panic containment keeps the pool at full strength | `extraction_runtime_contains_a_panicking_parser_and_keeps_the_pool_at_full_strength` (tests/extraction.rs) | Removed `catch_unwind` around the job closure — **independently re-mutated by the reviewer**; failed the named test |
| 6 | Dedicated named-thread pool, not tokio's shared blocking pool | `extraction_runs_on_a_dedicated_named_thread_not_the_shared_tokio_blocking_pool` (tests/extraction.rs) | Swapped the worker-thread name / routed through `spawn_blocking` |
| 7 | Output scalar count | `budget_push_str_rejects_over_the_scalar_limit` (tests/extraction.rs) | Changed `>` to `>=` tolerance on the scalar check |
| 8 | Output byte count | `budget_push_str_rejects_over_the_byte_limit` (tests/extraction.rs) | Changed the byte-check comparison / dropped it |
| 9 | Deadline, basic expiry | `budget_check_deadline_reports_exceeded_after_the_configured_wall_clock` (tests/extraction.rs) | Changed `>=` to `>` in `check_deadline` |
| 10 | Deadline, cooperative mid-decode | `plain_text_extractor_stops_mid_decode_when_the_deadline_expires_between_chunks` (tests/extraction.rs) | Removed the between-chunks `check_deadline` call inside the decode loop — **independently re-mutated by the reviewer**; failed the named test |
| 11 | Zip entry count | `zip_budget_rejects_over_the_configured_entry_count` (tests/extraction.rs) | Changed `>=` to `>` on `entries_seen` |
| 12 | Zip path validation (absolute/`..`/backslash/drive-letter/NUL) | `zip_budget_rejects_every_kind_of_unsafe_entry_path` (tests/extraction.rs) | Removed one clause of `is_safe_entry_path` at a time |
| 13 | Zip per-entry uncompressed bytes | `zip_budget_rejects_a_single_entry_over_the_per_entry_byte_limit` (tests/extraction.rs) | Raised/removed the per-entry comparison |
| 14 | Zip cumulative uncompressed bytes | `zip_budget_rejects_once_cumulative_uncompressed_bytes_exceed_the_limit` (tests/extraction.rs) | Removed the cumulative check or let a rejected entry still add to the running total |
| 15 | Zip compression ratio (42.zip-shaped) | `zip_budget_rejects_a_42_zip_shaped_compression_ratio` (tests/extraction.rs) | Raised `zip_max_compression_ratio` comparison threshold / dropped the ratio check |
| 16 | Structural: XML depth | `bounded_counter_rejects_the_increment_past_its_limit_xml_depth` (tests/extraction.rs) | Changed `BoundedCounter::increment`'s `>=` to `>` — **independently re-mutated by the reviewer**; failed the named test (and its five siblings) |
| 17 | Structural: XML events | `bounded_counter_rejects_the_increment_past_its_limit_xml_events` (tests/extraction.rs) | Same `BoundedCounter` mutation |
| 18 | Structural: sheets | `bounded_counter_rejects_the_increment_past_its_limit_sheets` (tests/extraction.rs) | Same `BoundedCounter` mutation |
| 19 | Structural: cells | `bounded_counter_rejects_the_increment_past_its_limit_cells` (tests/extraction.rs) | Same `BoundedCounter` mutation |
| 20 | Structural: RTF group depth / PDF pages | `bounded_counter_rejects_the_increment_past_its_limit_rtf_group_depth`, `bounded_counter_rejects_the_increment_past_its_limit_pdf_pages` (tests/extraction.rs) | Same `BoundedCounter` mutation |

Because all six structural counters share one `BoundedCounter::increment`,
mutating that one method is a single mutation with six named tests as
independent witnesses (rows 16-20); the table lists all six test names for
completeness even though the underlying code path mutated once.

The suite carries 12 additional tests beyond these 20 mutation entries:
pure-helper/round-trip tests (`decode_chunk_len_*`, lossy-decode, multi-chunk
round trip, `detect` fallback ordering, `bounded_counter_decrement_is_saturating`,
`zip_ratio_guard_rejects_an_entry_declaring_zero_compressed_bytes`,
`zip_admit_does_not_charge_a_rejected_entry_to_the_cumulative_total`,
`budget_push_str_leaves_output_untouched_when_it_would_overflow`,
`budget_remaining_reaches_zero_after_the_deadline` — all in `src/extract.rs`'s
`#[cfg(test)] mod tests`), the OLE2 typed-`UnsupportedFormat` trio, three
signature-dispatch tests, and the fixture-derived envelope test below. These
are correctness tests, not guard-violation mutation entries.

Total: 32 tests in `tests/extraction.rs` (28 from the red-test commit
`8c55098`, +4 from the round-1 review fix `ce43a24`), plus 12 unit tests for
pure helpers inside `src/extract.rs`.

## Verified against the fixture

`Parse -> 500` (`extract_error_parse_maps_to_the_corrupt_pdf_envelope_shape`)
is the **only** mapping with captured ground truth:
`solr-ref/responses/extract_corrupt_pdf.json` — HTTP 500,
`responseHeader.status=500`, `error.code=500`, no `params` echo (confirmed by
premise-check: `Envelope::NoParams` renders that shape, and the extract
handler is an `/update` path, which never echoes params), and a non-empty
`error.metadata` array. `tests/common/diff.rs` drops `error.trace` in the
comparison normaliser, so a metadata-without-trace response is not treated as
a divergence — `msg` text and Java stack content are documented as
non-contractual (findings 10/59); code and envelope shape are.

`413` (`BodyTooLarge`), `503` (`TooBusy`, `DeadlineExceeded`), `415`
(`UnsupportedFormat`), and `400` (`OutputTooLarge`/`ZipBudget`/
`StructuralLimit`) are **reasoned, uncaptured choices** — Solr's extract
handler was never provoked into a budget violation during the #171 capture,
so there is no fixture to check them against. Each is commented in
`impl From<ExtractError> for WfError` with its reasoning (client-caused
document defect = 4xx, server-capacity issue = 5xx). These are recorded here
explicitly as **to-verify-when-the-route-lands**. The self-expiring guard
enforcing that recheck is
`budget_violation_statuses_have_no_captured_fixture_yet`
(`src/extract.rs`'s test module): it enumerates the five extraction fixtures
that exist today and fails the moment a sixth `extract_*.json` fixture
appears in `solr-ref/responses/`, naming exactly which mappings must be
re-checked against real Solr before the note can be considered still valid.

## Known limitations and follow-ups

- **Guard integrity is by review, not by the type system.** `Budget`,
  `BoundedCounter`, and `ZipBudget` fields are private with read-only
  accessors, which stops *accidental* mutation, but anything holding
  `&mut Budget` can still reassign a whole counter or the whole budget
  (`budget.xml_depth = BoundedCounter::new(usize::MAX)`). The enforcing fix —
  delegating methods on `Budget` (`enter_xml_element()`,
  `count_xml_event()`, etc.) plus an `Extractor` signature taking `&Budget`
  instead of `&mut Budget` — is deferred to the first extractor that
  actually drives a structural counter (#171 follow-up, OOXML), where the
  method set can be designed against real call sites. This overclaim was
  caught and corrected in the doc comment itself by `b00834a`, after the
  implementor's own comment initially asserted the fields were unforgeable.
- **No `[extraction]` config section.** Limits are `ExtractLimits::default()`
  until a route exists to make them configurable — deliberately skipped per
  the task spec; belongs with the route issue (`/update/extract`, #171
  follow-up).
- **`<?xml` outranks the declared MIME type.** An XHTML document declared
  `text/html` but opening with an XML declaration detects as `Xml`, not
  `Html`, and would be silently unavailable to the phase-1 HTML extractor
  once it exists. Documented as a `ponytail:` comment in `detect_by_signature`
  naming the fix (look past the declaration for an `<html` root/XHTML
  doctype and prefer `Html`); harmless today because neither variant has an
  extractor yet. Belongs with the phase-1 HTML extractor issue.
- **`decrement` is available on cumulative counters where it must never be
  used.** `BoundedCounter::decrement` is meant only for the three
  depth-shaped counters (`xml_depth`, `rtf_group_depth`, and any future
  nesting counter); the three cumulative counters (`xml_events`, `cells`,
  `pdf_pages`) must never call it, or a document could alternate
  increment/decrement forever and defeat the bound. This is doc-only today —
  the type system does not enforce the split. Deferred `DepthCounter` split
  documented as a `ponytail:` comment; belongs with the first phase that
  drives XML nesting for real (#171 follow-up, OOXML).
- **Upload `write_all` is synchronous inside an async fn, deliberately.**
  `stream_to_tempfile` calls a blocking `write_all` per chunk rather than
  `tokio::fs`, because `tokio::fs` would dispatch to the shared blocking pool
  this module exists to stay off. Documented upgrade path if 32 MiB of inline
  writes ever measurably stalls a runtime worker: move the whole
  receive-to-tempfile step onto `ExtractionRuntime`'s own pool.
- **Deferred test coverage**, not written in phase 0: exact-limit `>` vs
  `>=` boundary cases (does the Nth item exactly at the limit succeed and the
  N+1th fail, rather than off-by-one somewhere), a 2x-peak-resident-memory
  probe for the output-text budget, `OneshotTx::send` wake ergonomics under
  contention, `Instant` overflow behavior on an absurdly large configured
  deadline, and tightening the corrupt-PDF metadata assertion from "array is
  non-empty" to the alternating error-class/root-error-class shape the
  fixture actually has.
- **The reviewer's closing note**: this is a security/resource-control
  substrate that received exactly two review rounds (the stage's default
  cap), and the reviewer recommends a fresh look specifically at the
  `Extractor` signature (the `&mut Budget` encapsulation gap above) when the
  first real extractor lands, rather than treating two rounds as closing the
  question.

## Process honesty

Two incidents from this task, recorded factually:

1. **The implementor destroyed its own uncommitted implementation.** Mid-task,
   it ran `git checkout -- src/extract.rs`, discarding the working
   implementation before it was committed, and rebuilt the module from
   context. Because of this, the reviewer was explicitly told to treat
   `src/extract.rs` as new code rather than a diff against prior working
   state it could partially trust.
2. **The implementor later truncated its own mutation-test harness**,
   producing one run that reported "empty output, exit 0" — a false pass that
   is not evidence of anything. Only the final mutation runs performed
   against the committed baseline (the state in `0d70c72`/`ce43a24`) count as
   evidence; the truncated run is disclosed here, not counted.

Given incident 2, the guard claims in the table above rest most solidly on
the **reviewer's independent re-mutation** of three guards during round 1 —
the between-chunks deadline check, the `BoundedCounter` `>=`-to-`>` mutation,
and removing `catch_unwind` around the pool job — each of which failed the
correct named test when the reviewer broke it directly, rather than relying
on the implementor's self-reported mutation runs for those three.

## Green evidence (as of `b00834a`)

Re-run directly rather than copied from an earlier claim:

- `cargo test` — **991 passed (53 suites)**, 63.7s, hermetic (no network, no
  Docker).
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact
  invocation).

## Review rounds

Two rounds, the stage's default cap: round 1 (`ce43a24`) added the four
tests in rows 4, 5, 6, and 10 of the guard table above and moved `Budget`/
`BoundedCounter`/`ZipBudget` state to private+read-only; round 2 was the
doc-comment correction in `b00834a`. Per the reviewer's closing note above,
this substrate would benefit from further review passes once a real
extractor exercises the `&mut Budget` encapsulation gap, rather than treating
two rounds as final.
