//! HTML extractor tests (issue #258, spec item 5), at `wayfinder::extract`'s
//! public API level — `detect()`/`dispatch()`/`extract()` directly, not via
//! HTTP. The HTTP-level fixture comparisons for HTML input live in
//! `tests/extract_route.rs`; this file is for extractor-internal behaviour
//! (script/style/template exclusion, title/author retention) that does not
//! need a whole fixture round trip to pin.
//!
//! Premise-check finding (stage 1, issue #258): `src/extract.rs`'s
//! `sniff_markup` already resolves an XHTML document opening with `<?xml`
//! to `ContentType::Html` when an `<html>` root appears in the leading
//! window (see `window_contains_html_root`) — this was believed, per the
//! spec, to still need fixing in this issue ("Resolve the `<?xml`-outranks-
//! `text/html` ponytail... that #257 left for this issue"), but tracing the
//! current code shows it already handles this case. `detect_xhtml_declaration_is_already_resolved_to_html`
//! below is a CONFIRMATION test and passes today (green) — it is included
//! anyway so a future regression here is caught, and so the discrepancy
//! between the spec's wording and the current codebase is pinned down
//! rather than silently assumed. Every other test in this file exercises
//! `dispatch(ContentType::Html)`/`extract()` end to end and is genuinely red
//! today, since `dispatch` has no HTML arm yet.

use wayfinder::extract::{
    Budget, ExtractError, ExtractInput, ExtractLimits, OutputLimitKind, detect, extract,
};

fn budget() -> Budget {
    Budget::new(ExtractLimits::default())
}

/// See the module doc comment: this specific assertion is a CONFIRMATION,
/// not a red test — `sniff_markup` already returns `ContentType::Html` for
/// this input today.
#[test]
fn detect_xhtml_declaration_is_already_resolved_to_html() {
    let bytes = b"<?xml version=\"1.0\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head></head><body>hi</body></html>";
    let content_type = detect(None, "sample.html", bytes);
    assert_eq!(
        content_type,
        wayfinder::extract::ContentType::Html,
        "an XHTML document opening with an XML declaration must detect as Html \
         (already true in the current sniff_markup, per stage-1 tracing)"
    );
}

/// Genuinely red: `dispatch(ContentType::Html)` returns `None` today, so
/// `extract()` on this same XHTML-declared input currently comes back
/// `UnsupportedFormat` rather than a successful `Extracted`.
#[test]
fn xhtml_declared_document_reaches_the_html_extractor() {
    let bytes = b"<?xml version=\"1.0\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>T</title></head><body><p>hello</p></body></html>";
    let input = ExtractInput {
        declared_type: None,
        resource_name: "sample.html",
        bytes,
    };
    let mut b = budget();
    let result = extract(&input, &mut b);
    assert!(
        result.is_ok(),
        "an XHTML-declared document must reach the HTML extractor and succeed, got {result:?}"
    );
}

/// `script`/`style`/`template` content must not appear in the extracted
/// text — genuinely red: no HTML extractor exists yet, so this input 415s
/// (`UnsupportedFormat`) instead of returning text with those bodies
/// excluded.
#[test]
fn script_style_template_content_is_excluded() {
    let html = br#"<html><head><style>body { color: red; }</style></head>
<body>
<script>alert('should not appear');</script>
<template><p>should not appear either</p></template>
<p>Visible paragraph text.</p>
</body></html>"#;
    let input = ExtractInput {
        declared_type: Some("text/html"),
        resource_name: "sample.html",
        bytes: html,
    };
    let mut b = budget();
    let extracted = extract(&input, &mut b)
        .unwrap_or_else(|e| panic!("HTML extraction must succeed once implemented, got {e:?}"));

    assert!(
        extracted.text.contains("Visible paragraph text."),
        "the visible paragraph must be present in the extracted text, got {:?}",
        extracted.text
    );
    assert!(
        !extracted.text.contains("should not appear"),
        "script/style/template content must be excluded from the extracted text, got {:?}",
        extracted.text
    );
    assert!(
        !extracted.text.contains("color: red"),
        "style element content must be excluded, got {:?}",
        extracted.text
    );
}

/// `<title>` and `<meta name="author">` must be retained in
/// `ExtractMetadata`. Genuinely red (no HTML extractor exists).
#[test]
fn title_and_author_are_retained_in_metadata() {
    let html = br#"<html><head><title>My Document Title</title>
<meta name="author" content="Jane Doe"></head>
<body><p>Body text.</p></body></html>"#;
    let input = ExtractInput {
        declared_type: Some("text/html"),
        resource_name: "sample.html",
        bytes: html,
    };
    let mut b = budget();
    let extracted = extract(&input, &mut b)
        .unwrap_or_else(|e| panic!("HTML extraction must succeed once implemented, got {e:?}"));

    assert_eq!(
        extracted.metadata.title.as_deref(),
        Some("My Document Title"),
        "title must be retained in ExtractMetadata, got {:?}",
        extracted.metadata.title
    );
    assert_eq!(
        extracted.metadata.author.as_deref(),
        Some("Jane Doe"),
        "author must be retained in ExtractMetadata, got {:?}",
        extracted.metadata.author
    );
}

/// A document with no explicit `<title>` must leave `metadata.title` unset
/// rather than inventing one — narrow companion to the retention test above,
/// same red reason (no HTML extractor exists).
#[test]
fn absent_title_leaves_metadata_title_none() {
    let html = b"<html><head></head><body><p>No title here.</p></body></html>";
    let input = ExtractInput {
        declared_type: Some("text/html"),
        resource_name: "sample.html",
        bytes: html,
    };
    let mut b = budget();
    let extracted = extract(&input, &mut b)
        .unwrap_or_else(|e| panic!("HTML extraction must succeed once implemented, got {e:?}"));
    assert_eq!(
        extracted.metadata.title, None,
        "no <title> element must leave metadata.title unset, got {:?}",
        extracted.metadata.title
    );
}

/// `<title>` accumulation must be charged against the same `Budget` as the
/// body text. The title is metadata, not body output, but it is still
/// extracted content a hostile upload could grow without bound — the one
/// unbudgeted allocation path left in the HTML extractor (issue #272).
///
/// A `<title>` long enough to exhaust a tiny `max_output_bytes`, with the
/// overrun happening inside one long character run and no following tag
/// token, also exercises the html5ever early-abort check (`||
/// tokenizer.sink.state.borrow().error.is_some()`): a budget blown
/// mid-character-run has no tag to carry `TokenSinkResult::Script`, so the
/// per-chunk error probe is the only thing that stops the run. That check
/// was the untested follow-up #4 from #258's tracer report; this test
/// covers it too.
#[test]
fn title_accumulation_is_charged_against_the_output_budget() {
    let big_title = "x".repeat(1000);
    let html =
        format!("<html><head><title>{big_title}</title></head><body><p>hi</p></body></html>");
    let input = ExtractInput {
        declared_type: Some("text/html"),
        resource_name: "bigtitle.html",
        bytes: html.as_bytes(),
    };
    // Every other default limit is generous enough not to trip; only the
    // output-byte ceiling is tightened so the title alone busts it.
    let limits = ExtractLimits {
        max_output_bytes: 10,
        ..ExtractLimits::default()
    };
    let mut budget = Budget::new(limits);
    let result = extract(&input, &mut budget);
    assert!(
        matches!(
            result,
            Err(ExtractError::OutputTooLarge(OutputLimitKind::Bytes))
        ),
        "a <title> long enough to exhaust max_output_bytes must fail as \
         OutputTooLarge(Bytes) — the documented 400 budget-violation status \
         (extraction-output-too-large) — rather than accumulating without \
         limit, got {result:?}"
    );
}
