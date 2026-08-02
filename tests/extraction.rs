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
        max_concurrency: 1000,
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
                .spawn_extraction(|| {
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
                .spawn_extraction(|| {
                    std::thread::sleep(Duration::from_millis(50));
                    "b"
                })
                .await
        })
    };
    // Give the two slow extractions a moment to actually acquire their
    // slots before the third is attempted.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let third = runtime.spawn_extraction(|| "c").await;

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
