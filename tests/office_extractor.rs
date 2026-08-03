//! Issue #260 — office-format extractors (DOCX/PPTX/XLSX/ODT/ODP/ODS/RTF).
//!
//! Unit-level: calls `extract::extract_document` directly (no HTTP) and pins
//! the internal `ExtractedDocument` fields against the committed Solr
//! fixtures. Expected `body_xhtml`/`body_text` are *derived* from
//! `solr-ref/responses/extract_<fmt>_{xml,text}.json`, never hand-typed, per
//! CLAUDE.md's compatibility contract. The differential harness
//! (`tests/differential.rs`) separately proves the full rendered envelope
//! matches end to end after the ratified office-metadata normalisation; this
//! suite pins the extractor's own field-by-field behaviour so a regression in
//! any one format names itself immediately, and it is what the
//! malformed-input and zip-bomb containment checks below hang off.

mod common;

use wayfinder::extract::{
    Budget, ContentType, ExtractError, ExtractLimits, ZipViolation, extract_document,
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

/// The extractor's `body_xhtml`, recovered from an `_xml` fixture by lifting
/// the `<body>…</body>` region — exactly what `ExtractRender::xhtml` wraps
/// `body_xhtml` in.
fn expected_body_xhtml(fmt: &str) -> String {
    let file = fixture_file(&format!("extract_{fmt}_xml"));
    let start = file
        .find("<body>")
        .map(|i| i + "<body>".len())
        .unwrap_or_else(|| panic!("extract_{fmt}_xml fixture has no <body>"));
    let end = file
        .find("</body>")
        .unwrap_or_else(|| panic!("extract_{fmt}_xml fixture has no </body>"));
    file[start..end].to_string()
}

/// The extractor's `body_text`, recovered from a `_text` fixture by dropping
/// the leading-newline run (finding 124) and the optional `title\n\n` prefix
/// that `ExtractRender::text` emits when the document carries a title.
fn expected_body_text(fmt: &str, title: Option<&str>) -> String {
    let file = fixture_file(&format!("extract_{fmt}_text"));
    let rest = file.trim_start_matches('\n');
    match title {
        Some(t) => rest
            .strip_prefix(&format!("{t}\n\n"))
            .unwrap_or_else(|| panic!("extract_{fmt}_text body did not start with title {t:?}"))
            .to_string(),
        None => rest.to_string(),
    }
}

struct Case {
    fmt: &'static str,
    mime: &'static str,
    content_type: ContentType,
    title: Option<&'static str>,
    author: Option<&'static str>,
}

#[test]
fn each_office_format_extracts_the_fixture_body_and_metadata() {
    let cases = [
        Case {
            fmt: "docx",
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            content_type: ContentType::Docx,
            title: Some("Office Capture Title"),
            author: Some("Ada Example"),
        },
        Case {
            fmt: "pptx",
            mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            content_type: ContentType::Pptx,
            title: Some("Office Capture Title"),
            author: Some("Ada Example"),
        },
        Case {
            fmt: "xlsx",
            mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            content_type: ContentType::Xlsx,
            title: Some("Office Capture Title"),
            author: Some("Ada Example"),
        },
        Case {
            fmt: "odt",
            mime: "application/vnd.oasis.opendocument.text",
            content_type: ContentType::Odt,
            title: None,
            author: None,
        },
        Case {
            fmt: "odp",
            mime: "application/vnd.oasis.opendocument.presentation",
            content_type: ContentType::Odp,
            title: None,
            author: None,
        },
        Case {
            fmt: "ods",
            mime: "application/vnd.oasis.opendocument.spreadsheet",
            content_type: ContentType::Ods,
            title: None,
            author: None,
        },
        Case {
            fmt: "rtf",
            mime: "application/rtf",
            content_type: ContentType::Rtf,
            title: None,
            author: None,
        },
    ];

    for c in cases {
        let bytes = std::fs::read(extract_inputs_dir().join(format!("sample.{}", c.fmt)))
            .unwrap_or_else(|e| panic!("read sample.{}: {e}", c.fmt));
        let budget = Budget::new(ExtractLimits::default());
        let doc = extract_document(Some(c.mime), &format!("sample.{}", c.fmt), &bytes, &budget)
            .unwrap_or_else(|e| panic!("{} must extract, got {e:?}", c.fmt));

        assert_eq!(doc.content_type, c.content_type, "{} content_type", c.fmt);
        assert_eq!(doc.title.as_deref(), c.title, "{} title", c.fmt);
        assert_eq!(doc.author.as_deref(), c.author, "{} author", c.fmt);
        assert_eq!(doc.charset_label, None, "{} is binary: no charset", c.fmt);
        assert_eq!(
            doc.body_xhtml,
            expected_body_xhtml(c.fmt),
            "{} body_xhtml (derived from extract_{}_xml fixture)",
            c.fmt,
            c.fmt
        );
        assert_eq!(
            doc.body_text,
            expected_body_text(c.fmt, c.title),
            "{} body_text (derived from extract_{}_text fixture)",
            c.fmt,
            c.fmt
        );
    }
}

/// Malformed office inputs must fail gracefully — an `Err`, never a panic and
/// never a false success. The differential harness already proves the route
/// renders these as the captured Solr 500; this pins the extractor's own
/// contract (`extract_document` returns `Err`) so a regression that panics or
/// silently returns empty output fails here rather than only in the HTTP
/// layer.
#[test]
fn malformed_office_inputs_fail_gracefully_not_with_a_panic() {
    let cases: &[(&str, &str)] = &[
        (
            "docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (
            "pptx",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ),
        (
            "xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        ("odt", "application/vnd.oasis.opendocument.text"),
        ("odp", "application/vnd.oasis.opendocument.presentation"),
        ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
        ("rtf", "application/rtf"),
    ];

    for (ext, mime) in cases {
        let bytes = std::fs::read(extract_inputs_dir().join(format!("broken.{ext}")))
            .unwrap_or_else(|e| panic!("read broken.{ext}: {e}"));
        let budget = Budget::new(ExtractLimits::default());
        let result = extract_document(Some(mime), &format!("broken.{ext}"), &bytes, &budget);
        assert!(
            result.is_err(),
            "broken.{ext} must extract to an Err, got {result:?}"
        );
    }
}

/// A DOCX-shaped zip bomb — `[Content_Types].xml` plus a `word/document.xml`
/// whose declared uncompressed size dwarfs its compressed size (~1000:1) — is
/// rejected by `ZipBudget`'s declared-ratio guard at admission time, before a
/// single byte is decompressed. This is the containment property the ratio
/// limit exists for; mutating the guard off must turn this test red.
#[test]
fn a_docx_shaped_zip_bomb_is_rejected_by_the_declared_ratio_guard() {
    let bytes = std::fs::read(extract_inputs_dir().join("bomb.docx"))
        .expect("bomb.docx fixture must exist");
    let budget = Budget::new(ExtractLimits::default());
    let err = extract_document(
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "bomb.docx",
        &bytes,
        &budget,
    )
    .expect_err("a zip bomb must be rejected, not extracted");
    assert!(
        matches!(err, ExtractError::ZipBudget(ZipViolation::RatioTooHigh)),
        "expected ZipBudget(RatioTooHigh), got {err:?}"
    );
}
