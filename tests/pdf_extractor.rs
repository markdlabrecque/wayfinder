//! Issue #294 — PDF text extraction (`pdf-extract`).
//!
//! Unit-level: calls `extract::extract_document` directly (no HTTP) and pins
//! the internal `ExtractedDocument` fields against the committed Solr
//! fixtures captured by #261 (`solr-ref/responses/extract_pdf_*.json`), never
//! hand-typed, per CLAUDE.md's compatibility contract. The extraction feature
//! tests separately prove the full rendered envelope matches end to end after
//! the ratified PDF normalisation; this
//! suite pins the extractor's own field-by-field behaviour so a regression
//! names itself immediately, and it is where the cancellation,
//! malformed-input, and encrypted-input checks hang off.
//!
//! ## Why body comparison is whitespace-insensitive
//!
//! `pdf-extract`'s coordinate-based text device and Tika/PDFBox emit the same
//! *words* in the same *order* but different *whitespace* (single vs double
//! newline between columns, none vs `\n\n\n\n` between pages). The #261 GO
//! report records this as "match (whitespace divergence only) ... a
//! normalisation detail for the renderer, not an extraction defect". So this
//! suite compares the sequence of non-whitespace tokens, which still catches
//! every real regression (a dropped word, a mojibake glyph, a reordered
//! column) while tolerating only the rendering whitespace the report waived.

mod common;

use std::sync::Arc;

use wayfinder::extract::{
    Budget, ContentType, ExtractError, ExtractLimits, OutputLimitKind, StructuralLimitKind,
    extract_document,
};

fn extract_inputs_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/extract-inputs")
}

/// The `file` string from a committed fixture (`solr-ref/responses/<name>.json`).
fn fixture_file(name: &str) -> String {
    common::fixture(name)["file"]
        .as_str()
        .unwrap_or_else(|| panic!("fixture {name} has no string `file`"))
        .to_string()
}

/// The fixture's body text with the leading/trailing newline runs stripped
/// and, for documents whose capture Tika prepended a title heading to, the
/// title removed — leaving exactly what a Wayfinder `PdfExtractor` should
/// place in `body_text` (title lives in metadata, prepended by `ExtractRender`).
fn fixture_body(name: &str, title: Option<&str>) -> String {
    let file = fixture_file(name);
    let rest = file.trim_matches('\n');
    match title {
        Some(t) => rest
            .strip_prefix(t)
            .unwrap_or(rest)
            .trim_start_matches('\n')
            .to_string(),
        None => rest.to_string(),
    }
}

/// `split_whitespace` token sequence — the comparison notion for PDF bodies
/// (see the module docs on why exact whitespace is not the contract).
fn words(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

struct Case {
    /// Stem of the corpus file under `solr-ref/extract-inputs/` and the
    /// fixture under `solr-ref/responses/` (`pdf-<stem>.pdf` / `extract_pdf_<stem>.json`).
    stem: &'static str,
    title: Option<&'static str>,
    author: Option<&'static str>,
}

/// The six success-path corpus files: each extracts text whose tokens match
/// the captured Tika body, and metadata comes from the Info dictionary only
/// (Q3: Info wins over XMP; `pdf-extract` has no XMP reader, so they align).
#[test]
fn pdf_corpus_extracts_body_and_metadata() {
    let cases = [
        Case {
            stem: "embedded-font",
            title: None,
            author: None,
        },
        Case {
            stem: "ligatures",
            title: None,
            author: None,
        },
        Case {
            stem: "multicolumn",
            title: None,
            author: None,
        },
        Case {
            stem: "multipage",
            title: None,
            author: None,
        },
        Case {
            stem: "metadata-conflict",
            title: Some("Info Dict Title"),
            author: Some("Info Dict Author"),
        },
        Case {
            stem: "image-only",
            title: None,
            author: None,
        },
    ];

    for c in cases {
        let resource = format!("pdf-{}.pdf", c.stem);
        let bytes = std::fs::read(extract_inputs_dir().join(&resource))
            .unwrap_or_else(|e| panic!("read {resource}: {e}"));
        let budget = Budget::new(ExtractLimits::default());
        let doc = extract_document(Some("application/pdf"), &resource, &bytes, &budget)
            .unwrap_or_else(|e| panic!("{} must extract, got {e:?}", c.stem));

        assert_eq!(
            doc.content_type,
            ContentType::Pdf,
            "{} content_type",
            c.stem
        );
        assert_eq!(doc.charset_label, None, "{} is binary: no charset", c.stem);
        assert_eq!(doc.title.as_deref(), c.title, "{} title", c.stem);
        assert_eq!(doc.author.as_deref(), c.author, "{} author", c.stem);

        let fixture = format!("extract_pdf_{}", c.stem.replace('-', "_"));
        let want = fixture_body(&fixture, c.title);
        assert_eq!(
            words(&doc.body_text),
            words(&want),
            "{} body_text tokens must match the captured Tika body (whitespace-insensitive)",
            c.stem
        );

        // The XHTML body is not captured for PDF (#261 captured
        // extractFormat=text only), so it is not fixture-wise pinned, but a
        // text-bearing page must still render its content inside `<p>` blocks.
        if !doc.body_text.is_empty() {
            assert!(
                doc.body_xhtml.contains("<p>") && doc.body_xhtml.contains("</p>"),
                "{} body_xhtml must wrap page text in <p> elements, got {:?}",
                c.stem,
                doc.body_xhtml
            );
        }
    }
}

/// The Q1 divergence #1 from the #261 report, pinned at the extractor level:
/// a PDF whose content stream is malformed (valid xref/trailer, broken Flate
/// stream inside) is *swallowed as empty output* by `pdf-extract`, where
/// Tika/PDFBox throw `DataFormatException` and answer 500. `pdf-extract`
/// cannot tell "broken document" from "legitimately empty document" (an
/// image-only scanned page), so Wayfinder emits the same 200-empty shape for
/// both and records the status divergence in the fixture comparison suite.
///
/// Mutation target: if a guard is added that turns empty output into an error
/// (trying to "fix" the divergence by guessing), this `Ok` assertion fails.
#[test]
fn malformed_pdf_extracts_to_empty_not_an_error() {
    let bytes = std::fs::read(extract_inputs_dir().join("pdf-malformed-objects.pdf"))
        .expect("pdf-malformed-objects.pdf fixture must exist");
    let budget = Budget::new(ExtractLimits::default());
    let doc = extract_document(
        Some("application/pdf"),
        "pdf-malformed-objects.pdf",
        &bytes,
        &budget,
    )
    .expect("a malformed-content-stream PDF extracts to Ok(empty), not Err");
    assert!(
        doc.body_text.trim().is_empty(),
        "malformed PDF body must be empty (the swallowed-error shape), got {:?}",
        doc.body_text
    );
}

/// The legitimate-empty counterpart: an image-only (scanned) PDF has no text
/// layer, so empty output is correct, not an error. No OCR (the #268 epic's
/// standing constraint). This and the test above together prove Wayfinder
/// cannot and does not distinguish the two — the honest behaviour the report
/// ratified.
#[test]
fn image_only_pdf_extracts_to_empty() {
    let bytes = std::fs::read(extract_inputs_dir().join("pdf-image-only.pdf"))
        .expect("pdf-image-only.pdf fixture must exist");
    let budget = Budget::new(ExtractLimits::default());
    let doc = extract_document(
        Some("application/pdf"),
        "pdf-image-only.pdf",
        &bytes,
        &budget,
    )
    .expect("an image-only PDF extracts to Ok(empty)");
    assert!(doc.body_text.is_empty(), "image-only body must be empty");
}

/// An encrypted PDF the empty wire password cannot decrypt is a parse error
/// (-> HTTP 500), matching captured Tika/PDFBox (`InvalidPasswordException`).
/// Encrypted PDFs are off the realistic wire (the client sends no password);
/// both tools 500 there.
#[test]
fn encrypted_pdf_is_a_parse_error() {
    let bytes = std::fs::read(extract_inputs_dir().join("pdf-encrypted.pdf"))
        .expect("pdf-encrypted.pdf fixture must exist");
    let budget = Budget::new(ExtractLimits::default());
    let err = extract_document(
        Some("application/pdf"),
        "pdf-encrypted.pdf",
        &bytes,
        &budget,
    )
    .expect_err("an encrypted PDF must fail to extract, not succeed or panic");
    assert!(
        matches!(err, ExtractError::Parse(_)),
        "expected ExtractError::Parse (-> 500), got {err:?}"
    );
}

/// `broken.pdf` (a valid `%PDF-` header with no xref/trailer/objects) fails
/// inside `lopdf`'s structural parse, surfacing as a parse error -> 500. This
/// is the row that retires the `extract_corrupt_pdf` status divergence
/// (PRD divergence 10): once Wayfinder can fail *inside* a PDF parser the
/// captured 500 becomes reachable, so the divergence entry must be deleted,
/// not re-justified.
#[test]
fn broken_pdf_is_a_parse_error() {
    let bytes = std::fs::read(extract_inputs_dir().join("broken.pdf"))
        .expect("broken.pdf fixture must exist");
    let budget = Budget::new(ExtractLimits::default());
    let err = extract_document(Some("application/pdf"), "broken.pdf", &bytes, &budget)
        .expect_err("broken.pdf must fail to extract, not succeed or panic");
    assert!(
        matches!(err, ExtractError::Parse(_)),
        "expected ExtractError::Parse (-> 500), got {err:?}"
    );
}

/// Between-page cooperative cancellation (#261 Q2 proof): the
/// `max_pdf_pages` guard stops extraction before the next page. A 4-page
/// corpus file under a 1-page cap must fail with `StructuralLimit(PdfPages)`,
/// not extract silently or panic. `Budget::count_pdf_page` is the guard; the
/// checkpoint is the `begin_page` seam.
#[test]
fn pdf_extraction_stops_at_the_page_count_limit() {
    let bytes = std::fs::read(extract_inputs_dir().join("pdf-multipage.pdf"))
        .expect("pdf-multipage.pdf fixture must exist");
    let limits = ExtractLimits {
        max_pdf_pages: 1,
        ..ExtractLimits::default()
    };
    let budget = Budget::new(limits);
    let err = extract_document(
        Some("application/pdf"),
        "pdf-multipage.pdf",
        &bytes,
        &budget,
    )
    .expect_err("a 4-page PDF under a 1-page cap must be rejected, not extracted");
    assert!(
        matches!(
            err,
            ExtractError::StructuralLimit(StructuralLimitKind::PdfPages)
        ),
        "expected StructuralLimit(PdfPages) at the between-page checkpoint, got {err:?}"
    );
}

/// Deadline-based cooperative cancellation: a deadline already expired before
/// any page is processed must abort at the first `begin_page` checkpoint,
/// before a single byte of content-stream work runs. Made deterministic with
/// an injected clock (no sleep race).
#[test]
fn pdf_extraction_stops_at_an_expired_deadline() {
    let bytes = std::fs::read(extract_inputs_dir().join("pdf-multipage.pdf"))
        .expect("pdf-multipage.pdf fixture must exist");
    let start = std::time::Instant::now();
    let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // The clock reports the start instant for the budget construction call
    // (so the deadline is computed as start+deadline), then a far-future
    // instant for every checkpoint thereafter — so the deadline is expired by
    // the time any checkpoint runs. No sleep race.
    let clock: wayfinder::extract::Clock = {
        let ticks = Arc::clone(&ticks);
        Arc::new(move || {
            if ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                start
            } else {
                start + std::time::Duration::from_secs(3600)
            }
        })
    };
    let budget = Budget::with_clock(ExtractLimits::default(), clock);
    let err = extract_document(
        Some("application/pdf"),
        "pdf-multipage.pdf",
        &bytes,
        &budget,
    )
    .expect_err("an already-expired deadline must cancel PDF extraction");
    assert!(
        matches!(err, ExtractError::DeadlineExceeded),
        "expected DeadlineExceeded at the first checkpoint, got {err:?}"
    );
}

/// Within-page cooperative cancellation (#261 Q2 proof): the extracted-output
/// budget stops a single page part-way through, the mode the #261
/// `cancel_proof` demonstrated ("aborted page 1 at exactly N characters").
/// `pdf-extract`'s `output_character` seam is checked against the budget on
/// every character, so a cap smaller than one page aborts mid-page.
#[test]
fn pdf_extraction_stops_part_way_through_a_page_at_the_output_limit() {
    let bytes = std::fs::read(extract_inputs_dir().join("pdf-embedded-font.pdf"))
        .expect("pdf-embedded-font.pdf fixture must exist");
    let limits = ExtractLimits {
        max_output_scalars: 16, // far below the page's ~48 characters
        ..ExtractLimits::default()
    };
    let budget = Budget::new(limits);
    let err = extract_document(
        Some("application/pdf"),
        "pdf-embedded-font.pdf",
        &bytes,
        &budget,
    )
    .expect_err("an output cap below one page must abort extraction mid-page");
    assert!(
        matches!(err, ExtractError::OutputTooLarge(OutputLimitKind::Scalars)),
        "expected OutputTooLarge(Scalars) from the per-character charge, got {err:?}"
    );
    // Progress was made before the cap tripped (proves the check fires
    // *during* the page, not only at a page boundary).
    let produced = budget.output_text().len();
    assert!(
        produced > 0 && produced < 48,
        "extraction must stop part-way through the page (~48 chars), having produced {produced} bytes"
    );
}
