//! Issue #257 — phase 0 extraction budgets + internal extractor contract.
//!
//! No HTTP route exists yet, so every test here calls `wayfinder::extract`
//! directly rather than going through `common::indexed_app()`. One
//! deliberate-violation test per budget named in the task spec's acceptance
//! bar:
//!
//! - body bytes (streaming, dishonest/absent Content-Length)
//! - concurrency / `TooBusy`
//! - output scalar count
//! - output byte count
//! - deadline
//! - zip entry count, zip path traversal, zip per-entry bytes, zip
//!   cumulative bytes, zip compression ratio
//! - each structural counter (XML depth, XML events, sheets, cells, RTF
//!   group nesting, PDF pages)
//!
//! Plus: signature dispatch beats a wrong declared MIME type; an OLE2
//! legacy file returns a typed `UnsupportedFormat` and is never routed to a
//! ZIP path; the plain-text extractor round-trips and respects
//! deadline+output budgets; `ExtractError` maps to a `WfError` whose
//! rendered envelope matches `solr-ref/responses/extract_corrupt_pdf.json`'s
//! shape.
//!
//! Every expected value in the fixture-derived test comes from that
//! committed fixture, never from what an implementation would produce
//! (repo convention, `docs/solr-ref-findings.md` finding 10).
//!
//! `ExtractLimits::default()` itself is not asserted here: phase 0 has no
//! captured value to derive an expectation from (no route exists to
//! observe it through), and the task spec only requires "documented
//! per-field rationale" in code comments, not a specific default value.
//! Every test below builds its own explicit `ExtractLimits` fixture instead
//! of relying on `Default`.

mod common;

use std::collections::VecDeque;
use std::time::Duration;

use axum::body::Bytes;
use serde_json::Value;
use tempfile::NamedTempFile;

use wayfinder::extract::{
    BoundedCounter, Budget, ChunkSource, ContentType, ExtractError, ExtractInput, ExtractLimits,
    Extractor, OutputLimitKind, PlainTextExtractor, StructuralLimitKind, ZipBudget, ZipEntryMeta,
    ZipViolation, detect, dispatch, extract, stream_to_tempfile,
};

use common::fixture;

/// A permissive baseline: every limit generous enough that no test
/// accidentally trips a budget it isn't testing. Individual tests override
/// just the field(s) under test.
fn permissive_limits() -> ExtractLimits {
    ExtractLimits {
        max_body_bytes: 1_000_000_000,
        // Not "generous" like the rest: this one costs an OS thread per
        // slot, since `ExtractionRuntime::new` spawns a dedicated worker per
        // unit of concurrency. Kept small deliberately; the concurrency
        // tests override it with the exact value they need anyway.
        max_concurrency: 4,
        max_output_scalars: 1_000_000,
        max_output_bytes: 1_000_000,
        deadline: Duration::from_secs(60),
        zip_max_entries: 1_000_000,
        zip_max_entry_bytes: 1_000_000_000,
        zip_max_cumulative_bytes: 1_000_000_000,
        zip_max_compression_ratio: 1_000.0,
        max_xml_depth: 1_000_000,
        max_xml_events: 1_000_000,
        max_sheets: 1_000_000,
        max_cells: 1_000_000,
        max_rtf_group_depth: 1_000_000,
        max_pdf_pages: 1_000_000,
    }
}

// ---------------------------------------------------------------------
// Budget 1 — max HTTP body bytes, enforced while streaming
// ---------------------------------------------------------------------

/// A `ChunkSource` fed from a fixed list of chunks, with no declared length
/// at all: `stream_to_tempfile`'s signature never takes one, so this is the
/// only shape a "dishonest or absent Content-Length" can take at this
/// layer — the guard can only ever see actually-delivered bytes.
struct VecChunkSource {
    chunks: VecDeque<Bytes>,
    calls: usize,
}

impl VecChunkSource {
    fn of_chunks(chunks: Vec<Bytes>) -> Self {
        VecChunkSource {
            chunks: chunks.into(),
            calls: 0,
        }
    }

    /// An effectively unbounded source: yields the same 4 KiB chunk forever.
    /// Models a client that keeps sending regardless of any declared
    /// length — a "dishonest Content-Length" (too low) or an absent one
    /// looks identical to this guard, since it never consults either.
    fn unbounded() -> Self {
        VecChunkSource {
            chunks: VecDeque::new(),
            calls: 0,
        }
    }
}

impl ChunkSource for VecChunkSource {
    async fn next_chunk(&mut self) -> Option<std::io::Result<Bytes>> {
        self.calls += 1;
        if self.chunks.is_empty() && self.calls < 1_000_000 {
            // "unbounded()" case: always another 4 KiB chunk available.
            return Some(Ok(Bytes::from(vec![b'x'; 4096])));
        }
        self.chunks.pop_front().map(Ok)
    }
}

#[tokio::test]
async fn stream_to_tempfile_rejects_once_running_total_exceeds_the_limit() {
    let mut source = VecChunkSource::of_chunks(vec![
        Bytes::from(vec![b'a'; 4096]),
        Bytes::from(vec![b'b'; 4096]),
        Bytes::from(vec![b'c'; 4096]),
    ]);
    let mut dest = NamedTempFile::new().expect("create temp file");
    let max_bytes: u64 = 6000; // between one and two 4096-byte chunks

    let result = stream_to_tempfile(&mut source, &mut dest, max_bytes).await;

    assert!(
        matches!(result, Err(ExtractError::BodyTooLarge { .. })),
        "streaming a body past max_bytes must fail with BodyTooLarge, got {result:?}"
    );
}

#[tokio::test]
async fn stream_to_tempfile_never_writes_past_limit_plus_one_chunk() {
    // Mutation-check companion: proves the guard checks incrementally
    // rather than buffering the whole (here: unbounded) body and measuring
    // it afterward. A guard that only checked after fully draining the
    // source would hang here (or blow memory) instead of returning.
    let mut source = VecChunkSource::unbounded();
    let mut dest = NamedTempFile::new().expect("create temp file");
    let max_bytes: u64 = 10_000;
    let chunk_size: u64 = 4096;

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        stream_to_tempfile(&mut source, &mut dest, max_bytes),
    )
    .await
    .expect("stream_to_tempfile must not hang draining an unbounded source");

    assert!(
        matches!(result, Err(ExtractError::BodyTooLarge { .. })),
        "an unbounded source must still be rejected as BodyTooLarge, got {result:?}"
    );

    let written = std::fs::metadata(dest.path())
        .expect("temp file must exist")
        .len();
    assert!(
        written <= max_bytes + chunk_size,
        "temp file grew to {written} bytes, more than max_bytes ({max_bytes}) plus one chunk \
         ({chunk_size}) — the guard must stop as soon as the running total exceeds the limit, \
         not after buffering further"
    );
}

// ---------------------------------------------------------------------
// Budget 2 — concurrency semaphore + dedicated blocking pool
// ---------------------------------------------------------------------

#[tokio::test]
async fn extraction_runtime_rejects_the_n_plus_first_concurrent_extraction() {
    let mut limits = permissive_limits();
    limits.max_concurrency = 2;
    let runtime = std::sync::Arc::new(wayfinder::extract::ExtractionRuntime::new(&limits));

    // Two slow (but bounded) "extractions" that hold their concurrency slot
    // for a few ms, so a third submitted while they are in flight lands
    // squarely inside the over-the-limit window.
    let held_a = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            runtime
                .spawn_extraction(Duration::from_secs(5), || {
                    std::thread::sleep(Duration::from_millis(50));
                    "a"
                })
                .await
        })
    };
    let held_b = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            runtime
                .spawn_extraction(Duration::from_secs(5), || {
                    std::thread::sleep(Duration::from_millis(50));
                    "b"
                })
                .await
        })
    };
    // Give the two slow extractions a moment to actually acquire their
    // slots before the third is attempted.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let third = runtime
        .spawn_extraction(Duration::from_secs(5), || "c")
        .await;

    assert!(
        matches!(third, Err(ExtractError::TooBusy)),
        "a 3rd concurrent extraction over a max_concurrency of 2 must be rejected with TooBusy \
         (reject policy, never an unbounded queue), got {third:?}"
    );

    let a = held_a.await.expect("task a must not panic");
    let b = held_b.await.expect("task b must not panic");
    assert_eq!(
        a.ok(),
        Some("a"),
        "extraction a must have run to completion"
    );
    assert_eq!(
        b.ok(),
        Some("b"),
        "extraction b must have run to completion"
    );
}

/// Review round 1, item 2. The `TooBusy` test above proves a slot is *taken*
/// but not that it is ever *given back*: a `Permit` whose `Drop` never
/// incremented the counter would produce identical results there, and the
/// pool would then wedge permanently after the first `max_concurrency`
/// extractions. This binds the release path on the success path.
#[tokio::test]
async fn extraction_runtime_returns_every_slot_once_the_extractions_complete() {
    let mut limits = permissive_limits();
    limits.max_concurrency = 2;
    let runtime = std::sync::Arc::new(wayfinder::extract::ExtractionRuntime::new(&limits));

    // Fill the pool, then drain it.
    let first = runtime
        .spawn_extraction(Duration::from_secs(5), || "a")
        .await;
    let second = runtime
        .spawn_extraction(Duration::from_secs(5), || "b")
        .await;
    assert_eq!(first.ok(), Some("a"));
    assert_eq!(second.ok(), Some("b"));

    // Every slot must be free again: `max_concurrency` fresh extractions in
    // a row, each of which would be `TooBusy` if a permit had leaked.
    for round in 0..limits.max_concurrency {
        let again = runtime
            .spawn_extraction(Duration::from_secs(5), || "c")
            .await;
        assert_eq!(
            again.as_ref().ok(),
            Some(&"c"),
            "round {round}: a completed extraction must return its concurrency slot, so the pool \
             admits max_concurrency fresh extractions again; got {again:?}"
        );
    }
}

/// Review round 1, item 3. Panic containment is what keeps the pool at full
/// strength: an uncaught panic would unwind the worker thread out of its
/// receive loop, permanently shrinking the pool by one and (with the permit
/// never released) leaking a slot as well.
#[tokio::test]
async fn extraction_runtime_contains_a_panicking_parser_and_keeps_the_pool_at_full_strength() {
    let mut limits = permissive_limits();
    limits.max_concurrency = 2;
    let runtime = std::sync::Arc::new(wayfinder::extract::ExtractionRuntime::new(&limits));

    let panicked = runtime
        .spawn_extraction(Duration::from_secs(5), || panic!("parser exploded"))
        .await
        .map(|(): ()| ());
    assert!(
        matches!(panicked, Err(ExtractError::Parse(_))),
        "a panicking parser must surface as ExtractError::Parse, not unwind the worker or hang \
         the caller; got {panicked:?}"
    );

    for round in 0..limits.max_concurrency {
        let after = runtime
            .spawn_extraction(Duration::from_secs(5), || "ok")
            .await;
        assert_eq!(
            after.as_ref().ok(),
            Some(&"ok"),
            "round {round}: after a panicking extraction the pool must still admit and run \
             max_concurrency extractions — the worker survived and the slot came back; \
             got {after:?}"
        );
    }
}

/// Review round 1, item 11. The isolation claim in the module docs is that
/// extraction runs on *its own* threads, not tokio's shared blocking pool.
/// Nothing else in this suite would notice if `spawn_extraction` were
/// quietly reimplemented on `tokio::task::spawn_blocking`; the thread name
/// is the observable that distinguishes them.
#[tokio::test]
async fn extraction_runs_on_a_dedicated_named_thread_not_the_shared_tokio_blocking_pool() {
    let mut limits = permissive_limits();
    limits.max_concurrency = 1;
    let runtime = wayfinder::extract::ExtractionRuntime::new(&limits);

    let name = runtime
        .spawn_extraction(Duration::from_secs(5), || {
            std::thread::current()
                .name()
                .map(str::to_string)
                .unwrap_or_default()
        })
        .await
        .expect("extraction must run");

    assert!(
        name.starts_with("wayfinder-extract-"),
        "extraction must run on a thread from Wayfinder's own extraction pool (a wedged parser \
         must not be able to consume tokio's shared blocking pool), but it ran on a thread named \
         {name:?}"
    );
}

// ---------------------------------------------------------------------
// Budget 3 — max extracted output: Unicode scalar count
// ---------------------------------------------------------------------

#[tokio::test]
async fn budget_push_str_rejects_over_the_scalar_limit() {
    let mut limits = permissive_limits();
    limits.max_output_scalars = 5;
    // Deliberately not the byte-limit test: ASCII text, so scalar count and
    // byte count are equal — this isolates the scalar guard.
    let mut budget = Budget::new(limits);

    for _ in 0..5 {
        budget
            .push_str("a")
            .expect("pushing up to the scalar limit must succeed");
    }
    let result = budget.push_str("a");
    assert!(
        matches!(
            result,
            Err(ExtractError::OutputTooLarge(OutputLimitKind::Scalars))
        ),
        "the 6th scalar over a max_output_scalars of 5 must fail as OutputTooLarge(Scalars), \
         got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Budget 3 — max extracted output: byte count
// ---------------------------------------------------------------------

#[tokio::test]
async fn budget_push_str_rejects_over_the_byte_limit() {
    let mut limits = permissive_limits();
    limits.max_output_bytes = 5;
    limits.max_output_scalars = 1_000_000; // generous, isolates the byte guard
    let mut budget = Budget::new(limits);

    // "e" + combining acute accent renders as one visual character, but
    // Rust and Unicode both count it as a scalar per `char`; pick 2-byte
    // scalars ("é" as a single precomposed codepoint, U+00E9) so scalar
    // count stays low while byte count climbs fast.
    for _ in 0..2 {
        budget
            .push_str("\u{00e9}")
            .expect("pushing 2-byte scalars up to the byte limit must succeed");
    }
    // 2 scalars * 2 bytes = 4 bytes pushed so far, under the 5-byte limit;
    // one more 2-byte scalar takes it to 6, over the limit.
    let result = budget.push_str("\u{00e9}");
    assert!(
        matches!(
            result,
            Err(ExtractError::OutputTooLarge(OutputLimitKind::Bytes))
        ),
        "pushing past a max_output_bytes of 5 must fail as OutputTooLarge(Bytes), got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Budget 4 — wall-clock deadline
// ---------------------------------------------------------------------

#[tokio::test]
async fn budget_check_deadline_reports_exceeded_after_the_configured_wall_clock() {
    let mut limits = permissive_limits();
    limits.deadline = Duration::from_millis(5);
    let budget = Budget::new(limits);

    tokio::time::sleep(Duration::from_millis(30)).await;

    let result = budget.check_deadline();
    assert!(
        matches!(result, Err(ExtractError::DeadlineExceeded)),
        "check_deadline must report DeadlineExceeded once the configured wall clock has passed, \
         got {result:?}"
    );
}

/// Review round 1, item 1. The test above only proves an *already expired*
/// deadline is reported — an extractor that checked its deadline exactly
/// once, before doing any work, would pass it. The claim the module actually
/// makes is cooperative cancellation: a deadline that expires *part-way
/// through* a decode stops that decode part-way.
///
/// Made deterministic with an injected clock rather than raced against a
/// sleep: the clock reports the start instant for the construction call, the
/// pre-loop check, and the first in-loop check, then jumps an hour. So the
/// decode is guaranteed to complete exactly one chunk and then be cancelled
/// by the *between-chunks* check.
#[tokio::test]
async fn plain_text_extractor_stops_mid_decode_when_the_deadline_expires_between_chunks() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let limits = permissive_limits(); // deadline: 60s, so only the clock decides
    let start = std::time::Instant::now();
    let ticks = Arc::new(AtomicUsize::new(0));
    let clock: wayfinder::extract::Clock = {
        let ticks = Arc::clone(&ticks);
        Arc::new(move || {
            // 0: construction. 1: the pre-loop check. 2: the first in-loop
            // check. 3+: expired.
            if ticks.fetch_add(1, Ordering::SeqCst) < 3 {
                start
            } else {
                start + Duration::from_secs(3600)
            }
        })
    };
    let mut budget = Budget::with_clock(limits, clock);

    // Many decode chunks' worth of input, so "stopped part-way" is
    // unambiguous.
    let text = "a".repeat(400_000);
    let input = ExtractInput {
        declared_type: Some("text/plain"),
        resource_name: "long.txt",
        bytes: text.as_bytes(),
    };

    let result = PlainTextExtractor.extract(&input, &mut budget);

    assert!(
        matches!(result, Err(ExtractError::DeadlineExceeded)),
        "a deadline expiring during the decode must cancel it with DeadlineExceeded, \
         got {result:?}"
    );
    let decoded = budget.output_text().len();
    assert!(
        decoded > 0,
        "the decode must have made progress before the deadline expired (otherwise this test \
         is only re-proving the pre-loop check), but nothing was decoded"
    );
    assert!(
        decoded < text.len() / 2,
        "the decode must stop far short of the input once the deadline passes, but {decoded} of \
         {} bytes were decoded — the between-chunks deadline check is not being consulted",
        text.len()
    );
    assert!(
        ticks.load(Ordering::SeqCst) >= 4,
        "the extractor must consult the clock repeatedly during the decode, not once up front"
    );
}

// ---------------------------------------------------------------------
// Zip guard — entry count
// ---------------------------------------------------------------------

#[tokio::test]
async fn zip_budget_rejects_over_the_configured_entry_count() {
    let mut limits = permissive_limits();
    limits.zip_max_entries = 2;
    let mut zip = ZipBudget::new(limits);

    for i in 0..2 {
        let entry = ZipEntryMeta {
            name: &format!("file{i}.txt"),
            compressed_size: 10,
            uncompressed_size: 10,
        };
        zip.admit(&entry)
            .expect("admitting up to the entry-count limit must succeed");
    }
    let third = ZipEntryMeta {
        name: "file2.txt",
        compressed_size: 10,
        uncompressed_size: 10,
    };
    let result = zip.admit(&third);
    assert!(
        matches!(
            result,
            Err(ExtractError::ZipBudget(ZipViolation::TooManyEntries))
        ),
        "a 3rd entry over a zip_max_entries of 2 must fail as ZipBudget(TooManyEntries), \
         got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Zip guard — path validation
// ---------------------------------------------------------------------

#[tokio::test]
async fn zip_budget_rejects_every_kind_of_unsafe_entry_path() {
    let unsafe_names = [
        ("/etc/passwd", "absolute path"),
        ("../../etc/passwd", ".. traversal"),
        ("a/../../b", "embedded .. traversal"),
        ("a\\b", "backslash separator"),
        (r"C:\Windows\System32", "drive letter"),
        ("a\0b", "embedded NUL"),
    ];

    for (name, why) in unsafe_names {
        let mut limits = permissive_limits();
        // Fresh budget per case, so one rejection doesn't consume state
        // (e.g. entry count) another case depends on.
        limits.zip_max_entries = 10;
        let mut zip = ZipBudget::new(limits);
        let entry = ZipEntryMeta {
            name,
            compressed_size: 10,
            uncompressed_size: 10,
        };
        let result = zip.admit(&entry);
        assert!(
            matches!(
                result,
                Err(ExtractError::ZipBudget(ZipViolation::InvalidPath))
            ),
            "entry name {name:?} ({why}) must be rejected as ZipBudget(InvalidPath), \
             got {result:?}"
        );
    }
}

#[tokio::test]
async fn zip_budget_admits_an_ordinary_relative_path() {
    // Companion to the unsafe-path test above: proves the path guard isn't
    // simply rejecting everything (which would make the unsafe-path test
    // vacuous).
    let limits = permissive_limits();
    let mut zip = ZipBudget::new(limits);
    let entry = ZipEntryMeta {
        name: "docs/word/document.xml",
        compressed_size: 10,
        uncompressed_size: 10,
    };
    let result = zip.admit(&entry);
    assert!(
        result.is_ok(),
        "an ordinary relative path must be admitted, got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Zip guard — per-entry uncompressed bytes
// ---------------------------------------------------------------------

#[tokio::test]
async fn zip_budget_rejects_a_single_entry_over_the_per_entry_byte_limit() {
    let mut limits = permissive_limits();
    limits.zip_max_entry_bytes = 1000;
    let mut zip = ZipBudget::new(limits);

    let entry = ZipEntryMeta {
        name: "huge.txt",
        compressed_size: 10,
        uncompressed_size: 1001,
    };
    let result = zip.admit(&entry);
    assert!(
        matches!(
            result,
            Err(ExtractError::ZipBudget(ZipViolation::EntryTooLarge))
        ),
        "a single entry over zip_max_entry_bytes must fail as ZipBudget(EntryTooLarge), \
         got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Zip guard — cumulative uncompressed bytes across entries
// ---------------------------------------------------------------------

#[tokio::test]
async fn zip_budget_rejects_once_cumulative_uncompressed_bytes_exceed_the_limit() {
    let mut limits = permissive_limits();
    limits.zip_max_entry_bytes = 1000; // each entry individually fits
    limits.zip_max_cumulative_bytes = 2500; // but three of them don't
    limits.zip_max_entries = 100;
    let mut zip = ZipBudget::new(limits);

    for i in 0..2 {
        let entry = ZipEntryMeta {
            name: &format!("part{i}.txt"),
            compressed_size: 10,
            uncompressed_size: 1000,
        };
        zip.admit(&entry)
            .expect("first two 1000-byte entries must fit under the cumulative limit of 2500");
    }
    let third = ZipEntryMeta {
        name: "part2.txt",
        compressed_size: 10,
        uncompressed_size: 1000,
    };
    let result = zip.admit(&third);
    assert!(
        matches!(
            result,
            Err(ExtractError::ZipBudget(ZipViolation::CumulativeTooLarge))
        ),
        "a 3rd 1000-byte entry taking the cumulative total to 3000 (over 2500) must fail as \
         ZipBudget(CumulativeTooLarge), got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Zip guard — compression ratio (42.zip-shaped)
// ---------------------------------------------------------------------

#[tokio::test]
async fn zip_budget_rejects_a_42_zip_shaped_compression_ratio() {
    let mut limits = permissive_limits();
    limits.zip_max_compression_ratio = 100.0;
    // Individually-permissive size limits, so only the ratio guard can be
    // the one that trips.
    limits.zip_max_entry_bytes = 10_000_000_000;
    limits.zip_max_cumulative_bytes = 10_000_000_000;
    let mut zip = ZipBudget::new(limits);

    // 42.zip's own shape: a few KB of compressed bytes expanding to
    // gigabytes. 1000:1 here is well over the 100:1 configured limit.
    let entry = ZipEntryMeta {
        name: "bomb.txt",
        compressed_size: 1000,
        uncompressed_size: 1_000_000,
    };
    let result = zip.admit(&entry);
    assert!(
        matches!(
            result,
            Err(ExtractError::ZipBudget(ZipViolation::RatioTooHigh))
        ),
        "an entry with a 1000:1 compression ratio over a configured limit of 100:1 must fail as \
         ZipBudget(RatioTooHigh), got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Structural-limit hooks: one generic BoundedCounter, six use sites
// ---------------------------------------------------------------------

#[test]
fn bounded_counter_rejects_the_increment_past_its_limit_xml_depth() {
    let mut counter = BoundedCounter::new(3);
    for _ in 0..3 {
        counter
            .increment(StructuralLimitKind::XmlDepth)
            .expect("incrementing up to the limit must succeed");
    }
    let result = counter.increment(StructuralLimitKind::XmlDepth);
    assert!(
        matches!(
            result,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::XmlDepth))
        ),
        "the 4th increment over a limit of 3 must fail naming XmlDepth, got {result:?}"
    );
}

#[test]
fn bounded_counter_rejects_the_increment_past_its_limit_xml_events() {
    let mut counter = BoundedCounter::new(3);
    for _ in 0..3 {
        counter.increment(StructuralLimitKind::XmlEvents).unwrap();
    }
    let result = counter.increment(StructuralLimitKind::XmlEvents);
    assert!(
        matches!(
            result,
            Err(ExtractError::StructuralLimit(
                StructuralLimitKind::XmlEvents
            ))
        ),
        "got {result:?}"
    );
}

#[test]
fn bounded_counter_rejects_the_increment_past_its_limit_sheets() {
    let mut counter = BoundedCounter::new(3);
    for _ in 0..3 {
        counter.increment(StructuralLimitKind::Sheets).unwrap();
    }
    let result = counter.increment(StructuralLimitKind::Sheets);
    assert!(
        matches!(
            result,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::Sheets))
        ),
        "got {result:?}"
    );
}

#[test]
fn bounded_counter_rejects_the_increment_past_its_limit_cells() {
    let mut counter = BoundedCounter::new(3);
    for _ in 0..3 {
        counter.increment(StructuralLimitKind::Cells).unwrap();
    }
    let result = counter.increment(StructuralLimitKind::Cells);
    assert!(
        matches!(
            result,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::Cells))
        ),
        "got {result:?}"
    );
}

#[test]
fn bounded_counter_rejects_the_increment_past_its_limit_rtf_group_depth() {
    let mut counter = BoundedCounter::new(3);
    for _ in 0..3 {
        counter
            .increment(StructuralLimitKind::RtfGroupDepth)
            .unwrap();
    }
    let result = counter.increment(StructuralLimitKind::RtfGroupDepth);
    assert!(
        matches!(
            result,
            Err(ExtractError::StructuralLimit(
                StructuralLimitKind::RtfGroupDepth
            ))
        ),
        "got {result:?}"
    );
}

#[test]
fn bounded_counter_rejects_the_increment_past_its_limit_pdf_pages() {
    let mut counter = BoundedCounter::new(3);
    for _ in 0..3 {
        counter.increment(StructuralLimitKind::PdfPages).unwrap();
    }
    let result = counter.increment(StructuralLimitKind::PdfPages);
    assert!(
        matches!(
            result,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::PdfPages))
        ),
        "got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Signature dispatch beats a wrong declared MIME type / extension
// ---------------------------------------------------------------------

#[test]
fn detect_prefers_magic_bytes_over_a_wrong_declared_mime_type_and_extension() {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"more content after the signature");
    let got = detect(Some("text/plain"), "definitely-not-a.txt", &bytes);
    assert_eq!(
        got,
        ContentType::Pdf,
        "a %PDF- signature must win over a declared text/plain MIME type and a .txt extension"
    );
}

#[test]
fn detect_recognizes_zip_container_signature() {
    let bytes = b"PK\x03\x04rest of a zip local file header".to_vec();
    let got = detect(None, "unknown", &bytes);
    assert_eq!(got, ContentType::Zip, "PK\\x03\\x04 must detect as Zip");
}

#[test]
fn detect_recognizes_rtf_signature() {
    let bytes = b"{\\rtf1\\ansi more rtf content".to_vec();
    let got = detect(Some("application/octet-stream"), "doc.bin", &bytes);
    assert_eq!(got, ContentType::Rtf, "{{\\rtf must detect as Rtf");
}

// ---------------------------------------------------------------------
// OLE2 legacy files: typed UnsupportedFormat, never routed to a ZIP path
// ---------------------------------------------------------------------

const OLE2_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

#[test]
fn detect_recognizes_ole2_legacy_binary_signature_as_legacy_ole_not_zip() {
    let mut bytes = OLE2_SIGNATURE.to_vec();
    bytes.extend_from_slice(&[0u8; 32]); // padding, real CFB files carry a header here
    // A wrong declared type/extension pointing at a modern OOXML format
    // must not override the OLE2 signature either.
    let got = detect(
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "legacy.doc",
        &bytes,
    );
    assert_eq!(
        got,
        ContentType::LegacyOle,
        "an OLE2 CFB signature must detect as LegacyOle, never Zip, regardless of a declared \
         OOXML MIME type or a .doc extension"
    );
}

#[test]
fn dispatch_has_no_extractor_for_legacy_ole() {
    let got = dispatch(ContentType::LegacyOle);
    assert!(
        got.is_none(),
        "dispatch(LegacyOle) must return None: phase 0 has no extractor for it, and it must \
         never silently resolve to the Zip extractor"
    );
}

#[test]
fn extract_returns_typed_unsupported_format_for_legacy_ole() {
    let mut bytes = OLE2_SIGNATURE.to_vec();
    bytes.extend_from_slice(&[0u8; 32]);
    let input = ExtractInput {
        declared_type: None,
        resource_name: "legacy.doc",
        bytes: &bytes,
    };
    let mut budget = Budget::new(permissive_limits());
    let result = extract(&input, &mut budget);
    assert!(
        matches!(
            result,
            Err(ExtractError::UnsupportedFormat {
                content_type: ContentType::LegacyOle
            })
        ),
        "extracting an OLE2 legacy binary must fail with a typed \
         UnsupportedFormat{{content_type: LegacyOle}}, never a misparse through a ZIP/OOXML \
         path, got {result:?}"
    );
}

// ---------------------------------------------------------------------
// The plain-text extractor: round trip, deadline, output budget
// ---------------------------------------------------------------------

#[test]
fn plain_text_extractor_round_trips_utf8_text() {
    let text = "hello, wayfinder extraction";
    let input = ExtractInput {
        declared_type: Some("text/plain"),
        resource_name: "note.txt",
        bytes: text.as_bytes(),
    };
    let mut budget = Budget::new(permissive_limits());
    let result = PlainTextExtractor.extract(&input, &mut budget);
    let extracted = result.expect("plain-text extraction of valid UTF-8 must succeed");
    assert_eq!(
        extracted.text, text,
        "plain-text extraction must round-trip the input bytes as-is"
    );
    assert_eq!(extracted.metadata.resource_name, "note.txt");
    assert_eq!(extracted.metadata.content_type, ContentType::PlainText);
}

#[test]
fn plain_text_extractor_respects_the_output_byte_budget() {
    let text = "x".repeat(1000);
    let input = ExtractInput {
        declared_type: Some("text/plain"),
        resource_name: "big.txt",
        bytes: text.as_bytes(),
    };
    let mut limits = permissive_limits();
    limits.max_output_bytes = 10;
    let mut budget = Budget::new(limits);
    let result = PlainTextExtractor.extract(&input, &mut budget);
    assert!(
        matches!(
            result,
            Err(ExtractError::OutputTooLarge(OutputLimitKind::Bytes))
        ),
        "extracting 1000 bytes of text under a max_output_bytes of 10 must fail as \
         OutputTooLarge(Bytes) rather than truncating silently or succeeding, got {result:?}"
    );
}

#[test]
fn plain_text_extractor_respects_the_deadline() {
    let text = "some text".repeat(100);
    let input = ExtractInput {
        declared_type: Some("text/plain"),
        resource_name: "slow.txt",
        bytes: text.as_bytes(),
    };
    let mut limits = permissive_limits();
    limits.deadline = Duration::from_millis(0);
    let mut budget = Budget::new(limits);
    // A zero-length deadline, checked before any chunk is processed, must
    // report exceeded immediately rather than extracting the whole input.
    std::thread::sleep(Duration::from_millis(5));
    let result = PlainTextExtractor.extract(&input, &mut budget);
    assert!(
        matches!(result, Err(ExtractError::DeadlineExceeded)),
        "extraction must consult the deadline and stop once it has passed, got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Error taxonomy: ExtractError -> WfError matches the captured envelope
// ---------------------------------------------------------------------

#[tokio::test]
async fn extract_error_parse_maps_to_the_corrupt_pdf_envelope_shape() {
    use http_body_util::BodyExt;
    use wayfinder::extract::extract_error_response;

    let expected = fixture("extract_corrupt_pdf");
    let want_status = expected["responseHeader"]["status"]
        .as_i64()
        .expect("fixture must have responseHeader.status");
    let want_code = expected["error"]["code"]
        .as_i64()
        .expect("fixture must have error.code");
    assert_eq!(
        want_status, want_code,
        "fixture sanity: responseHeader.status must mirror error.code"
    );
    assert_eq!(
        want_code, 500,
        "fixture sanity: extract_corrupt_pdf.json must be the captured 500"
    );
    assert!(
        expected["responseHeader"].get("params").is_none(),
        "fixture sanity: the extract handler is an /update path and never echoes params"
    );

    let err = ExtractError::Parse("could not parse malformed PDF".to_string());
    let response = extract_error_response(err);

    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("response body must be valid JSON");

    assert_eq!(
        status.as_u16() as i64,
        want_status,
        "a parser-failure ExtractError must render as HTTP {want_status}, matching \
         extract_corrupt_pdf.json"
    );
    assert_eq!(
        body["responseHeader"]["status"].as_i64(),
        Some(want_status),
        "responseHeader.status must match the fixture"
    );
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(want_code),
        "error.code must match the fixture"
    );
    assert!(
        body["responseHeader"].get("params").is_none(),
        "an /update-path error must never echo params, matching the fixture"
    );
    let metadata = body["error"]["metadata"]
        .as_array()
        .expect("error.metadata must be present as an array, matching the fixture's shape");
    assert!(
        !metadata.is_empty(),
        "error.metadata must be non-empty, matching the fixture's shape"
    );
}

// =======================================================================
// #257 follow-up: extraction hardening (security pass 3)
// =======================================================================
//
// New guards pinned below, one block per spec item (A-F). None of the
// tests above are touched except the mechanical `spawn_extraction` call
// sites updated for item C's new `deadline` parameter (search this file's
// diff for `Duration::from_secs(5)` inserted immediately after
// `spawn_extraction(`) — their assertions are unchanged.

// ---------------------------------------------------------------------
// Item A — ZIP budget: the 0/0 declared-metadata bypass
// ---------------------------------------------------------------------

#[test]
fn zip_budget_charges_actual_bytes_against_the_per_entry_limit_despite_zero_declared_metadata() {
    let mut limits = permissive_limits();
    limits.zip_max_entries = 4096;
    limits.zip_max_entry_bytes = 1_000_000; // 1 MB actual per-entry limit
    limits.zip_max_cumulative_bytes = 1_000_000_000_000; // generous: isolates the per-entry guard
    let mut zip = ZipBudget::new(limits);

    // 4096 entries, all declaring compressed_size == 0, uncompressed_size ==
    // 0 -- exactly what a data-descriptor entry (general-purpose bit 3), or
    // a forged central directory, declares. Cheap: metadata structs, never
    // a real archive. Every one passes `admit()`, because the declared-size
    // check and the ratio check are both guarded by `uncompressed_size > 0`
    // and skip entirely for a 0/0 entry -- that is the bypass this guard
    // closes at the actual-bytes layer, not at `admit()`.
    for i in 0..4095 {
        let name = format!("entry{i}.bin");
        let entry = ZipEntryMeta {
            name: &name,
            compressed_size: 0,
            uncompressed_size: 0,
        };
        zip.admit(&entry).unwrap_or_else(|e| {
            panic!("entry {i}: a 0/0-declared entry must pass the declared-metadata pre-filter, got {e:?}")
        });
        // Modest real decompressed output for this entry, comfortably under
        // the 1 MB per-entry actual limit.
        zip.charge_actual(1000).unwrap_or_else(|e| {
            panic!("entry {i}: charging 1000 actual bytes must stay under zip_max_entry_bytes, got {e:?}")
        });
    }
    assert_eq!(
        zip.cumulative_uncompressed(),
        0,
        "declared metadata must contribute nothing for a 0/0-declared entry, even after 4095 of \
         them were admitted"
    );

    // The 4096th entry: still declares 0/0 and is still admitted by the
    // pre-filter, but its real deflate stream expands to 2 MB -- over the 1
    // MB per-entry actual limit. This must be stopped, regardless of what
    // it declared.
    let last_name = "entry4095.bin";
    let last_entry = ZipEntryMeta {
        name: last_name,
        compressed_size: 0,
        uncompressed_size: 0,
    };
    zip.admit(&last_entry)
        .expect("the 4096th 0/0-declared entry must also pass the declared-metadata pre-filter");
    let result = zip.charge_actual(2_000_000);
    assert!(
        matches!(
            result,
            Err(ExtractError::ZipBudget(ZipViolation::EntryTooLarge))
        ),
        "2,000,000 actual decompressed bytes over a zip_max_entry_bytes of 1,000,000 must fail \
         as ZipBudget(EntryTooLarge) even though the entry declared uncompressed_size: 0, \
         got {result:?}"
    );
}

#[test]
fn zip_budget_charges_actual_bytes_against_the_cumulative_limit_despite_zero_declared_metadata() {
    let mut limits = permissive_limits();
    limits.zip_max_entries = 100;
    limits.zip_max_entry_bytes = 10_000_000; // generous: isolates the cumulative guard
    limits.zip_max_cumulative_bytes = 5_000_000;
    let mut zip = ZipBudget::new(limits);

    // Five entries, each declaring 0/0 and each actually expanding to
    // 1,000,000 bytes -- individually well under the 10 MB per-entry limit,
    // but five of them exactly fill the 5,000,000-byte cumulative limit.
    for i in 0..5 {
        let name = format!("part{i}.bin");
        let entry = ZipEntryMeta {
            name: &name,
            compressed_size: 0,
            uncompressed_size: 0,
        };
        zip.admit(&entry).unwrap_or_else(|e| {
            panic!("entry {i}: a 0/0-declared entry must be admitted, got {e:?}")
        });
        zip.charge_actual(1_000_000).unwrap_or_else(|e| {
            panic!(
                "entry {i}: charging up to the cumulative actual limit of 5,000,000 must \
                 succeed, got {e:?}"
            )
        });
    }

    // A 6th entry, also declaring 0/0, whose actual bytes take the running
    // cumulative actual total to 6,000,000 -- over the 5,000,000 limit.
    let sixth = ZipEntryMeta {
        name: "part5.bin",
        compressed_size: 0,
        uncompressed_size: 0,
    };
    zip.admit(&sixth)
        .expect("the 6th 0/0-declared entry must also pass the declared-metadata pre-filter");
    let result = zip.charge_actual(1_000_000);
    assert!(
        matches!(
            result,
            Err(ExtractError::ZipBudget(ZipViolation::CumulativeTooLarge))
        ),
        "actual decompressed bytes taking the cumulative running total to 6,000,000 (over a \
         zip_max_cumulative_bytes of 5,000,000) must fail as ZipBudget(CumulativeTooLarge), \
         even though every entry declared uncompressed_size: 0, got {result:?}"
    );
}

/// Added by the implementor stage: the mutation harness proved the two tests
/// above do not bind the *accumulation* inside one entry. Both charge a
/// single time per entry, so `entry_actual = bytes` (assign instead of
/// add-assign) survived them — and that mutant is precisely the real attack:
/// a walker reading a zero-declared entry in chunks would have each chunk
/// checked in isolation and the entry as a whole never bounded at all.
#[test]
fn zip_budget_accumulates_actual_bytes_across_the_chunks_of_a_single_entry() {
    let mut limits = permissive_limits();
    limits.zip_max_entry_bytes = 10_000;
    limits.zip_max_cumulative_bytes = 1_000_000_000; // isolates the per-entry guard
    let mut zip = ZipBudget::new(limits);

    let entry = ZipEntryMeta {
        name: "streamed.bin",
        compressed_size: 0,
        uncompressed_size: 0,
    };
    zip.admit(&entry).expect("0/0 entry passes the pre-filter");

    // Ten 1000-byte chunks exactly fill the 10,000-byte per-entry limit. No
    // single chunk is anywhere near it.
    for chunk in 0..10 {
        zip.charge_actual(1000).unwrap_or_else(|e| {
            panic!("chunk {chunk}: charging up to the per-entry limit must succeed, got {e:?}")
        });
    }
    let result = zip.charge_actual(1);
    assert!(
        matches!(
            result,
            Err(ExtractError::ZipBudget(ZipViolation::EntryTooLarge))
        ),
        "one more byte after 10,000 already charged for this entry must fail as EntryTooLarge: \
         actual bytes must accumulate across an entry's chunks, not be checked one chunk at a \
         time, got {result:?}"
    );
    assert_eq!(
        zip.cumulative_actual(),
        10_000,
        "the rejected charge must not have been added to the running total"
    );
}

// ---------------------------------------------------------------------
// Item B — ZIP entry count vs a skip-and-continue walker
// ---------------------------------------------------------------------

#[test]
fn zip_budget_entry_count_terminates_a_skip_and_continue_walker_even_when_every_entry_is_rejected()
{
    let mut limits = permissive_limits();
    limits.zip_max_entries = 5;
    let mut zip = ZipBudget::new(limits);

    // An archive of nothing but `..\evil`-shaped entries: every single
    // attempt fails path validation. A walker that skips a rejected entry
    // and keeps going must still be stopped by the entry-count guard -- if
    // `entries_seen` only ever advances on a successful admission (as it did
    // before this fix), this loop has no bound at all.
    let mut results = Vec::new();
    for _ in 0..=limits.zip_max_entries {
        let entry = ZipEntryMeta {
            name: "..\\evil",
            compressed_size: 10,
            uncompressed_size: 10,
        };
        results.push(zip.admit(&entry));
    }

    for (i, result) in results.iter().take(limits.zip_max_entries).enumerate() {
        assert!(
            matches!(
                result,
                Err(ExtractError::ZipBudget(ZipViolation::InvalidPath))
            ),
            "attempt {i}, within zip_max_entries, must still fail on its own merits as \
             InvalidPath, got {result:?}"
        );
    }
    let last = results.last().expect("at least one attempt was made");
    assert!(
        matches!(
            last,
            Err(ExtractError::ZipBudget(ZipViolation::TooManyEntries))
        ),
        "the attempt one past zip_max_entries must terminate the walk as TooManyEntries instead \
         of looping forever on a walker that skips every rejected entry and keeps going -- \
         `entries_seen` (or an equivalent attempted-entry count) must advance even for a \
         rejected entry, got {last:?}"
    );
}

// ---------------------------------------------------------------------
// Item C — a wedged parser must not pin its caller forever
// ---------------------------------------------------------------------

#[tokio::test]
async fn spawn_extraction_times_out_a_job_that_never_returns_while_the_slot_stays_occupied() {
    let mut limits = permissive_limits();
    limits.max_concurrency = 1;
    let runtime = wayfinder::extract::ExtractionRuntime::new(&limits);

    // A deterministic hang, not a sleep or a busy loop: `thread::park()` in
    // a loop blocks the worker thread forever (a spurious wakeup just parks
    // again), matching the "opaque parser that never returns" case the spec
    // names (PDF is the real-world instance). Unlike a channel-based hang,
    // this has no dependency on any value's drop order back in this test
    // function, which matters once the test itself is expected to panic
    // below: nothing here can be inadvertently dropped/closed by that
    // unwind and let the job return early.
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        runtime.spawn_extraction(Duration::from_millis(50), move || -> () {
            loop {
                std::thread::park();
            }
        }),
    )
    .await
    .expect(
        "spawn_extraction itself must resolve with a timeout error once its baked-in deadline \
         (plus grace) elapses -- it must not pend forever waiting on a wedged parser that never \
         sends and never drops its OneshotTx",
    );

    assert!(
        matches!(result, Err(ExtractError::DeadlineExceeded)),
        "a job that never returns must yield a timeout error to the caller, got {result:?}"
    );

    // Second half of the claim, and the whole point of the split: the pool
    // slot the wedged job is holding must NOT have been freed by the
    // caller's timeout. The permit is released only on the worker thread
    // when/if the job actually returns; the future timing out changes
    // nothing about the still-blocked worker. With max_concurrency == 1,
    // a fresh extraction attempted right after must find the pool full.
    let second = runtime
        .spawn_extraction(Duration::from_secs(5), || "ok")
        .await;
    assert!(
        matches!(second, Err(ExtractError::TooBusy)),
        "the pool slot occupied by the wedged job must remain unavailable after the caller's \
         timeout fired -- only the future timed out, not the worker thread -- got {second:?}"
    );
}

// ---------------------------------------------------------------------
// Item E — the budget must be unforgeable through its public API
// ---------------------------------------------------------------------
//
// A true "cannot be reassigned" proof is a compile-time property (a shared
// reference cannot assign to a field), which this crate has no compile-fail
// test harness (e.g. trybuild) to assert directly, and the task spec rules
// out adding one ("no new dependencies"). What is asserted here instead,
// as the best runtime-observable proxy: the *only* surface `Budget` now
// exposes for driving a structural counter is these delegating methods, and
// that surface enforces the exact same limits `BoundedCounter` always did,
// with no reset or bypass reachable through it -- including under
// repeated hostile hammering on an already-tripped counter.

#[test]
fn budget_xml_depth_delegating_methods_enforce_the_limit_and_cannot_be_worn_down_past_it() {
    let limits = ExtractLimits {
        max_xml_depth: 3,
        ..permissive_limits()
    };
    let budget = Budget::new(limits);

    for depth in 0..3 {
        budget.enter_xml_element().unwrap_or_else(|e| {
            panic!("depth {depth}: entering up to the limit must succeed, got {e:?}")
        });
    }
    let over = budget.enter_xml_element();
    assert!(
        matches!(
            over,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::XmlDepth))
        ),
        "the 4th enter_xml_element() over a max_xml_depth of 3 must fail naming XmlDepth, \
         got {over:?}"
    );

    // A hostile extractor restricted to this public API cannot wear the
    // limit down, force a silent reset, or otherwise bypass it by simply
    // hammering the same call: every subsequent call must keep failing
    // identically.
    for attempt in 0..1000 {
        let repeat = budget.enter_xml_element();
        assert!(
            matches!(
                repeat,
                Err(ExtractError::StructuralLimit(StructuralLimitKind::XmlDepth))
            ),
            "attempt {attempt}: a hostile extractor hammering enter_xml_element() through the \
             public API alone must never succeed in raising the limit or resetting the count, \
             got {repeat:?}"
        );
    }

    // Leaving is the legitimate way to free capacity, and must still work
    // after the hammering above -- proving the limit is genuinely
    // depth-based (decrementable through the sanctioned method), not a
    // one-shot lockout that leave_xml_element() also can't recover from.
    budget.leave_xml_element();
    let re_enter = budget.enter_xml_element();
    assert!(
        re_enter.is_ok(),
        "leave_xml_element() must free one level of depth, allowing a legitimate re-entry, \
         got {re_enter:?}"
    );
}

#[test]
fn budget_cumulative_counters_only_increase_through_their_delegating_methods() {
    let limits = ExtractLimits {
        max_xml_events: 3,
        max_sheets: 3,
        max_cells: 3,
        max_pdf_pages: 3,
        ..permissive_limits()
    };
    let budget = Budget::new(limits);

    // Each of the four cumulative counters (xml_events, sheets, cells,
    // pdf_pages) shares the same shape: no leave()/decrement counterpart at
    // all is exposed for them, so once tripped, repeated calls through the
    // only available method must keep failing -- there is no way to wear
    // the count back down or reset it via the public API.
    for _ in 0..3 {
        budget
            .count_xml_event()
            .expect("counting up to max_xml_events must succeed");
    }
    for attempt in 0..50 {
        let result = budget.count_xml_event();
        assert!(
            matches!(
                result,
                Err(ExtractError::StructuralLimit(
                    StructuralLimitKind::XmlEvents
                ))
            ),
            "attempt {attempt}: count_xml_event() past the limit must keep failing as \
             XmlEvents on every repeated call, with no reset reachable through the public API, \
             got {result:?}"
        );
    }

    for _ in 0..3 {
        budget
            .count_sheet()
            .expect("counting up to max_sheets must succeed");
    }
    let sheets_over = budget.count_sheet();
    assert!(
        matches!(
            sheets_over,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::Sheets))
        ),
        "got {sheets_over:?}"
    );

    for _ in 0..3 {
        budget
            .count_cell()
            .expect("counting up to max_cells must succeed");
    }
    let cells_over = budget.count_cell();
    assert!(
        matches!(
            cells_over,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::Cells))
        ),
        "got {cells_over:?}"
    );

    for _ in 0..3 {
        budget
            .count_pdf_page()
            .expect("counting up to max_pdf_pages must succeed");
    }
    let pages_over = budget.count_pdf_page();
    assert!(
        matches!(
            pages_over,
            Err(ExtractError::StructuralLimit(StructuralLimitKind::PdfPages))
        ),
        "got {pages_over:?}"
    );
}

#[test]
fn budget_rtf_group_depth_delegating_methods_round_trip_like_xml_depth() {
    let limits = ExtractLimits {
        max_rtf_group_depth: 2,
        ..permissive_limits()
    };
    let budget = Budget::new(limits);

    budget
        .enter_rtf_group()
        .expect("entering up to the limit must succeed");
    budget
        .enter_rtf_group()
        .expect("entering up to the limit must succeed");
    let over = budget.enter_rtf_group();
    assert!(
        matches!(
            over,
            Err(ExtractError::StructuralLimit(
                StructuralLimitKind::RtfGroupDepth
            ))
        ),
        "the 3rd enter_rtf_group() over a max_rtf_group_depth of 2 must fail naming \
         RtfGroupDepth, got {over:?}"
    );

    budget.leave_rtf_group();
    let re_enter = budget.enter_rtf_group();
    assert!(
        re_enter.is_ok(),
        "leave_rtf_group() must free one level of nesting, allowing a legitimate re-entry, \
         got {re_enter:?}"
    );
}

// ---------------------------------------------------------------------
// Item F — XHTML dispatch: signature-first, but Html beats bare Xml
// ---------------------------------------------------------------------

#[test]
fn detect_prefers_html_for_xhtml_with_an_xml_declaration_declared_as_text_html() {
    // The headline bug case named in the spec: an XHTML document served as
    // text/html, opening with an XML declaration. The `<?xml` signature
    // must not outrank the fact that this is really HTML the phase-1 HTML
    // extractor needs to see.
    let bytes = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                  <html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>t</title></head>\
                  <body><p>hi</p></body></html>";
    let got = detect(Some("text/html"), "page.xhtml", bytes);
    assert_eq!(
        got,
        ContentType::Html,
        "XHTML declared text/html, with an XML declaration and an <html> root, must detect as \
         Html, not Xml -- otherwise it is silently unavailable to the HTML extractor"
    );
}

#[test]
fn detect_prefers_html_for_xhtml_with_an_xml_declaration_and_no_declared_type() {
    let bytes = b"<?xml version=\"1.0\"?>\n\
                  <html xmlns=\"http://www.w3.org/1999/xhtml\"><body>hi</body></html>";
    let got = detect(None, "page.xhtml", bytes);
    assert_eq!(
        got,
        ContentType::Html,
        "XHTML with an XML declaration and an <html> root must detect as Html even with no \
         declared type at all, got {got:?}"
    );
}

#[test]
fn detect_prefers_html_for_xhtml_without_an_xml_declaration() {
    // No `<?xml` prologue at all -- valid XHTML, since the declaration is
    // optional. A misleading declared type (text/plain) and an unhelpful
    // extension prove this is genuinely content-sniffed as Html, not merely
    // falling through to a coincidentally-correct declared type.
    let bytes = b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><head></head>\
                  <body><p>hi</p></body></html>";
    let got = detect(Some("text/plain"), "mystery", bytes);
    assert_eq!(
        got,
        ContentType::Html,
        "XHTML with no XML declaration, opening directly with an <html> root, must still \
         detect as Html despite a misleading declared type and extension, got {got:?}"
    );
}

#[test]
fn detect_still_recognizes_a_plain_non_html_xml_document_as_xml() {
    // The non-regression check: the Html-preference fix must not swallow
    // ordinary XML that has no <html> root at all.
    let bytes = b"<?xml version=\"1.0\"?>\n<catalog><item id=\"1\"/></catalog>";
    let got = detect(None, "catalog.xml", bytes);
    assert_eq!(
        got,
        ContentType::Xml,
        "a plain non-HTML XML document must still detect as Xml, got {got:?}"
    );
}

/// Added by the implementor stage: the mutation harness proved the
/// non-regression test above does not bind the *tag delimiter* check, because
/// its sample XML contains no `<html`-prefixed name at all. Dropping the
/// delimiter check survived it, and that mutant steals any XML vocabulary
/// with an `html`-prefixed element from the XML extractor.
#[test]
fn detect_does_not_mistake_an_html_prefixed_xml_element_for_an_html_root() {
    let bytes = b"<?xml version=\"1.0\"?>\n\
                  <htmlContent><htmlFragment>not html</htmlFragment></htmlContent>";
    let got = detect(None, "feed.xml", bytes);
    assert_eq!(
        got,
        ContentType::Xml,
        "an XML vocabulary whose element names merely start with `html` must still detect as \
         Xml -- the root check must require a tag delimiter after `<html`, got {got:?}"
    );
}

/// Added by the implementor stage, for the same reason: nothing in the suite
/// bound the *anchoring* of the no-declaration branch. Searching the whole
/// leading window there (rather than only the start of the document) survived
/// every existing test, and that mutant lets any text file that merely
/// mentions `<html>` override its declared `text/plain` and get routed to an
/// HTML parser.
#[test]
fn detect_does_not_sniff_html_from_a_mention_of_html_inside_plain_text() {
    let mut bytes =
        b"Notes on markup.\n\nA document's root element is written <html> in HTML.\n".to_vec();
    bytes.extend_from_slice(&b"filler ".repeat(50));
    let got = detect(Some("text/plain"), "notes.txt", &bytes);
    assert_eq!(
        got,
        ContentType::PlainText,
        "a plain-text file that merely mentions `<html>` must stay PlainText: with no XML \
         declaration, only a document that *opens* with an html root may be sniffed as Html, \
         got {got:?}"
    );
}
