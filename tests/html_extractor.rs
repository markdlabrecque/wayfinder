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

use std::time::Instant;
use wayfinder::extract::{
    Budget, ExtractError, ExtractInput, ExtractLimits, OutputLimitKind, StructuralLimitKind,
    detect, extract,
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
    let b = budget();
    let result = extract(&input, &b);
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
    let b = budget();
    let extracted = extract(&input, &b)
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
    let b = budget();
    let extracted = extract(&input, &b)
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
    let b = budget();
    let extracted = extract(&input, &b)
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
/// This does *not* exercise the html5ever early-abort check (`||
/// tokenizer.sink.state.borrow().error.is_some()`), despite this input's
/// overrun landing mid-character-run: the document is small enough to fit in
/// one decode chunk *and* has a following `</title>` tag, so `feed` returns
/// `Script` off that tag before the per-chunk error probe is ever consulted
/// (verified directly — deleting the check leaves this test green). The
/// dedicated mutation test for that check is
/// `budget_exhausted_mid_character_run_aborts_within_one_chunk` below
/// (issue #275, follow-up #4 from #258's tracer report).
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
    let budget = Budget::new(limits);
    let result = extract(&input, &budget);
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

/// Issue #275 — mutation test for the html5ever early-abort check
/// (`|| tokenizer.sink.state.borrow().error.is_some()` in `HtmlExtractor::extract`,
/// `src/extract.rs`).
///
/// The check is exactly the class of code the working agreement makes
/// mutation-tested: "code whose whole value is failing correctly". It bounds
/// the tokenizer overshoot to a single chunk when the budget is exhausted
/// **mid-character-run with no following tag**. Every other HTML test has a
/// following tag, so an abort there is carried by `TokenSinkResult::Script`
/// on that tag whether or not this check exists — which is why reverting the
/// check leaves the full suite green (#258 round-2 reviewer).
///
/// Why work is the *only* observable. `TokenSinkResult::Script` is reachable
/// only from a tag token (html5ever's `process_token_and_continue` asserts
/// `Continue` for every other kind), so a budget blown on a character token
/// cannot signal until the next tag arrives. The sink's `error` guard then
/// drops every subsequent token before `charge_token`/`push_text`, so the
/// returned error and the accumulated output are byte-identical with or
/// without the check, and `Tokenizer::end()` is a no-op on our sink. The
/// sole difference is how many bytes html5ever's lexer runs over after the
/// violation: one chunk (with the check) versus the whole remaining input
/// (without). So this is a promptness test.
///
/// The assertion compares the extraction's wall time to the one piece of
/// work unavoidable in *both* paths — the `String::from_utf8_lossy` copy
/// `extract` performs up front — and requires the extraction to do little
/// more than that copy. Measuring both on the same machine in the same run
/// cancels runner speed to first order: the ratio is ~1.0 with the check and
/// ~6.5 with the arm deleted (16 MiB run), so a 3.0x bound has ~3x headroom
/// on the green side and still fails the mutation with ~2x to spare — it
/// does not fight CI runner speed the way an absolute millisecond bound
/// would. The `min` over several runs rejects scheduling spikes rather than
/// hiding real work.
///
/// Mutation test (recorded): deleting the `|| ...error.is_some()` arm makes
/// this test fail — the lexer runs over all 16 MiB instead of stopping one
/// chunk past the violation, and `extract` time rises to ~6.5x the copy.
#[test]
fn budget_exhausted_mid_character_run_aborts_within_one_chunk() {
    // A budget that blows on a character token: `<html>` and `<body>` are the
    // only tags (tokens 1 and 2, both under the limit), and everything after
    // is one unbroken character run with no following tag — so the only thing
    // that can carry an abort (`Script`) never appears.
    const MAX_XML_EVENTS: usize = 3;
    // 16 MiB: large enough that "the whole remaining input" is a real,
    // measurable amount of lexer work and small enough to keep the test light.
    const RUN_MIB: usize = 16;
    // The extraction may do at most this many times the unavoidable copy's
    // work. ~1.0x with the check, ~6.5x without; 3.0 sits between (see above).
    const EARLY_ABORT_MAX_COPY_MULTIPLES: f64 = 3.0;

    let head = "<html><body>";
    let run: String = "a".repeat(RUN_MIB * 1024 * 1024);
    let html = format!("{head}{run}");
    let bytes = html.as_bytes();

    // Lock the premise first: the budget must actually be exhausted, on the
    // structural event counter, and the input must have no following tag for
    // `Script` to ride on.
    let probe = Budget::new(ExtractLimits {
        max_xml_events: MAX_XML_EVENTS,
        ..ExtractLimits::default()
    });
    let input = ExtractInput {
        declared_type: Some("text/html"),
        resource_name: "sample.html",
        bytes,
    };
    let result = extract(&input, &probe);
    assert!(
        matches!(
            result,
            Err(ExtractError::StructuralLimit(
                StructuralLimitKind::XmlEvents
            ))
        ),
        "the character run must exhaust the XML-event budget, got {result:?}"
    );

    // The unavoidable work both paths share: the lossy copy `extract` does
    // internally. `min` over a few runs rejects scheduling spikes.
    let copy_ns = (0..5)
        .map(|_| {
            let t = Instant::now();
            let _ = String::from_utf8_lossy(bytes).into_owned();
            t.elapsed().as_nanos() as u64
        })
        .min()
        .expect("non-empty iterator");

    let extract_ns = (0..5)
        .map(|_| {
            let b = Budget::new(ExtractLimits {
                max_xml_events: MAX_XML_EVENTS,
                ..ExtractLimits::default()
            });
            let t = Instant::now();
            let r = extract(&input, &b);
            let ns = t.elapsed().as_nanos() as u64;
            assert!(
                matches!(
                    r,
                    Err(ExtractError::StructuralLimit(
                        StructuralLimitKind::XmlEvents
                    ))
                ),
                "every run must exhaust the budget the same way, got {r:?}"
            );
            ns
        })
        .min()
        .expect("non-empty iterator");

    let ratio = extract_ns as f64 / copy_ns.max(1) as f64;
    assert!(
        ratio < EARLY_ABORT_MAX_COPY_MULTIPLES,
        "the HTML extractor must stop one chunk past a mid-character-run \
         budget violation instead of lexing the whole remaining input: \
         extract was {ratio:.2}x the unavoidable lossy-copy cost (bound \
         {EARLY_ABORT_MAX_COPY_MULTIPLES}). If this fired green-side, raise \
         EARLY_ABORT_MAX_COPY_MULTIPLES only after confirming the \
         early-abort check in `HtmlExtractor::extract` is still present; if \
         it fired with the check deleted, that is the mutation this test \
         exists to catch (issue #275)."
    );
}
