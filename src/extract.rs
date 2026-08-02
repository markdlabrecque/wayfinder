//! Phase 0 (issue #257): the resource-control substrate every later
//! extraction parser runs inside. **No HTTP route. No format parsers beyond
//! plain text.** Later issues (#171 follow-ups) add `/update/extract`, HTML,
//! OOXML, PDF; they land inside the guards defined here.
//!
//! Three deliverables live here:
//!
//! 1. the extractor contract (`ExtractInput`/`Extracted`/`Extractor`, plus
//!    signature-first `detect`/`dispatch`);
//! 2. the budgets (`ExtractLimits`/`Budget`/`ExtractionRuntime`/`ZipBudget`
//!    and the streaming body guard);
//! 3. the error taxonomy and its mapping onto the captured Solr envelope.
//!
//! Every guard here is *incremental*: it is checked as the thing it bounds
//! grows, never by building the whole thing and measuring it afterwards. That
//! is the entire point of doing this before any parser lands.
//!
//! ## Cancellation, and what phase 0 cannot do
//!
//! Cancellation is **cooperative**. `Budget::check_deadline()` reports that
//! the wall clock has passed; the plain-text extractor consults it between
//! chunks.
//!
//! What a timeout can and cannot do here is worth stating precisely, because
//! the two halves are easy to conflate:
//!
//! - **`tokio::time::timeout(tokio::task::spawn_blocking(...))` is still the
//!   wrong shape**, for one reason only: dropping a join handle does not stop
//!   the thread behind it, so that shape frees the *caller* while the runaway
//!   parser keeps burning a thread from tokio's **shared** blocking pool —
//!   the pool `/select`, `/update`, and every other filesystem-touching path
//!   depend on (see `docs/reports/2026-08-01-text-extraction-exploration.md`).
//! - **Wrapping `spawn_extraction` in a timeout is correct and safe**, and
//!   since the #257 follow-up it is not optional: the timeout is baked into
//!   `spawn_extraction` itself so a caller cannot forget it. The permit is
//!   released by the worker thread, never by the future, and dropping the
//!   internal `OneshotRx` early is harmless (the sender writes into shared
//!   state nobody reads). So the caller is freed without pretending the work
//!   stopped.
//!
//! ponytail: bounded pool + deadline reporting + a baked-in caller-side
//! timeout is the whole in-process cancellation story. An *opaque* parser (a
//! third-party library that never checks a deadline — PDF is the case that
//! forces this) cannot be hard-killed from inside the process. The
//! containment is structural instead: `ExtractionRuntime` owns its own
//! fixed-size worker threads, so the residual risk of a wedged parser is a
//! **burnt pool slot** — that worker thread and its permit are gone until the
//! process exits, and once enough slots burn, extraction sheds every request
//! with `TooBusy` — while the request that provoked it is *not* hung, and the
//! tokio runtime and its shared blocking pool keep running. Upgrade path when
//! PDF lands: either a parser that exposes per-page checkpoints, or move
//! extraction into a separate OS process that can actually be killed.
//! Revisit at that issue, not before.

use std::cell::Cell;
use std::fmt;
use std::future::Future;
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use tempfile::NamedTempFile;

// ---------------------------------------------------------------------
// Deliverable 1 — the extractor contract
// ---------------------------------------------------------------------

/// Wire-visible content types phase 0 can name. A ZIP container's specific
/// OOXML/ODF flavour cannot be told apart from magic bytes alone (only
/// `[Content_Types].xml` / `mimetype` inside the archive can do that), so
/// `Zip` is one variant rather than guessing `Docx`/`Pptx`/... at signature
/// time.
///
/// ponytail: phase 2a splits `Zip` into `Ooxml`/`OpenDocument` variants (or a
/// nested enum) once it can open the archive and read the manifest part.
/// Phase 0 only ever produces `Zip` from `detect()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ContentType {
    PlainText,
    Html,
    Xml,
    Zip,
    Rtf,
    Pdf,
    /// Binary (OLE2/CFB) legacy DOC/PPT/XLS. Never routed to a ZIP/OOXML
    /// path — always `ExtractError::UnsupportedFormat` in phase 0.
    LegacyOle,
    #[default]
    Unknown,
}

impl ContentType {
    /// A stable, wire-safe name used in error messages. Not a MIME type: it
    /// names the *detected* class, which for `Zip` is deliberately vaguer
    /// than any single MIME type would be.
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::PlainText => "plain text",
            ContentType::Html => "html",
            ContentType::Xml => "xml",
            ContentType::Zip => "zip container",
            ContentType::Rtf => "rtf",
            ContentType::Pdf => "pdf",
            ContentType::LegacyOle => "legacy OLE2 binary office document",
            ContentType::Unknown => "unknown",
        }
    }
}

/// The declared content type, resource name, and bytes of one thing to
/// extract. Phase 0 only needs `&[u8]` — no streaming abstraction until a
/// caller (the future multipart route) needs one.
///
/// #257 follow-up item G, stated honestly because the streaming body guard is
/// easy to over-read: `bytes` being a plain slice means the route must have
/// the **entire** document resident (read the temp file back into RAM, or
/// memory-map it) before extraction starts. So `max_body_bytes` bounds the
/// temp file *and*, by consequence, that resident copy — but it is a *ceiling*
/// claim, not a memory-safety claim: peak RSS per in-flight extraction is
/// `max_body_bytes` of input plus up to `max_output_bytes` of accumulated
/// output plus the extractor's own copy of it (see
/// `PlainTextExtractor::extract`), and nothing here bounds how many of those
/// are in flight at once.
///
/// ponytail: the fix is a reader-shaped input (`&mut dyn Read`, or the
/// `ChunkSource` this module already defines) so a parser streams from the
/// temp file instead of a resident slice. Deferred to the route issue, which
/// is where the first caller that actually has a temp file lands; phase 0 has
/// no caller to design it against.
#[derive(Debug, Clone, Copy)]
pub struct ExtractInput<'a> {
    pub declared_type: Option<&'a str>,
    pub resource_name: &'a str,
    pub bytes: &'a [u8],
}

/// The narrow normalized metadata set. Unknown-format metadata is dropped
/// (PRD says so explicitly) rather than passed through as a grab-bag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtractMetadata {
    pub resource_name: String,
    pub content_type: ContentType,
    pub title: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub text: String,
    pub metadata: ExtractMetadata,
}

/// Object-safe extraction trait. `dispatch()` hands back `&'static dyn
/// Extractor` for a given `ContentType`.
pub trait Extractor: Send + Sync {
    fn extract(
        &self,
        input: &ExtractInput<'_>,
        budget: &mut Budget,
    ) -> Result<Extracted, ExtractError>;
}

const OLE2_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Signature-first dispatch: magic bytes win over a wrong declared MIME type
/// or file extension.
///
/// The precedence is deliberate and ordered: a hostile or merely careless
/// uploader controls both the declared MIME type and the filename, but not
/// the file's own leading bytes. So signatures decide first; the declared
/// type is the next-best evidence; the extension is the weakest and only
/// speaks when nothing else did.
pub fn detect(
    declared_type: Option<&str>,
    resource_name: &str,
    leading_bytes: &[u8],
) -> ContentType {
    if let Some(by_signature) = detect_by_signature(leading_bytes) {
        return by_signature;
    }
    if let Some(by_mime) = declared_type.and_then(detect_by_mime) {
        return by_mime;
    }
    detect_by_extension(resource_name).unwrap_or(ContentType::Unknown)
}

/// Magic-byte signatures. `%PDF-`, `PK\x03\x04` (zip container), `{\rtf`, and
/// the OLE2 CFB header are the ones the spec requires; `<?xml` is a cheap
/// extra that keeps a declared `text/plain` from mislabelling real XML.
fn detect_by_signature(bytes: &[u8]) -> Option<ContentType> {
    // A UTF-8 BOM in front of a textual signature is common enough that
    // ignoring it would send BOM-prefixed XML down the plain-text path.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if bytes.starts_with(b"%PDF-") {
        return Some(ContentType::Pdf);
    }
    if bytes.starts_with(&OLE2_SIGNATURE) {
        // Checked before the ZIP signature on purpose: an OLE2 file must
        // never be routed to a ZIP/OOXML path just because a declared type
        // or a `.docx`-ish name said so.
        return Some(ContentType::LegacyOle);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return Some(ContentType::Zip);
    }
    if bytes.starts_with(b"{\\rtf") {
        return Some(ContentType::Rtf);
    }
    sniff_markup(bytes)
}

/// How many leading bytes the markup sniff looks at. An XHTML document's
/// `<html>` root follows the XML declaration and any doctype/comments; a
/// kilobyte covers both comfortably, and bounds the work (and the
/// lowercased copy) regardless of input size.
const MARKUP_SNIFF_WINDOW: usize = 1024;

/// Tells XHTML apart from ordinary XML, and recognises a bare HTML root.
///
/// #257 follow-up item F. `<?xml` used to return `Xml` unconditionally, which
/// outranks the declared MIME type — so XHTML served as `text/html` detected
/// as `Xml` and phase 1's HTML extractor would silently never see it.
///
/// The two branches deliberately differ in how much they trust:
///
/// - After an XML declaration, the document is *known* to be markup, so the
///   `<html>` root (or an XHTML doctype) is looked for anywhere in the
///   leading window — it legitimately sits behind a doctype, comments, or a
///   processing instruction.
/// - With no declaration, only a document that *opens* with `<html` or
///   `<!DOCTYPE html` counts. An unanchored search there would let any plain
///   text file that merely mentions `<html` in its first kilobyte be sniffed
///   as HTML, overriding a declared `text/plain`.
fn sniff_markup(bytes: &[u8]) -> Option<ContentType> {
    let window = &bytes[..bytes.len().min(MARKUP_SNIFF_WINDOW)];
    if bytes.starts_with(b"<?xml") {
        return Some(if window_contains_html_root(window) {
            ContentType::Html
        } else {
            ContentType::Xml
        });
    }
    let head = trim_ascii_whitespace_start(window);
    if is_html_root_at_start(head) {
        return Some(ContentType::Html);
    }
    None
}

fn trim_ascii_whitespace_start(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    &bytes[start..]
}

/// True when `bytes` *begins* with an `<html` element tag or an
/// `<!DOCTYPE html` declaration, case-insensitively. `<html` must be followed
/// by a tag delimiter so `<htmlContent>` in some unrelated vocabulary is not
/// mistaken for an HTML root.
fn is_html_root_at_start(bytes: &[u8]) -> bool {
    let lower = bytes[..bytes.len().min(32)].to_ascii_lowercase();
    if lower.starts_with(b"<html") {
        return match lower.get(5) {
            // Truncated exactly at the window edge: nothing further to
            // disambiguate with, and `<html` alone is far more likely a root
            // element than a prefix.
            None => true,
            Some(&b) => b == b'>' || b == b'/' || b.is_ascii_whitespace(),
        };
    }
    if let Some(rest) = lower.strip_prefix(b"<!doctype") {
        return trim_ascii_whitespace_start(rest).starts_with(b"html");
    }
    false
}

/// True when an `<html` root or an `html` doctype appears anywhere in the
/// already-known-to-be-XML leading window.
fn window_contains_html_root(window: &[u8]) -> bool {
    window
        .iter()
        .enumerate()
        .filter(|&(_, &b)| b == b'<')
        .any(|(i, _)| is_html_root_at_start(&window[i..]))
}

/// Maps a declared MIME type, with any parameters (`; charset=utf-8`)
/// stripped, onto a content type.
fn detect_by_mime(declared: &str) -> Option<ContentType> {
    let essence = declared
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let ct = match essence.as_str() {
        "text/plain" | "text/markdown" | "text/csv" => ContentType::PlainText,
        "text/html" | "application/xhtml+xml" => ContentType::Html,
        "text/xml" | "application/xml" => ContentType::Xml,
        "application/pdf" => ContentType::Pdf,
        "application/rtf" | "text/rtf" => ContentType::Rtf,
        "application/zip"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.oasis.opendocument.text"
        | "application/vnd.oasis.opendocument.presentation"
        | "application/vnd.oasis.opendocument.spreadsheet" => ContentType::Zip,
        "application/msword" | "application/vnd.ms-powerpoint" | "application/vnd.ms-excel" => {
            ContentType::LegacyOle
        }
        _ => return None,
    };
    Some(ct)
}

/// The weakest signal: the filename extension.
fn detect_by_extension(resource_name: &str) -> Option<ContentType> {
    let ext = resource_name.rsplit_once('.')?.1.to_ascii_lowercase();
    let ct = match ext.as_str() {
        "txt" | "text" | "log" | "md" | "csv" => ContentType::PlainText,
        "html" | "htm" | "xhtml" => ContentType::Html,
        "xml" => ContentType::Xml,
        "pdf" => ContentType::Pdf,
        "rtf" => ContentType::Rtf,
        "zip" | "docx" | "pptx" | "xlsx" | "odt" | "odp" | "ods" => ContentType::Zip,
        "doc" | "ppt" | "xls" => ContentType::LegacyOle,
        _ => return None,
    };
    Some(ct)
}

static PLAIN_TEXT_EXTRACTOR: PlainTextExtractor = PlainTextExtractor;

/// Looks up the extractor for a content type. Phase 0 only implements
/// `PlainText`; every other content type (including `LegacyOle`) has no
/// extractor, and the caller must turn that into a typed
/// `UnsupportedFormat` rather than silently routing to some other parser.
pub fn dispatch(content_type: ContentType) -> Option<&'static dyn Extractor> {
    match content_type {
        ContentType::PlainText => Some(&PLAIN_TEXT_EXTRACTOR),
        // Exhaustive on purpose rather than a `_ => None` arm: when phase 1
        // adds the HTML extractor, this match is the place the compiler
        // points at.
        ContentType::Html
        | ContentType::Xml
        | ContentType::Zip
        | ContentType::Rtf
        | ContentType::Pdf
        | ContentType::LegacyOle
        | ContentType::Unknown => None,
    }
}

/// How much text is decoded between two budget/deadline checks. Small enough
/// that a runaway input is stopped promptly, large enough that the checks are
/// not the dominant cost.
const DECODE_CHUNK_BYTES: usize = 8 * 1024;

/// The one real phase-0 extractor: cooperative, chunked UTF-8 decoding.
/// Checks the deadline and output budget between chunks — never
/// `String::from_utf8(whole_thing)`.
///
/// Invalid UTF-8 uses `String::from_utf8_lossy` semantics: each maximal
/// invalid subsequence becomes one U+FFFD REPLACEMENT CHARACTER, and those
/// replacement characters count against the output budget like any other
/// text. Extraction does not fail on undecodable bytes — a document with a
/// mojibake tail still yields the text before it, which matches what Tika
/// does through Solr's extract handler.
///
/// ponytail: charset *detection* (`chardetng`/`encoding_rs`) is phase 1; this
/// extractor assumes UTF-8 (or ASCII, a UTF-8 subset), never sniffs a BOM or
/// a declared charset, and so will mangle a Latin-1 or Shift-JIS upload into
/// replacement characters. Phase 1 prefers a declared/BOM charset, then
/// detection, and decodes through `encoding_rs`.
pub struct PlainTextExtractor;

impl Extractor for PlainTextExtractor {
    fn extract(
        &self,
        input: &ExtractInput<'_>,
        budget: &mut Budget,
    ) -> Result<Extracted, ExtractError> {
        let start = budget.output_text().len();
        let mut rest = input.bytes;
        // The deadline is checked before the first chunk as well as between
        // chunks: an extraction admitted with an already-spent budget must
        // not get one free chunk of work.
        budget.check_deadline()?;
        while !rest.is_empty() {
            budget.check_deadline()?;
            let take = decode_chunk_len(rest, DECODE_CHUNK_BYTES);
            let (chunk, tail) = rest.split_at(take);
            budget.push_str(&String::from_utf8_lossy(chunk))?;
            rest = tail;
        }
        Ok(Extracted {
            // #257 follow-up item G: this `to_string()` is a second full copy
            // of everything this extractor decoded, so peak resident text is
            // up to 2x `max_output_bytes` (40 MB by default) per in-flight
            // extraction — the budget bounds the *accumulated* text, not the
            // process's peak. It is deliberate for now: `Extracted.text` is
            // an owned `String` because the caller outlives the budget, and
            // the alternatives (handing back a range into the budget, or
            // `std::mem::take`-ing it) both break the "one budget accumulates
            // across several extractors" shape phase 2a needs for a
            // multi-part archive.
            //
            // ponytail: if peak RSS becomes the binding constraint, make
            // `Extracted` borrow the budget's output (`text: &str` plus a
            // lifetime) or return the byte range and let the caller slice.
            // Trigger: the first format whose extractor is not the *only*
            // producer of the text it returns.
            text: budget.output_text()[start..].to_string(),
            metadata: ExtractMetadata {
                resource_name: input.resource_name.to_string(),
                content_type: ContentType::PlainText,
                // Plain text carries no title or author. Phase 1+ formats fill
                // these; phase 0 promises `resourceName` + content type only.
                title: None,
                author: None,
            },
        })
    }
}

/// How many bytes of `rest` to decode next, never splitting a multi-byte
/// UTF-8 sequence across a chunk boundary — otherwise a perfectly valid
/// character straddling the boundary would decode as two replacement
/// characters.
fn decode_chunk_len(rest: &[u8], max: usize) -> usize {
    if rest.len() <= max {
        return rest.len();
    }
    match std::str::from_utf8(&rest[..max]) {
        Ok(_) => max,
        // `error_len() == None` means "valid so far, sequence truncated by
        // the chunk boundary": hand the partial sequence to the next chunk.
        Err(e) if e.error_len().is_none() && e.valid_up_to() > 0 => e.valid_up_to(),
        // A genuinely invalid sequence inside the chunk: take the whole
        // chunk and let the lossy decode replace it.
        Err(_) => max,
    }
}

/// Top-level entry point: `detect` + `dispatch`, with a typed
/// `UnsupportedFormat` when there is no extractor for the detected content
/// type. `LegacyOle` in particular comes out here as `UnsupportedFormat`,
/// never silently routed to a ZIP/OOXML extractor.
pub fn extract(input: &ExtractInput<'_>, budget: &mut Budget) -> Result<Extracted, ExtractError> {
    let content_type = detect(input.declared_type, input.resource_name, input.bytes);
    let extractor =
        dispatch(content_type).ok_or(ExtractError::UnsupportedFormat { content_type })?;
    extractor.extract(input, budget)
}

// ---------------------------------------------------------------------
// Deliverable 2 — budgets
// ---------------------------------------------------------------------

/// All resource limits, defaulted until a route/config section exists to
/// override them (deliberately no `[extraction]` section in
/// `src/config.rs` in phase 0 — see the issue #257 report).
///
/// Every default below is a *containment* number, not a capacity plan: it is
/// picked so that a single hostile upload cannot take the process down, and
/// deliberately loose enough that ordinary documents never see it. Once
/// `/update/extract` exists and these are configurable, operators tighten
/// them; nothing here should be read as "the largest document Wayfinder can
/// usefully handle".
#[derive(Debug, Clone, Copy)]
pub struct ExtractLimits {
    /// 32 MiB. Comfortably above the office documents and PDFs that make up
    /// realistic Search API attachment corpora (a text-heavy 500-page PDF is
    /// single-digit MB). Enforced while streaming, so what this number bounds
    /// is **one** upload's temp file — nothing else.
    ///
    /// #257 follow-up item D corrects what this comment used to claim ("small
    /// enough that `max_concurrency` uploads in flight cannot exhaust a modest
    /// host's disk or page cache"). That was false: nothing ties
    /// `stream_to_tempfile` to `max_concurrency`. It takes `(source, dest,
    /// max_bytes)` and never consults a permit, so total disk and file
    /// descriptors in flight are bounded by *HTTP* concurrency, which is
    /// unbounded — 1000 concurrent 32 MiB POSTs are 32 GB of temp files and
    /// 1000 open descriptors before the extraction pool is ever asked for a
    /// slot. Nor does it bound memory: see `ExtractInput::bytes`.
    ///
    /// Required route-side design when `/update/extract` lands (documented
    /// here rather than built now — phase 0 has no route, and a route-shaped
    /// abstraction guessed at in advance is worse than none):
    ///
    /// 1. Bound in-flight uploads **separately** from extraction — their own
    ///    semaphore, or a global counter of temp-file bytes currently
    ///    allocated, sized against the host's disk.
    /// 2. Acquire the extraction permit **only around the parse**, and
    ///    **never across a body read**.
    ///
    /// Point 2 is the trap, and it is the shape someone fixing point 1 the
    /// obvious way reaches for first: acquire-then-stream holds an extraction
    /// permit for the whole upload, so with the default `max_concurrency` of
    /// 4, *four* slowloris connections dribbling one byte per second take
    /// extraction offline indefinitely for everyone else. The deadline does
    /// not save it — `Budget` is only consulted inside the job, which has not
    /// started. Read the body to the temp file under the upload bound, then
    /// acquire the extraction slot.
    pub max_body_bytes: u64,
    /// Four slots. Sized to the *dedicated* pool, not to the machine:
    /// extraction is CPU-bound blocking work, and the reason to bound it is
    /// that a wedged parser holds its thread until the process exits. Four
    /// keeps a small blast radius while leaving the tokio runtime and its
    /// shared blocking pool untouched. Over the limit rejects with
    /// `TooBusy` rather than queueing, so an overloaded server sheds load
    /// instead of accumulating unbounded latency.
    pub max_concurrency: usize,
    /// 10 million Unicode scalars, roughly a 2-3 million word document —
    /// far past any document a human wrote, and the point at which "this is
    /// a decompression bomb" is a better explanation than "this is a long
    /// book". Bounded separately from bytes because a scalar count is what
    /// downstream indexing and tokenizing costs scale with.
    pub max_output_scalars: usize,
    /// 40 MB. The byte twin of `max_output_scalars` at UTF-8's 4-bytes-per-
    /// scalar worst case, so neither limit can be evaded by choosing an
    /// encoding-unfriendly script. Both are checked, and whichever trips
    /// first names itself in the error.
    pub max_output_bytes: usize,
    /// 30 seconds. Chosen against the client rather than the server: Search
    /// API sends extract requests with its indexing timeout, so a budget far
    /// above that would only ever produce work whose result nobody is still
    /// waiting for. Cooperative — see the module docs on cancellation.
    pub deadline: Duration,
    /// 4096 entries. A real DOCX/XLSX/ODT has tens to low hundreds of parts;
    /// four thousand is generous for a media-heavy deck and still bounds the
    /// per-entry work (path validation, ratio checks) to something trivial.
    pub zip_max_entries: usize,
    /// 128 MiB uncompressed per entry. Above any single document part worth
    /// extracting text from, and below the point where one entry's expansion
    /// alone is a denial of service.
    pub zip_max_entry_bytes: u64,
    /// 512 MiB uncompressed across all entries. The per-entry limit alone is
    /// evadable by many merely-large entries; this is the guard that actually
    /// bounds total expansion, so it is the one a zip bomb hits.
    pub zip_max_cumulative_bytes: u64,
    /// 200:1. Ordinary DEFLATE on XML — the densest thing in an OOXML
    /// package — lands well under 20:1; 42.zip-shaped entries are in the
    /// thousands-to-one range. 200 leaves an order of magnitude of headroom
    /// over legitimate content while still catching the bomb early, from
    /// metadata alone, before a single byte is decompressed.
    pub zip_max_compression_ratio: f64,
    /// 256 levels of XML nesting. Office XML is shallow (tens of levels at
    /// worst); depth beyond this is a billion-laughs-shaped input, and a
    /// recursive walker would otherwise risk stack exhaustion.
    pub max_xml_depth: usize,
    /// 5 million XML events. Bounds total parser work independently of
    /// depth, for the wide-and-shallow document that never trips the depth
    /// guard.
    pub max_xml_events: usize,
    /// 256 sheets. Excel's own practical workbook sizes are far below this;
    /// the limit exists so a generated workbook cannot multiply the cell
    /// budget by an unbounded sheet count.
    pub max_sheets: usize,
    /// 1 million cells. A full 1024-column sheet is ~1000 rows at this
    /// budget; text extraction from anything larger is a data export, not a
    /// document, and belongs in an indexing pipeline instead.
    pub max_cells: usize,
    /// 256 levels of RTF group nesting. RTF groups nest a handful deep in
    /// practice, and deep nesting is the classic malformed-RTF stack
    /// exhaustion vector.
    pub max_rtf_group_depth: usize,
    /// 5000 PDF pages. Above essentially every real document and below the
    /// page counts synthesised PDFs use to make per-page work explode. PDF
    /// is phase 3; this counter exists now so that issue inherits it.
    pub max_pdf_pages: usize,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        ExtractLimits {
            max_body_bytes: 32 * 1024 * 1024,
            max_concurrency: 4,
            max_output_scalars: 10_000_000,
            max_output_bytes: 40_000_000,
            deadline: Duration::from_secs(30),
            zip_max_entries: 4096,
            zip_max_entry_bytes: 128 * 1024 * 1024,
            zip_max_cumulative_bytes: 512 * 1024 * 1024,
            zip_max_compression_ratio: 200.0,
            max_xml_depth: 256,
            max_xml_events: 5_000_000,
            max_sheets: 256,
            max_cells: 1_000_000,
            max_rtf_group_depth: 256,
            max_pdf_pages: 5000,
        }
    }
}

/// One generic bounded counter, used for every structural limit (XML depth,
/// XML events, sheets, cells, RTF group nesting, PDF pages) so there is one
/// type instead of six bespoke ones. Each *use site* still gets its own
/// default (from `ExtractLimits`) and its own error identity in the
/// rendered message (`StructuralLimitKind`).
/// Both fields are private and read-only from outside. Since the #257
/// follow-up (item E) the six counters *inside* a `Budget` are private too
/// and reachable only through `Budget`'s delegating methods, so an extractor
/// cannot reassign one — see `Budget` for exactly how far that goes and
/// where it stops.
#[derive(Debug, Clone, Copy)]
pub struct BoundedCounter {
    count: usize,
    limit: usize,
}

impl BoundedCounter {
    pub fn new(limit: usize) -> Self {
        BoundedCounter { count: 0, limit }
    }

    /// How many increments have been accepted so far.
    pub fn count(&self) -> usize {
        self.count
    }

    /// The ceiling this counter was constructed with.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Increments by one, failing with a `StructuralLimit` error naming
    /// `kind` as soon as the count would exceed `limit`. The count is not
    /// advanced on failure, so a caller that maps the error and continues
    /// cannot silently drift past the limit.
    pub fn increment(&mut self, kind: StructuralLimitKind) -> Result<(), ExtractError> {
        if self.count >= self.limit {
            return Err(ExtractError::StructuralLimit(kind));
        }
        self.count += 1;
        Ok(())
    }

    /// The mirror of `increment` for depth-shaped counters, which go back
    /// down when an element closes. Saturating: an unbalanced document
    /// cannot drive this below zero.
    ///
    /// **Only the three depth-shaped counters may be decremented:**
    /// `Budget::xml_depth`, `Budget::rtf_group_depth`, and any future
    /// nesting counter. The three cumulative counters — `xml_events`,
    /// `cells`, `pdf_pages` — bound *total work* and must never be
    /// decremented: doing so would let a document alternate
    /// increment/decrement forever and defeat the bound entirely. The type
    /// system does not yet enforce this split.
    ///
    /// ponytail: the enforcing version is a separate `DepthCounter` type
    /// with no `increment`-only sibling sharing its API, so a cumulative
    /// counter simply has no `decrement` to call. Deferred to the phase that
    /// first drives XML nesting for real (#171 follow-up, OOXML), because
    /// splitting the type now would churn the six call sites before any of
    /// them exists.
    pub fn decrement(&mut self) {
        self.count = self.count.saturating_sub(1);
    }
}

/// A clamped `now + deadline`.
///
/// #257 follow-up item G: `Instant + Duration` **panics** on overflow, in
/// release as well as debug (the `Add` impl is not wrapping), so a
/// nonsensically large configured deadline would take the process down at
/// budget-construction time rather than merely being useless. Overflow needs
/// a deadline in the hundreds-of-years range, which is a configuration
/// mistake and not something to fail *closed* on: clamping to `now` would
/// silently expire every extraction immediately, which reads as an outage. So
/// it clamps to a year out — still effectively "no deadline" for any real
/// request, and representable on every platform — and only falls back to
/// `now` if even that overflows, which no reachable clock does.
fn deadline_instant(now: Instant, deadline: Duration) -> Instant {
    const CLAMP: Duration = Duration::from_secs(365 * 24 * 60 * 60);
    now.checked_add(deadline)
        .or_else(|| now.checked_add(CLAMP))
        .unwrap_or(now)
}

/// Where `Budget` reads "now" from when checking its deadline.
/// `Budget::new` uses the system clock; `Budget::with_clock` takes one
/// explicitly.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The live per-extraction state threaded to extractors: deadline, output
/// counters, structural counters.
///
/// `limits` is deliberately not a public field. Extractors receive
/// `&mut Budget` — they must be able to push output and advance counters,
/// and must *not* be able to raise the ceilings they are being held to.
///
/// ## What is unforgeable here, and what is not (#257 follow-up item E)
///
/// The six structural counters are private and `Cell`-backed, driven only
/// through the delegating methods below (`enter_xml_element`,
/// `count_xml_event`, ...). Those methods take `&self`, so an extractor
/// holding this budget **cannot** raise a structural limit, reset a
/// structural count, or reach a `decrement` for a counter that bounds
/// cumulative work: `xml_events`, `sheets`, `cells`, and `pdf_pages` have no
/// leave/decrement method at all, while `xml_depth` and `rtf_group_depth`
/// have exactly one each. That split is now enforced by which methods exist,
/// not by a doc comment asking nicely.
///
/// **That is the whole of the claim.** The `Extractor` trait still takes
/// `&mut Budget`, so an in-tree extractor can still reassign the *whole*
/// budget (`*budget = Budget::new(ExtractLimits { .. })`) and get fresh
/// counters and a fresh deadline that way. The structural counters became
/// unforgeable; whole-budget reassignment did not. Do not read this section
/// as "guard integrity no longer rests on review of in-tree extractors" — it
/// rests on it for exactly one, much more conspicuous, wholesale move.
///
/// The trait was left on `&mut Budget` on purpose rather than by omission.
/// The one real call site — `PlainTextExtractor` reading
/// `output_text()[start..]` back after pushing to it — would, under `&Budget`,
/// force the output `String` into a `RefCell` and either a `Ref` dance across
/// the decode loop or an extra copy of the whole output, on the hot path, to
/// close a hole that requires a deliberate wholesale reassignment to exploit.
///
/// ponytail: the upgrade is `Extractor::extract(&self, input, budget:
/// &Budget)` with the output buffer behind interior mutability too (a `Cell`
/// swap, or a `RefCell` borrowed only inside `push_str`), which makes
/// reassignment impossible because the extractor never holds a `&mut`.
/// Trigger: the first extractor that does not need to read its own output
/// back — at that point the `RefCell`/copy cost disappears and there is
/// nothing left to trade off.
pub struct Budget {
    limits: ExtractLimits,
    xml_depth: Cell<BoundedCounter>,
    xml_events: Cell<BoundedCounter>,
    sheets: Cell<BoundedCounter>,
    cells: Cell<BoundedCounter>,
    rtf_group_depth: Cell<BoundedCounter>,
    pdf_pages: Cell<BoundedCounter>,
    /// The wall-clock instant past which `check_deadline` fails. Stored as
    /// an absolute instant computed once, so repeated checks cannot drift.
    deadline_at: Instant,
    clock: Clock,
    output: String,
    output_scalars: usize,
}

impl Budget {
    pub fn new(limits: ExtractLimits) -> Self {
        Budget::with_clock(limits, Arc::new(Instant::now))
    }

    /// `Budget::new` with an explicit source of "now".
    ///
    /// This is the seam that makes cooperative cancellation *testable*
    /// rather than merely asserted. The interesting property is not "an
    /// already-expired deadline fails immediately" — it is "a deadline that
    /// expires part-way through a decode stops the decode part-way", and
    /// with the system clock that can only be probed by racing a sleep
    /// against a chunk loop. A supplied clock makes it deterministic.
    pub fn with_clock(limits: ExtractLimits, clock: Clock) -> Self {
        let now = clock();
        Budget {
            deadline_at: deadline_instant(now, limits.deadline),
            clock,
            output: String::new(),
            output_scalars: 0,
            xml_depth: Cell::new(BoundedCounter::new(limits.max_xml_depth)),
            xml_events: Cell::new(BoundedCounter::new(limits.max_xml_events)),
            sheets: Cell::new(BoundedCounter::new(limits.max_sheets)),
            cells: Cell::new(BoundedCounter::new(limits.max_cells)),
            rtf_group_depth: Cell::new(BoundedCounter::new(limits.max_rtf_group_depth)),
            pdf_pages: Cell::new(BoundedCounter::new(limits.max_pdf_pages)),
            limits,
        }
    }

    /// The limits this budget enforces. Read-only on purpose — see the type
    /// docs.
    pub fn limits(&self) -> &ExtractLimits {
        &self.limits
    }

    /// `Err(DeadlineExceeded)` once the wall-clock deadline has passed. This
    /// is the *only* cancellation signal a phase-0 extractor gets; see the
    /// module docs.
    pub fn check_deadline(&self) -> Result<(), ExtractError> {
        if (self.clock)() >= self.deadline_at {
            return Err(ExtractError::DeadlineExceeded);
        }
        Ok(())
    }

    /// Time left before the deadline; zero once it has passed.
    pub fn remaining(&self) -> Duration {
        self.deadline_at.saturating_duration_since((self.clock)())
    }

    /// Appends `s` to the accumulated output, checked incrementally against
    /// both the Unicode scalar count and byte count budgets — never by
    /// producing a huge `String` and measuring it afterward. Nothing is
    /// appended when either check fails, so the accumulated output never
    /// exceeds either limit even transiently.
    pub fn push_str(&mut self, s: &str) -> Result<(), ExtractError> {
        let added_scalars = s.chars().count();
        let added_bytes = s.len();
        if self.output_scalars + added_scalars > self.limits.max_output_scalars {
            return Err(ExtractError::OutputTooLarge(OutputLimitKind::Scalars));
        }
        if self.output.len() + added_bytes > self.limits.max_output_bytes {
            return Err(ExtractError::OutputTooLarge(OutputLimitKind::Bytes));
        }
        self.output.push_str(s);
        self.output_scalars += added_scalars;
        Ok(())
    }

    /// The accumulated output text.
    pub fn output_text(&self) -> &str {
        &self.output
    }

    /// Scalars accumulated so far (the byte count is `output_text().len()`).
    pub fn output_scalars(&self) -> usize {
        self.output_scalars
    }

    // -- #257 follow-up item E: delegating methods on `&self` -----------
    //
    // These are the *only* way to drive a structural counter. See the type
    // docs for what that does and does not make unforgeable.

    /// Increments a `Cell`-backed counter. `BoundedCounter` is `Copy`, so
    /// this is a get/modify/set on one thread — a `Budget` belongs to exactly
    /// one worker thread at a time, which is why a `Cell` is enough and no
    /// atomic or lock is used. `increment` does not advance the count when it
    /// fails, so writing the counter back on the error path is a no-op and
    /// the count cannot drift.
    fn bump(cell: &Cell<BoundedCounter>, kind: StructuralLimitKind) -> Result<(), ExtractError> {
        let mut counter = cell.get();
        let result = counter.increment(kind);
        cell.set(counter);
        result
    }

    fn unbump(cell: &Cell<BoundedCounter>) {
        let mut counter = cell.get();
        counter.decrement();
        cell.set(counter);
    }

    /// Enters one level of XML element nesting, failing once
    /// `max_xml_depth` would be exceeded.
    pub fn enter_xml_element(&self) -> Result<(), ExtractError> {
        Budget::bump(&self.xml_depth, StructuralLimitKind::XmlDepth)
    }

    /// Leaves one level of XML element nesting. Saturating, like
    /// `BoundedCounter::decrement`.
    pub fn leave_xml_element(&self) {
        Budget::unbump(&self.xml_depth);
    }

    /// Counts one XML parser event, failing once `max_xml_events` would be
    /// exceeded. No decrementing counterpart: this bounds total work, not
    /// nesting.
    pub fn count_xml_event(&self) -> Result<(), ExtractError> {
        Budget::bump(&self.xml_events, StructuralLimitKind::XmlEvents)
    }

    /// Counts one spreadsheet sheet, failing once `max_sheets` would be
    /// exceeded. No decrementing counterpart — cumulative, not nesting.
    pub fn count_sheet(&self) -> Result<(), ExtractError> {
        Budget::bump(&self.sheets, StructuralLimitKind::Sheets)
    }

    /// Counts one spreadsheet cell, failing once `max_cells` would be
    /// exceeded. No decrementing counterpart — cumulative, not nesting.
    pub fn count_cell(&self) -> Result<(), ExtractError> {
        Budget::bump(&self.cells, StructuralLimitKind::Cells)
    }

    /// Enters one level of RTF group nesting, failing once
    /// `max_rtf_group_depth` would be exceeded.
    pub fn enter_rtf_group(&self) -> Result<(), ExtractError> {
        Budget::bump(&self.rtf_group_depth, StructuralLimitKind::RtfGroupDepth)
    }

    /// Leaves one level of RTF group nesting. Saturating.
    pub fn leave_rtf_group(&self) {
        Budget::unbump(&self.rtf_group_depth);
    }

    /// Counts one PDF page, failing once `max_pdf_pages` would be exceeded.
    /// No decrementing counterpart — cumulative, not nesting.
    pub fn count_pdf_page(&self) -> Result<(), ExtractError> {
        Budget::bump(&self.pdf_pages, StructuralLimitKind::PdfPages)
    }
}

// ---------------------------------------------------------------------
// Concurrency: a reject-not-queue permit count over a dedicated pool
// ---------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send + 'static>;

/// An in-flight extraction slot. Releasing is `Drop`, so every early return
/// and every panicking parser gives its slot back.
struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

fn try_acquire(available: &Arc<AtomicUsize>) -> Option<Permit> {
    let mut current = available.load(Ordering::Acquire);
    loop {
        if current == 0 {
            return None;
        }
        match available.compare_exchange_weak(
            current,
            current - 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Some(Permit(Arc::clone(available))),
            Err(actual) => current = actual,
        }
    }
}

/// Rejects (never queues unboundedly) extractions over the configured
/// concurrency limit, and is the isolation boundary keeping stuck parser
/// work off both the async executor and tokio's *shared* blocking pool that
/// the rest of Wayfinder depends on.
///
/// Two decisions worth stating:
///
/// - **Dedicated OS threads, not `tokio::task::spawn_blocking`.** The whole
///   requirement is that a wedged parser must not consume the blocking pool
///   that `/select`, `/update`, and every other filesystem-touching path
///   share. Running extraction on `spawn_blocking` would do exactly that.
///   The pool here is sized to `max_concurrency`, so the permit count and
///   the thread count cannot disagree and no job ever waits in the channel.
/// - **A plain atomic permit count, not `tokio::sync::Semaphore`.** The
///   policy is *reject when full*, so none of a semaphore's async waiting
///   machinery would ever run; a compare-exchange try-acquire is the whole
///   behaviour, in std, with no extra tokio feature to depend on.
///
/// ponytail: shutdown drops the job channel and lets idle workers exit, but
/// deliberately does **not** join them — joining would make process shutdown
/// hang on precisely the wedged parser this type exists to contain. A stuck
/// worker thread therefore outlives the runtime and is reclaimed only at
/// process exit. Revisit together with the out-of-process cancellation
/// decision when PDF lands.
pub struct ExtractionRuntime {
    available: Arc<AtomicUsize>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
}

impl ExtractionRuntime {
    /// Builds the pool, one worker thread per `max_concurrency` slot.
    ///
    /// `max_concurrency = 0` is **not** "extraction disabled" — it is
    /// clamped to one worker. Zero would advertise no permits at all, so
    /// every request would return `TooBusy` forever, which reads as an
    /// outage rather than as configuration. Disabling extraction is the
    /// route's job (don't mount it), not this pool's; there is no route
    /// yet, so there is nothing to disable. If a later issue wants a real
    /// "off" switch, it belongs in the `[extraction]` config section as an
    /// explicit `enabled` flag, not as a magic zero here.
    pub fn new(limits: &ExtractLimits) -> Self {
        let workers = limits.max_concurrency.max(1);
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..workers {
            let rx = Arc::clone(&rx);
            let spawned = thread::Builder::new()
                .name(format!("wayfinder-extract-{i}"))
                .spawn(move || {
                    loop {
                        // The lock is released before the job runs, so a
                        // long extraction never blocks its sibling workers
                        // from picking up their own jobs.
                        let job = {
                            let guard = match rx.lock() {
                                Ok(guard) => guard,
                                // A poisoned receiver means another worker
                                // panicked while holding the lock; there is
                                // no job to run and no state to repair.
                                Err(poisoned) => poisoned.into_inner(),
                            };
                            guard.recv()
                        };
                        match job {
                            Ok(job) => job(),
                            // Sender dropped: the runtime is shutting down.
                            Err(_) => break,
                        }
                    }
                });
            // A thread that cannot be spawned simply lowers the effective
            // pool size; the permit count is what actually bounds admission,
            // and it is set from the threads that did start — exactly `i` of
            // them, never rounded up. Advertising one permit more than there
            // are workers would admit an extraction that then blocks in the
            // channel forever (or, if the sender is gone, fails as `Io`),
            // when the honest answer is `TooBusy`.
            if spawned.is_err() {
                // `i == 0` (the very first spawn failed) is the degenerate
                // case: no workers, so no permits, so every admission is
                // rejected with `TooBusy` and the job channel is dropped
                // rather than left dangling with nothing to drain it.
                let jobs = if i == 0 { None } else { Some(tx) };
                return ExtractionRuntime {
                    available: Arc::new(AtomicUsize::new(i)),
                    jobs: Mutex::new(jobs),
                };
            }
        }
        ExtractionRuntime {
            available: Arc::new(AtomicUsize::new(workers)),
            jobs: Mutex::new(Some(tx)),
        }
    }

    /// Runs `f` on the dedicated blocking pool if a concurrency slot is
    /// free; otherwise `Err(ExtractError::TooBusy)` immediately. The slot is
    /// held for the whole duration of `f` and released on the worker thread,
    /// so it reflects real in-flight work rather than pending futures.
    ///
    /// #257 follow-up item C: `deadline` is the baked-in timeout — the caller
    /// cannot forget it, unlike a `tokio::time::timeout` applied at the call
    /// site by convention. Without it, a parser that never returns never
    /// sends and never drops its `OneshotTx`, and the originating request
    /// pends forever; with an opaque, non-cooperative parser (PDF) that is
    /// the expected case, not the exotic one.
    ///
    /// Pass the same duration the job's `Budget` was built with. The two are
    /// layered on purpose: the `Budget` deadline is the *cooperative* one a
    /// well-behaved parser stops itself with, and this is the *unconditional*
    /// one that frees the caller when it doesn't. Hence the grace margin —
    /// the cooperative stop should win whenever it can, so the caller sees
    /// the job's own `DeadlineExceeded` and the slot comes back.
    ///
    /// On expiry this resolves with `DeadlineExceeded` **without freeing the
    /// pool slot**. That is not an oversight: the permit is released by the
    /// worker thread when the job returns, so a job that never returns leaves
    /// the slot burnt. Burnt slots are the documented residual risk (see the
    /// module docs) — a hung *request* is not.
    pub async fn spawn_extraction<F, T>(&self, deadline: Duration, f: F) -> Result<T, ExtractError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = match try_acquire(&self.available) {
            Some(permit) => permit,
            None => return Err(ExtractError::TooBusy),
        };
        let (tx, rx) = oneshot::<Result<T, ExtractError>>();
        let job: Job = Box::new(move || {
            // A panicking third-party parser must not take a pool worker
            // with it: catching here keeps the pool at full strength and
            // turns the panic into an ordinary 500.
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(f))
                .map_err(|_| ExtractError::Parse("extraction panicked".to_string()));
            drop(permit);
            tx.send(outcome);
        });
        {
            let guard = match self.jobs.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match guard.as_ref() {
                Some(sender) if sender.send(job).is_ok() => {}
                _ => {
                    return Err(ExtractError::Io("extraction pool is shut down".to_string()));
                }
            }
        }
        // Dropping `rx` on the timeout path is harmless: the sender writes
        // its value into shared state nobody reads afterwards, and the `Arc`
        // keeps that state alive, so the worker cannot fault on a send into a
        // gone receiver.
        match tokio::time::timeout(deadline.saturating_add(SPAWN_TIMEOUT_GRACE), rx).await {
            Ok(Some(outcome)) => outcome,
            Ok(None) => Err(ExtractError::Io(
                "extraction worker exited without a result".to_string(),
            )),
            Err(_elapsed) => Err(ExtractError::DeadlineExceeded),
        }
    }
}

/// How long past the job's own deadline `spawn_extraction` waits before
/// giving up on it.
///
/// Small, and deliberately so: it exists only to let a *cooperative* parser
/// notice its `Budget` deadline and return its own `DeadlineExceeded` first,
/// which is the outcome that also returns the pool slot. Long enough that a
/// parser checking its deadline every chunk wins the race; short enough that
/// a wedged one does not add a meaningful wait to a request that is already
/// at its deadline.
const SPAWN_TIMEOUT_GRACE: Duration = Duration::from_millis(250);

/// Drops the job sender so idle workers see a disconnected channel and exit.
///
/// #257 follow-up item G: the poisoned case is recovered here, like every
/// other lock site in this module. `if let Ok(..)` silently skipped it, which
/// meant a panic anywhere under this mutex left the sender alive forever and
/// every idle worker blocked in `recv()` for the life of the process — a
/// shutdown path that fails to shut down, in the one situation where it
/// matters most. There is no invariant to repair: the guarded value is being
/// set to `None` regardless of who panicked.
///
/// A free function rather than inline in `Drop` so it can be unit-tested
/// against a deliberately poisoned mutex, which is not reachable through
/// `ExtractionRuntime`'s public API.
fn drop_job_sender(jobs: &Mutex<Option<mpsc::Sender<Job>>>) {
    let mut guard = match jobs.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = None;
}

impl Drop for ExtractionRuntime {
    fn drop(&mut self) {
        // Deliberately no join — see the type's ponytail note.
        drop_job_sender(&self.jobs);
    }
}

// ---------------------------------------------------------------------
// A minimal std-only oneshot, so a pool thread can hand a result back to an
// async caller without pulling in an extra tokio feature.
// ---------------------------------------------------------------------

struct OneshotState<T> {
    value: Option<T>,
    waker: Option<Waker>,
    closed: bool,
}

struct OneshotTx<T>(Arc<Mutex<OneshotState<T>>>);

/// Resolves to `Some(value)` when the sender sent one, and `None` if the
/// sender was dropped without sending (a worker that vanished).
struct OneshotRx<T>(Arc<Mutex<OneshotState<T>>>);

fn oneshot<T>() -> (OneshotTx<T>, OneshotRx<T>) {
    let state = Arc::new(Mutex::new(OneshotState {
        value: None,
        waker: None,
        closed: false,
    }));
    (OneshotTx(Arc::clone(&state)), OneshotRx(state))
}

impl<T> OneshotTx<T> {
    /// Stores the value. The receiver is woken by `Drop`, which runs
    /// immediately after this in every caller, so there is exactly one wake
    /// path whether or not a value was ever sent.
    fn send(&self, value: T) {
        if let Ok(mut state) = self.0.lock() {
            state.value = Some(value);
        }
    }
}

impl<T> Drop for OneshotTx<T> {
    fn drop(&mut self) {
        let waker = match self.0.lock() {
            Ok(mut state) => {
                state.closed = true;
                state.waker.take()
            }
            Err(_) => None,
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Future for OneshotRx<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(value) = state.value.take() {
            return Poll::Ready(Some(value));
        }
        if state.closed {
            return Poll::Ready(None);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

// ---------------------------------------------------------------------
// Budget 1 — max HTTP body bytes, enforced while streaming to a temp file
// ---------------------------------------------------------------------

/// The shape a future axum multipart field satisfies
/// (`axum::extract::multipart::Field::chunk()`), so the streaming-body
/// budget can be exercised without a real HTTP request.
///
/// ponytail: native `async fn` in a public trait is not object-safe, so this
/// can only be used with `impl Trait` / generics. Phase 0 has exactly one
/// call site and does not need dynamic dispatch here; if the route ever
/// needs to store a boxed source, desugar to a boxed `Send` future.
#[allow(async_fn_in_trait)]
pub trait ChunkSource: Send {
    /// `None` when exhausted.
    async fn next_chunk(&mut self) -> Option<std::io::Result<Bytes>>;
}

/// Streams `source` into `dest`, failing with `BodyTooLarge` **as soon as**
/// the running total exceeds `max_bytes` — never after buffering the whole
/// body. Returns the number of bytes written on success.
///
/// `source` carries no declared length at all, by construction: this
/// function only ever trusts bytes it has actually counted, so a dishonest
/// or absent `Content-Length` cannot get around the limit. A chunk that
/// would take the running total over the limit is not written, so the temp
/// file never exceeds `max_bytes` — well inside the "limit plus one chunk"
/// ceiling the contract promises.
///
/// The `write_all` below is a *synchronous* write inside an `async fn`, and
/// that is deliberate rather than an oversight. It is bounded work: at most
/// `max_bytes` (32 MiB by default) of buffered writes to a local temp file,
/// spread across chunk-sized calls that each yield to the executor at the
/// next `await`. Neither alternative pays for itself here — `tokio::fs`
/// dispatches every call to the *shared* blocking pool this module exists to
/// stay off, and `spawn_blocking` per chunk would do the same. The write
/// that actually deserves isolation is the parse, which already gets it via
/// `ExtractionRuntime`.
///
/// ponytail: if the route ever accepts bodies large enough that 32 MiB of
/// inline writes measurably stalls a runtime worker, move the whole
/// receive-to-temp-file step onto `ExtractionRuntime` too (its own pool,
/// still not tokio's), rather than reaching for `tokio::fs`.
pub async fn stream_to_tempfile(
    source: &mut impl ChunkSource,
    dest: &mut NamedTempFile,
    max_bytes: u64,
) -> Result<u64, ExtractError> {
    let mut written: u64 = 0;
    while let Some(chunk) = source.next_chunk().await {
        let chunk = chunk.map_err(|e| ExtractError::Io(format!("reading upload body: {e}")))?;
        let len = chunk.len() as u64;
        // Checked *before* the write, and with a saturating add so a
        // pathological chunk length cannot wrap the running total back under
        // the limit.
        if written.saturating_add(len) > max_bytes {
            return Err(ExtractError::BodyTooLarge { limit: max_bytes });
        }
        dest.as_file_mut()
            .write_all(&chunk)
            .map_err(|e| ExtractError::Io(format!("writing upload to temp file: {e}")))?;
        written += len;
    }
    dest.as_file_mut()
        .flush()
        .map_err(|e| ExtractError::Io(format!("flushing upload temp file: {e}")))?;
    Ok(written)
}

// ---------------------------------------------------------------------
// ZIP entry-metadata guards (over metadata only — no zip reader in phase 0)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ZipEntryMeta<'a> {
    pub name: &'a str,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

/// The guards a ZIP-container extractor consults *before* decompressing
/// anything. Everything here works off the central-directory metadata the
/// archive already declares, which is why phase 0 can define and test it
/// with no zip reader at all: phase 2a's `zip` crate feeds it
/// `ZipEntryMeta` and honours the answer.
///
/// Every field is private with read-only accessors, for the same reason
/// `Budget`'s are: the caller this guards against is the archive walker
/// itself, and a walker that could assign `cumulative_uncompressed = 0`
/// between entries would defeat the only check that actually bounds a zip
/// bomb.
///
/// ## The required call sequence (#257 follow-up item A)
///
/// Declared metadata is a **pre-filter, not the guard**. The guard is
/// `charge_actual`, and the two are used in a fixed order per entry:
///
/// ```text
/// for entry in archive {
///     zip.admit(&meta)?;                       // once, per entry
///     let mut reader = entry.take(zip.limits().zip_max_entry_bytes);
///     while let Some(chunk) = reader.read_some()? {
///         zip.charge_actual(chunk.len() as u64)?;   // once per chunk read
///         // ... use chunk ...
///     }
/// }
/// ```
///
/// One `admit()` per entry, then one or more `charge_actual()` calls for that
/// same entry's real decompressed bytes. **The next `admit()` is what marks
/// the entry boundary**: it resets the per-entry actual total, so charges
/// always land against the entry most recently admitted. Charging before any
/// `admit()` charges a zeroth entry whose per-entry total starts at zero;
/// nothing is lost, but the cumulative total still counts it.
///
/// The `.take(zip_max_entry_bytes)` limiter above is not decoration. It is
/// what keeps the decompressor from materialising a gigabyte in RAM *between*
/// two charge calls; `charge_actual` can only stop a read that has already
/// happened.
pub struct ZipBudget {
    limits: ExtractLimits,
    entries_seen: usize,
    /// #257 follow-up item B: every entry the walker *offered*, admitted or
    /// not. Separate from `entries_seen` because the two answer different
    /// questions — see `entries_seen()` and `admit()`.
    entries_attempted: usize,
    cumulative_uncompressed: u64,
    /// Actual decompressed bytes charged for the entry most recently
    /// admitted. Reset by `admit()`.
    entry_actual: u64,
    /// Actual decompressed bytes charged across every entry.
    cumulative_actual: u64,
}

impl ZipBudget {
    pub fn new(limits: ExtractLimits) -> Self {
        ZipBudget {
            limits,
            entries_seen: 0,
            entries_attempted: 0,
            cumulative_uncompressed: 0,
            entry_actual: 0,
            cumulative_actual: 0,
        }
    }

    /// The limits this budget enforces.
    pub fn limits(&self) -> &ExtractLimits {
        &self.limits
    }

    /// How many entries have been **admitted** so far. Rejected entries do
    /// not count.
    ///
    /// This deliberately does *not* answer "how many entries has the walker
    /// been through" — `zip_max_entries` is enforced against that separate,
    /// internal attempted count (#257 follow-up item B). The two are kept
    /// apart because a caller reading this to decide how many parts it
    /// successfully processed would get the wrong answer from an attempt
    /// count, and the entry-count guard would get the wrong answer from this
    /// one.
    pub fn entries_seen(&self) -> usize {
        self.entries_seen
    }

    /// Total declared uncompressed bytes across admitted entries. Rejected
    /// entries contribute nothing, and so do entries that declared zero —
    /// this reports what the archive *claimed*. What it actually produced is
    /// what `charge_actual` bounds.
    pub fn cumulative_uncompressed(&self) -> u64 {
        self.cumulative_uncompressed
    }

    /// Total *actual* decompressed bytes charged so far, across all entries.
    pub fn cumulative_actual(&self) -> u64 {
        self.cumulative_actual
    }

    /// Admits one entry's metadata, or rejects it.
    ///
    /// **This is a pre-filter over what the archive declares, not the guard.**
    /// It stops the cheap, honest-metadata cases (a 42.zip-shaped ratio, an
    /// oversized part) before a byte is decompressed, and it is trivially
    /// bypassed by an entry declaring `compressed_size == 0,
    /// uncompressed_size == 0` — exactly what a data-descriptor entry
    /// (general-purpose bit 3) declares, and what a forged central directory
    /// can declare. Such an entry skips the per-entry size check and the
    /// ratio check (both guarded by `uncompressed_size > 0`) and adds nothing
    /// to the cumulative declared total, while its real deflate stream
    /// expands to whatever it likes. **`charge_actual` is what actually
    /// bounds expansion**; see the type docs for the required call sequence.
    ///
    /// Checks run cheapest-and-most-categorical first (count, then path, then
    /// per-entry size, then ratio) and the cumulative check last, because it
    /// is the only one that mutates the byte totals: a rejected entry must
    /// not have contributed to the running total.
    ///
    /// The entry *count* is the one thing charged before any check can reject
    /// (#257 follow-up item B): `zip_max_entries` counts **attempted**
    /// entries, not admitted ones. Counting only admissions disabled the
    /// guard entirely for the most natural walker shape — an archive of ten
    /// million entries all named `..\evil` returns `InvalidPath` ten million
    /// times, and a walker that skips a bad entry and continues has no bound
    /// at all. Bytes are still only charged for admitted entries, which was
    /// always right.
    pub fn admit(&mut self, entry: &ZipEntryMeta<'_>) -> Result<(), ExtractError> {
        if self.entries_attempted >= self.limits.zip_max_entries {
            return Err(ExtractError::ZipBudget(ZipViolation::TooManyEntries));
        }
        self.entries_attempted += 1;
        // The entry boundary: charges from here on belong to this entry,
        // whether or not it is admitted below (a rejected entry is never
        // decompressed, so it never charges anything).
        self.entry_actual = 0;
        if !is_safe_entry_path(entry.name) {
            return Err(ExtractError::ZipBudget(ZipViolation::InvalidPath));
        }
        if entry.uncompressed_size > self.limits.zip_max_entry_bytes {
            return Err(ExtractError::ZipBudget(ZipViolation::EntryTooLarge));
        }
        // Ratio is judged from declared metadata, before a byte is
        // decompressed — that is what makes it a bomb guard rather than a
        // post-mortem. A zero compressed size with non-zero declared output
        // is an infinite ratio and is treated as the violation it is.
        if entry.uncompressed_size > 0 {
            let over = if entry.compressed_size == 0 {
                true
            } else {
                (entry.uncompressed_size as f64 / entry.compressed_size as f64)
                    > self.limits.zip_max_compression_ratio
            };
            if over {
                return Err(ExtractError::ZipBudget(ZipViolation::RatioTooHigh));
            }
        }
        let cumulative = self
            .cumulative_uncompressed
            .saturating_add(entry.uncompressed_size);
        if cumulative > self.limits.zip_max_cumulative_bytes {
            return Err(ExtractError::ZipBudget(ZipViolation::CumulativeTooLarge));
        }
        self.entries_seen += 1;
        self.cumulative_uncompressed = cumulative;
        Ok(())
    }

    /// #257 follow-up item A: charges `bytes` of *actual* decompressed output
    /// read for the entry most recently admitted by `admit()`, enforcing both
    /// `zip_max_entry_bytes` and `zip_max_cumulative_bytes` against real
    /// bytes rather than declared metadata. Call it once per decompressed
    /// chunk, as the chunk is read — see the type docs for the full sequence.
    ///
    /// This is the guard the declared-metadata path only pre-filters for. It
    /// is what stops 4096 entries that all declared `0/0` and then expanded
    /// to a gigabyte each.
    ///
    /// The actual and declared totals are tracked **separately**, each
    /// bounded by the same limits, rather than summed: summing would
    /// double-charge every honest entry (which declares roughly what it
    /// produces) and reject ordinary archives at half the configured ceiling.
    /// Real expansion is bounded by the actual side alone, which is the side
    /// that corresponds to bytes that exist.
    ///
    /// Nothing is charged when a check fails, so the running totals never
    /// exceed their limits even transiently, and a caller that maps the error
    /// and keeps reading cannot drift past them.
    pub fn charge_actual(&mut self, bytes: u64) -> Result<(), ExtractError> {
        let entry_actual = self.entry_actual.saturating_add(bytes);
        if entry_actual > self.limits.zip_max_entry_bytes {
            return Err(ExtractError::ZipBudget(ZipViolation::EntryTooLarge));
        }
        let cumulative_actual = self.cumulative_actual.saturating_add(bytes);
        if cumulative_actual > self.limits.zip_max_cumulative_bytes {
            return Err(ExtractError::ZipBudget(ZipViolation::CumulativeTooLarge));
        }
        self.entry_actual = entry_actual;
        self.cumulative_actual = cumulative_actual;
        Ok(())
    }
}

/// A ZIP entry name is a relative POSIX-style path and nothing else.
///
/// Rejected: empty names, embedded NUL (truncates the path for any C API
/// downstream), backslashes (a Windows separator that a POSIX host would
/// treat as an ordinary filename character, so the two disagree about where
/// the file lands), absolute paths, Windows drive letters, and any `..`
/// component anywhere in the path — not just a leading one, since
/// `a/../../b` escapes just as effectively.
///
/// ponytail (#257 follow-up item G): **no name check can catch the symlink
/// variant of zip-slip.** An entry with a symlink unix mode in its external
/// attributes, named `link` and containing the bytes `../../etc`, passes
/// every check above — and a later entry named `link/x`, also perfectly safe
/// on its face, then writes through it and out of the extraction directory.
/// Harmless for as long as extraction is memory-only, which is why this is a
/// note rather than a check: `ZipEntryMeta` does not even carry the mode
/// bits, and inventing a guard for a threat model with no code in it would be
/// guessing. Trigger, precisely: *anything that writes archive contents to
/// disk*. At that point the entry's unix mode must be inspected and symlink
/// entries refused outright (or the write must resolve through a directory
/// handle that cannot escape, e.g. `openat` with `O_NOFOLLOW`).
fn is_safe_entry_path(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') || name.contains('\\') {
        return false;
    }
    if name.starts_with('/') {
        return false;
    }
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    !name.split('/').any(|component| component == "..")
}

// ---------------------------------------------------------------------
// Deliverable 3 — error taxonomy
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputLimitKind {
    Scalars,
    Bytes,
}

impl OutputLimitKind {
    fn as_str(self) -> &'static str {
        match self {
            OutputLimitKind::Scalars => "character",
            OutputLimitKind::Bytes => "byte",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZipViolation {
    TooManyEntries,
    InvalidPath,
    EntryTooLarge,
    CumulativeTooLarge,
    RatioTooHigh,
}

impl ZipViolation {
    fn as_str(self) -> &'static str {
        match self {
            ZipViolation::TooManyEntries => "too many entries",
            ZipViolation::InvalidPath => "unsafe entry path",
            ZipViolation::EntryTooLarge => "entry too large",
            ZipViolation::CumulativeTooLarge => "archive expands to too many bytes",
            ZipViolation::RatioTooHigh => "compression ratio too high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralLimitKind {
    XmlDepth,
    XmlEvents,
    Sheets,
    Cells,
    RtfGroupDepth,
    PdfPages,
}

impl StructuralLimitKind {
    /// Each structural limit shares one counter type but keeps its own
    /// identity in the rendered message, so an operator can tell which of
    /// the six actually tripped.
    fn as_str(self) -> &'static str {
        match self {
            StructuralLimitKind::XmlDepth => "XML element depth",
            StructuralLimitKind::XmlEvents => "XML event count",
            StructuralLimitKind::Sheets => "spreadsheet sheet count",
            StructuralLimitKind::Cells => "spreadsheet cell count",
            StructuralLimitKind::RtfGroupDepth => "RTF group nesting depth",
            StructuralLimitKind::PdfPages => "PDF page count",
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    UnsupportedFormat {
        content_type: ContentType,
    },
    TooBusy,
    BodyTooLarge {
        limit: u64,
    },
    OutputTooLarge(OutputLimitKind),
    DeadlineExceeded,
    ZipBudget(ZipViolation),
    StructuralLimit(StructuralLimitKind),
    /// A parser could not make sense of the document. This is the variant
    /// the captured `extract_corrupt_pdf.json` 500 corresponds to.
    Parse(String),
    /// Transport or filesystem failure while handling the upload — neither a
    /// parser failure nor a budget violation, but it renders the same way a
    /// parser failure does (it is a server-side problem, and the client can
    /// only retry).
    Io(String),
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExtractError::UnsupportedFormat { content_type } => {
                write!(f, "unsupported document format: {}", content_type.as_str())
            }
            ExtractError::TooBusy => f.write_str(
                "too many extractions in flight; the extraction concurrency limit was reached",
            ),
            ExtractError::BodyTooLarge { limit } => {
                write!(f, "uploaded document exceeds the {limit} byte limit")
            }
            ExtractError::OutputTooLarge(kind) => write!(
                f,
                "extracted text exceeds the maximum {} count",
                kind.as_str()
            ),
            ExtractError::DeadlineExceeded => f.write_str("extraction exceeded its time budget"),
            ExtractError::ZipBudget(violation) => {
                write!(f, "archive rejected: {}", violation.as_str())
            }
            ExtractError::StructuralLimit(kind) => {
                write!(f, "document exceeds the maximum {}", kind.as_str())
            }
            ExtractError::Parse(msg) => write!(f, "{msg}"),
            ExtractError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Maps an `ExtractError` onto the Solr error envelope.
///
/// The parser-failure arm is the only one with captured ground truth:
/// `solr-ref/responses/extract_corrupt_pdf.json` is HTTP 500 with
/// `responseHeader.status=500`, no `params` echo (the extract handler is an
/// `/update` path, and `/update` never echoes params — hence
/// `Envelope::NoParams`), `error.code=500`, and a `metadata` array naming the
/// error and root-error classes. `msg` text and Java stack content are not
/// contractual (findings 10/59); the code and the envelope shape are.
///
/// **Every other arm has no captured fixture.** Solr's extract handler was
/// never provoked into a budget violation during the #171 capture, so the
/// statuses below are Wayfinder's own defensible choice, not a matched
/// contract. The rule applied: a violation the *client* caused by sending
/// this particular document is a 4xx (it will fail identically on retry); a
/// violation caused by *server* capacity is a 5xx (retry may succeed).
/// This module's `budget_violation_statuses_have_no_captured_fixture_yet`
/// test is the self-expiring note for that — it fails the moment anyone
/// captures an extraction fixture beyond the five that exist today, which is
/// exactly when these mappings must be re-checked against real Solr.
impl From<ExtractError> for crate::error::WfError {
    fn from(err: ExtractError) -> Self {
        use axum::http::StatusCode;

        let msg = err.to_string();
        let (status, class) = match err {
            // Captured: extract_corrupt_pdf.json.
            ExtractError::Parse(_) => (StatusCode::INTERNAL_SERVER_ERROR, "extraction-failed"),
            // Server-side failure, not the document's fault.
            ExtractError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "extraction-io"),
            // 415: the request names a media type this server will not
            // process. The canonical meaning of the status, and it tells the
            // client retrying is pointless.
            ExtractError::UnsupportedFormat { .. } => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "extraction-unsupported-format",
            ),
            // 503: capacity, not content. Retry later may succeed, and 503
            // is the status load balancers and clients already understand as
            // "shed load, back off".
            ExtractError::TooBusy => (StatusCode::SERVICE_UNAVAILABLE, "extraction-too-busy"),
            // 503 rather than 504: 504 asserts an *upstream* gateway timed
            // out, and there is no upstream here — extraction is in-process.
            // The honest statement is that this server could not complete
            // the work in the time it allows itself.
            ExtractError::DeadlineExceeded => {
                (StatusCode::SERVICE_UNAVAILABLE, "extraction-timeout")
            }
            // 413: the canonical "your request body is too big" status, and
            // the one shape here that maps onto an existing HTTP status
            // without argument.
            ExtractError::BodyTooLarge { .. } => {
                (StatusCode::PAYLOAD_TOO_LARGE, "extraction-body-too-large")
            }
            // 400 for the remaining three: the uploaded document itself is
            // out of bounds. Not 413 — the *request* may be small; it is the
            // document's expansion, structure, or archive shape that is
            // unacceptable, and no smaller body of the same document would
            // help. Retrying is pointless, which is what a 4xx says.
            ExtractError::OutputTooLarge(_) => {
                (StatusCode::BAD_REQUEST, "extraction-output-too-large")
            }
            ExtractError::ZipBudget(_) => (StatusCode::BAD_REQUEST, "extraction-archive-rejected"),
            ExtractError::StructuralLimit(_) => {
                (StatusCode::BAD_REQUEST, "extraction-structural-limit")
            }
        };
        crate::error::WfError::new(status, class, msg).envelope(crate::error::Envelope::NoParams)
    }
}

/// Public probe so integration tests (which cannot name the crate-private
/// `crate::error::WfError` type directly) can exercise the
/// `ExtractError` -> `WfError` -> rendered envelope path without a route.
pub fn extract_error_response(err: ExtractError) -> axum::response::Response {
    use axum::response::IntoResponse;
    crate::error::WfError::from(err).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five extraction fixtures captured by the #171 exploration. All
    /// five are success/parser-failure cases; none is a budget violation.
    const CAPTURED_EXTRACT_FIXTURES: [&str; 5] = [
        "extract_corrupt_pdf.json",
        "extract_html_index.json",
        "extract_html_select.json",
        "extract_plain_text_text.json",
        "extract_plain_text_xml.json",
    ];

    /// Self-expiring note for the uncaptured budget-violation statuses in
    /// `impl From<ExtractError> for WfError`.
    ///
    /// Those statuses (413/503/415/400) are Wayfinder's own reasoned choice,
    /// because Solr was never provoked into a budget violation during the
    /// #171 capture. That is only acceptable for as long as there is no
    /// captured evidence to check them against — so this test asserts the
    /// evidence is still missing. The moment the `/update/extract` route
    /// issue captures any further extraction fixture, this fails and names
    /// the mapping that has to be re-verified, instead of the note quietly
    /// rotting into a permanently green lie.
    #[test]
    fn budget_violation_statuses_have_no_captured_fixture_yet() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/responses");
        let mut found: Vec<String> = std::fs::read_dir(&dir)
            .expect("solr-ref/responses must exist")
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().to_string_lossy().into_owned();
                name.starts_with("extract_").then_some(name)
            })
            .collect();
        found.sort();
        assert_eq!(
            found, CAPTURED_EXTRACT_FIXTURES,
            "the set of captured extraction fixtures changed. The budget-violation status \
             mapping in `impl From<ExtractError> for WfError` (413 BodyTooLarge, 503 TooBusy, \
             503 DeadlineExceeded, 415 UnsupportedFormat, 400 OutputTooLarge/ZipBudget/\
             StructuralLimit) was chosen with no captured Solr evidence. If the new fixture \
             covers any of those cases, verify the mapping against it and either fix the \
             mapping or record a deliberate divergence; then update \
             CAPTURED_EXTRACT_FIXTURES."
        );
    }

    #[test]
    fn decode_chunk_len_never_splits_a_multibyte_sequence() {
        // 4 x 'e-acute' = 8 bytes; a max of 5 would land mid-sequence.
        let bytes = "\u{00e9}\u{00e9}\u{00e9}\u{00e9}".as_bytes();
        let take = decode_chunk_len(bytes, 5);
        assert_eq!(take, 4, "must back off to the last complete scalar");
        assert!(std::str::from_utf8(&bytes[..take]).is_ok());
    }

    #[test]
    fn decode_chunk_len_takes_everything_when_under_the_max() {
        assert_eq!(decode_chunk_len(b"abc", 8), 3);
    }

    #[test]
    fn plain_text_decode_is_lossy_not_fatal_on_invalid_utf8() {
        let bytes = b"ok \xff\xfe tail";
        let input = ExtractInput {
            declared_type: Some("text/plain"),
            resource_name: "mixed.txt",
            bytes,
        };
        let mut budget = Budget::new(ExtractLimits::default());
        let extracted = PlainTextExtractor
            .extract(&input, &mut budget)
            .expect("invalid UTF-8 must not fail extraction");
        assert!(extracted.text.starts_with("ok "));
        assert!(extracted.text.ends_with(" tail"));
        assert!(
            extracted.text.contains('\u{FFFD}'),
            "invalid sequences must become replacement characters, got {:?}",
            extracted.text
        );
    }

    #[test]
    fn plain_text_decode_survives_a_scalar_straddling_a_chunk_boundary() {
        // Long enough to span several decode chunks, with multi-byte
        // scalars densely packed so at least one lands on a boundary.
        let text = "\u{00e9}\u{4e2d}a".repeat(DECODE_CHUNK_BYTES);
        let input = ExtractInput {
            declared_type: Some("text/plain"),
            resource_name: "wide.txt",
            bytes: text.as_bytes(),
        };
        let mut budget = Budget::new(ExtractLimits::default());
        let extracted = PlainTextExtractor
            .extract(&input, &mut budget)
            .expect("multi-chunk decode must succeed");
        assert_eq!(extracted.text, text);
    }

    #[test]
    fn detect_falls_back_to_declared_type_then_extension() {
        assert_eq!(
            detect(Some("text/html; charset=utf-8"), "page.txt", b"<p>hi</p>"),
            ContentType::Html,
            "a declared type must beat the extension when no signature matches"
        );
        assert_eq!(
            detect(None, "notes.txt", b"plain words"),
            ContentType::PlainText,
            "the extension is the last resort"
        );
        assert_eq!(
            detect(None, "mystery", b"\x01\x02\x03"),
            ContentType::Unknown
        );
    }

    #[test]
    fn budget_remaining_reaches_zero_after_the_deadline() {
        let limits = ExtractLimits {
            deadline: Duration::from_millis(0),
            ..ExtractLimits::default()
        };
        let budget = Budget::new(limits);
        assert_eq!(budget.remaining(), Duration::ZERO);
    }

    #[test]
    fn bounded_counter_decrement_is_saturating() {
        let mut counter = BoundedCounter::new(4);
        counter.decrement();
        assert_eq!(counter.count(), 0);
        counter.increment(StructuralLimitKind::XmlDepth).unwrap();
        counter.decrement();
        assert_eq!(counter.count(), 0);
    }

    #[test]
    fn zip_ratio_guard_rejects_an_entry_declaring_zero_compressed_bytes() {
        let mut zip = ZipBudget::new(ExtractLimits::default());
        let entry = ZipEntryMeta {
            name: "impossible.txt",
            compressed_size: 0,
            uncompressed_size: 1_000_000,
        };
        assert!(matches!(
            zip.admit(&entry),
            Err(ExtractError::ZipBudget(ZipViolation::RatioTooHigh))
        ));
    }

    #[test]
    fn zip_admit_does_not_charge_a_rejected_entry_to_the_cumulative_total() {
        let limits = ExtractLimits {
            zip_max_entry_bytes: 100,
            ..ExtractLimits::default()
        };
        let mut zip = ZipBudget::new(limits);
        let rejected = ZipEntryMeta {
            name: "big.txt",
            compressed_size: 90,
            uncompressed_size: 500,
        };
        assert!(zip.admit(&rejected).is_err());
        assert_eq!(zip.cumulative_uncompressed(), 0);
        assert_eq!(zip.entries_seen(), 0);
    }

    /// #257 follow-up item G. `Instant + Duration` panics on overflow in
    /// release as well as debug, so a nonsensical configured deadline used to
    /// take the process down inside `Budget::with_clock`. The clamp must keep
    /// construction infallible *and* leave the deadline effectively unreached,
    /// not silently expired.
    #[test]
    fn budget_construction_clamps_an_overflowing_deadline_instead_of_panicking() {
        let limits = ExtractLimits {
            deadline: Duration::MAX,
            ..ExtractLimits::default()
        };
        let budget = Budget::new(limits);
        assert!(
            budget.check_deadline().is_ok(),
            "a clamped absurd deadline must not read as already expired"
        );
        assert!(
            budget.remaining() > Duration::from_secs(30 * 24 * 60 * 60),
            "the clamp must leave a deadline no real request reaches, got {:?}",
            budget.remaining()
        );
    }

    /// #257 follow-up item G. The shutdown path must run on a poisoned mutex
    /// too: skipping it there leaves the sender alive and every idle worker
    /// blocked in `recv()` for the life of the process.
    #[test]
    fn drop_job_sender_clears_the_sender_even_through_a_poisoned_mutex() {
        let (tx, rx) = mpsc::channel::<Job>();
        let jobs = Arc::new(Mutex::new(Some(tx)));

        let poisoner = Arc::clone(&jobs);
        let panicked = thread::spawn(move || {
            let _guard = poisoner.lock().expect("first lock must succeed");
            panic!("poison the jobs mutex");
        })
        .join();
        assert!(panicked.is_err(), "the poisoning thread must have panicked");
        assert!(
            jobs.lock().is_err(),
            "test setup: the mutex must actually be poisoned"
        );

        drop_job_sender(&jobs);

        // `try_recv`, not `recv`: a shutdown that skipped the poisoned mutex
        // leaves the sender alive, and `recv()` would then block this test
        // forever — the exact failure mode under test, reported as a hang
        // instead of a failure. `Disconnected` vs `Empty` distinguishes the
        // two states without waiting.
        assert!(
            matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
            "the sender must have been dropped, so a worker's recv() returns \
             disconnected instead of blocking in recv() for the life of the process"
        );
    }

    #[test]
    fn budget_push_str_leaves_output_untouched_when_it_would_overflow() {
        let limits = ExtractLimits {
            max_output_bytes: 4,
            ..ExtractLimits::default()
        };
        let mut budget = Budget::new(limits);
        budget.push_str("ab").unwrap();
        assert!(budget.push_str("cde").is_err());
        assert_eq!(
            budget.output_text(),
            "ab",
            "a rejected push must not partially append"
        );
        assert_eq!(budget.output_scalars(), 2);
    }
}
