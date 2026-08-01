//! Solr TermsComponent (issue #155, PRD's contract-endpoint backlog):
//! `GET /solr/{core}/terms` — enumerates the analyzed inverted-index term
//! dictionary of a field with per-term document frequency.
//!
//! ## Ground truth
//!
//! `solr-ref/search-api/trace/00028.json` — a real `solr:9` response to
//! `GET .../terms?omitHeader=true&wt=json&json.nl=flat&terms=true&terms.fl=tm_X3b_en_title`:
//!
//! ```text
//! {"terms":{"tm_X3b_en_title":[
//!   "dog",2, "lazi",2, "quick",2, "about",1, "afternoon",1,
//!   "archiv",1, "brown",1, "cat",1, "day",1, "document",1]}}
//! ```
//!
//! That trace's underlying Drupal/Search-API corpus is not itself captured
//! anywhere in this repo (unlike `tests/mlt.rs`'s dedicated Solr container),
//! so the exact document text behind it is unknown. What *is* pinned by the
//! trace, and asserted here byte-for-byte, is: the ten analyzed terms, each
//! one's document frequency, and count-desc/term-asc ordering (`dog`/`lazi`/
//! `quick` tied at 2 and already alphabetical; the seven count-1 singletons
//! `about, afternoon, archiv, brown, cat, day, document` are in alphabetical
//! order too — confirming the tie-break is term-ascending, not insertion
//! order). `terms_matches_trace_analyzed_terms_with_count_desc_term_asc_order`
//! below indexes a small hand-built corpus, independently verified (via a
//! throwaway harness using the exact `tantivy` version this crate pins,
//! `0.26.1`, and the exact filter chain `src/schema.rs::build_tokenizers`
//! wires for `text_en`: `SimpleTokenizer` -> `RemoveLongFilter(40)` ->
//! `LowerCaser` -> `StopWordFilter(English)` -> `Stemmer(English)`) to
//! tokenize into precisely that same ten-term, same-counts, same-order set.
//! Nothing here is derived from what Wayfinder's own `/terms` handler
//! happens to produce — the handler does not exist yet.
//!
//! ## Premises verified before writing these tests (per the task spec)
//!
//! 1. **`CoreIndex::term_facet` cannot be the implementation path.**
//!    `src/core_index.rs::term_facet` (around line 2517) builds a Tantivy
//!    `TermsAggregation` over a *fast* (docValues) column — it aggregates
//!    already-decoded column values, never touches
//!    `InvertedIndexReader::terms()`. Run against a `text_en` field (not
//!    `fast`, and analyzed rather than raw), it cannot see stemmed tokens
//!    like `lazi`/`archiv` at all. The ticket's implementation note is
//!    correct as written; this is not a premise to escalate.
//! 2. **Tantivy's English stemmer does agree with Solr's `text_en` on the
//!    tokens this trace needs**, confirmed empirically (not assumed):
//!    `lazy` -> `lazi`, `archived`/`archive` -> `archiv`, `documents` ->
//!    `document`, `dogs`/`cats` -> `dog`/`cat`, matching the trace exactly.
//!    The stopword halves agree too: Tantivy 0.26.1's inlined English list is
//!    the same 33-word Lucene list (its own source comment says so), and
//!    `solr-ref/search-api/configset/stopwords_en.txt` — the file the trace's
//!    `text_en` actually loads, per `schema_extra_types.xml`'s
//!    `words="stopwords_en.txt"` — is that same list plus `s` and `t`. The
//!    corpora below stay inside the intersection of the two, so no assertion
//!    here rests on a word the two lists disagree about. The captured
//!    configset does show other analyzer differences
//!    (`StandardTokenizerFactory` vs Wayfinder's `SimpleTokenizer`,
//!    `LengthFilterFactory min="2"`, `WordDelimiterGraphFilterFactory`,
//!    `MappingCharFilterFactory`'s `accents_en.txt`, and those two extra
//!    stopwords); none of them has been verified against a capture, so this
//!    file claims none of them. The deferred `solr-ref/manifest.tsv` row is
//!    what settles them.
//! 3. **Doc-frequency-includes-deletes is a genuine, currently-unenforced
//!    property.** Nothing in `src/core_index.rs` today reads
//!    `InvertedIndexReader::terms()`/`TermDictionary` at all (grep confirms
//!    no caller), so there is no existing behaviour to contradict — this is
//!    new ground the implementor has to get right from Tantivy's own
//!    semantics (a `delete_term` + `commit()` tombstones a doc without
//!    touching its segment's postings/term dictionary; only a merge purges
//!    them), not something Wayfinder previously handled differently for
//!    another endpoint.
//!
//! ## Envelope shape
//!
//! `{terms: {<field>: [term, count, term, count, ...]}}` — the flat
//! `json.nl=flat` array shape is the only shape this endpoint's response
//! takes (per the ticket, no general named-list machinery is needed).
//! `omitHeader=true` (which the module always sends) suppresses
//! `responseHeader` entirely; its absence, or `omitHeader=false`, keeps it.
//!
//! ## An interpretation this file had to make
//!
//! The "undefined field in `terms.fl` errors in Solr's envelope" acceptance
//! criterion has no captured fixture behind it (the ticket's "Not in this
//! ticket" section explicitly defers a `terms` capture). There is no base
//! query for `/terms` to have partially run, so
//! `terms_undefined_field_errors_with_solr_envelope` below follows the
//! *pre-query* error precedent already in this codebase (`facet.range`'s
//! `PreQueryFacetError`, which renders with no `response` key at all — see
//! `src/lib.rs` around the `facet_result` block, and
//! `WfError` tests `without_with_response_there_is_no_response_key`) rather
//! than the *post-query* precedent (`facet_unknown_field.json`, which does
//! carry a `response` block). It asserts status 400, `error.code == 400`,
//! `error.msg` mentioning the undefined field's name, and the absence of a
//! `response` key and of a `terms` key — but does not pin the exact wording
//! of `error.msg`, since no fixture backs one.

// The `dead_code` allow for partially-used shared helpers is an inner
// attribute inside `tests/common/mod.rs`; a second `#![allow(dead_code)]`
// here would be a clippy error under `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

use common::{app_with_schema, get, post_docs, request};

/// `id` (string, fast, stored, unique key) plus two independent `text_en`
/// fields (`title`, `body`), so the multiple-`terms.fl` test can exercise two
/// distinct term dictionaries without the primary trace-shaped corpus (which
/// only ever populates `title`) needing to grow extra vocabulary. `views`
/// (`int`) exists solely so
/// `terms_non_text_field_is_rejected_rather_than_lossily_decoded` has a
/// declared, indexed, non-text field to point `terms.fl` at; no other test
/// mentions it, and no corpus populates it except that one's.
const TERMS_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true
"#;

async fn terms_app(corpus: &Value) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), TERMS_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, corpus).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the terms corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Reads `terms/<field>` as the flat `[term, count, term, count, ...]` array
/// `json.nl=flat` produces, panicking with the whole body if the shape is
/// wrong -- so a failure here still says *why*, not just that an assertion
/// didn't hold.
fn flat_terms<'a>(body: &'a Value, field: &str) -> &'a Vec<Value> {
    body.pointer(&format!("/terms/{field}"))
        .unwrap_or_else(|| panic!("no /terms/{field} in response: {body}"))
        .as_array()
        .unwrap_or_else(|| panic!("/terms/{field} is not an array: {body}"))
}

/// Doc frequency for one term out of a flat `[term, count, ...]` array,
/// `None` if the term is absent.
fn term_count(flat: &[Value], term: &str) -> Option<u64> {
    flat.iter()
        .position(|v| v.as_str() == Some(term))
        .and_then(|i| flat.get(i + 1))
        .and_then(Value::as_u64)
}

// --- the trace's exact term list ---------------------------------------

/// Hand-built corpus whose `title` field's analyzed vocabulary is exactly the
/// ten terms `solr-ref/search-api/trace/00028.json` reports, with exactly the
/// same per-term document frequency: `dog`/`lazi`/`quick` at 2 (each
/// appearing, in some inflected form, in exactly two of the three docs
/// below), the other seven at 1. No other vocabulary appears in `title`
/// across these three docs, so there is no ambiguity about what the default
/// `terms.limit=10` would have to cut -- confirmed independently via the
/// throwaway `tantivy` 0.26.1 harness described in the module doc comment:
/// `"quick brown lazy dog"` -> `[quick, brown, lazi, dog]`,
/// `"lazy afternoon about archived document"` ->
/// `[lazi, afternoon, about, archiv, document]`,
/// `"quick dog cat day"` -> `[quick, dog, cat, day]`.
fn trace_corpus() -> Value {
    json!([
        {"id": "t1", "title": "quick brown lazy dog"},
        {"id": "t2", "title": "lazy afternoon about archived document"},
        {"id": "t3", "title": "quick dog cat day"},
    ])
}

#[tokio::test]
async fn terms_matches_trace_analyzed_terms_with_count_desc_term_asc_order() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title").await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let expected = json!([
        "dog",
        2,
        "lazi",
        2,
        "quick",
        2,
        "about",
        1,
        "afternoon",
        1,
        "archiv",
        1,
        "brown",
        1,
        "cat",
        1,
        "day",
        1,
        "document",
        1,
    ]);
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&expected),
        "terms block must match the trace's exact analyzed terms, counts, and \
         count-desc/term-asc order, got {body}"
    );
}

// --- default limit ------------------------------------------------------

/// Twelve distinct, unstemmed, non-stopword tokens in a single document (so
/// every term ties at document frequency 1, isolating the *limit* behaviour
/// from the *count-desc* ordering already pinned above). With every count
/// tied, the tie-break is pure alphabetical order, so `terms.limit`'s default
/// of 10 must return exactly the first 10 of the 12 alphabetically --
/// dropping `theta` and `zeta`, the two that sort last -- confirmed
/// unstemmed/unfiltered by the same throwaway harness (none of the twelve are
/// in Tantivy's English stopword list or change under its English stemmer).
fn twelve_term_corpus() -> Value {
    json!([
        {"id": "d1", "title": "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu"},
    ])
}

#[tokio::test]
async fn terms_default_limit_is_ten_alphabetical_among_ties() {
    let (app, _dir) = terms_app(&twelve_term_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title").await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let flat = flat_terms(&body, "title");
    assert_eq!(
        flat.len(),
        20,
        "default terms.limit=10 must cap the response at 10 (term, count) pairs \
         out of 12 available terms, got {flat:?}"
    );
    let expected = json!([
        "alpha", 1, "beta", 1, "delta", 1, "epsilon", 1, "eta", 1, "gamma", 1, "iota", 1, "kappa",
        1, "lambda", 1, "mu", 1,
    ]);
    assert_eq!(
        Value::Array(flat.clone()),
        expected,
        "the 10 lowest-sorting (alphabetically, since every count ties at 1) \
         of the 12 terms must be returned, excluding theta/zeta, got {flat:?}"
    );
}

// --- multiple terms.fl ---------------------------------------------------

/// Two docs with independent `title`/`body` vocabulary, so a request naming
/// both fields must produce two independent term lists under `terms`, not
/// one field's terms leaking into the other's key or overwriting it.
/// Verified via the same harness: `"quick fox"`/`"quick cat"` (title) ->
/// `quick` at 2, `cat`/`fox` at 1 each; `"lazy dog"`/`"lazy cat"` (body) ->
/// `lazi` at 2, `cat`/`dog` at 1 each.
fn multi_field_corpus() -> Value {
    json!([
        {"id": "m1", "title": "quick fox", "body": "lazy dog"},
        {"id": "m2", "title": "quick cat", "body": "lazy cat"},
    ])
}

#[tokio::test]
async fn terms_multiple_fl_produces_multiple_independent_keys() {
    let (app, _dir) = terms_app(&multi_field_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.fl=body").await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["quick", 2, "cat", 1, "fox", 1])),
        "title's term list must be present and independent of body's, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/body"),
        Some(&json!(["lazi", 2, "cat", 1, "dog", 1])),
        "body's term list must be present and independent of title's, got {body}"
    );
    assert_eq!(
        body.pointer("/terms")
            .and_then(Value::as_object)
            .map(|o| o.len()),
        Some(2),
        "exactly the two requested fields must appear under terms, no more, no fewer, got {body}"
    );
}

// --- omitHeader -----------------------------------------------------------

#[tokio::test]
async fn terms_omit_header_true_suppresses_response_header() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&omitHeader=true").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=true must suppress responseHeader entirely, got {body}"
    );
    assert!(
        body.get("terms").is_some(),
        "the terms block itself must still be present, got {body}"
    );
}

#[tokio::test]
async fn terms_response_header_present_when_omit_header_absent() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader is absent, got {body}"
    );
}

#[tokio::test]
async fn terms_response_header_present_when_omit_header_false() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&omitHeader=false").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "responseHeader must be present when omitHeader=false, got {body}"
    );
}

// --- undefined field ------------------------------------------------------

#[tokio::test]
async fn terms_undefined_field_errors_with_solr_envelope() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=nosuchfield").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an undefined terms.fl field must 400, not panic or silently return an \
         empty block, got {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no /error/msg string in {body}"));
    assert!(
        msg.contains("nosuchfield"),
        "error.msg should name the offending field, got {msg:?}"
    );
    assert!(
        body.get("response").is_none(),
        "terms has no base query to have partially run, so no response block \
         should be attached, got {body}"
    );
    assert!(
        body.get("terms").is_none(),
        "an undefined field must not silently render an empty terms block, got {body}"
    );
}

// --- cross-segment summing --------------------------------------------------

/// Independent oracle for how many segments an app's data directory really
/// has: opens the same committed directory as a fresh `tantivy::Index`, not
/// through any Wayfinder type, and counts its searchable segment metas. Same
/// technique `tests/admin_ui_index_stats.rs::segment_count_oracle` uses, and
/// deliberately not `CoreIndex::segment_count` — the point is to establish the
/// premise of the test below without trusting the code under test.
fn segment_count_oracle(data_dir: &std::path::Path) -> usize {
    tantivy::Index::open_in_dir(data_dir)
        .expect("independent oracle must open the committed data directory")
        .searchable_segment_metas()
        .expect("independent oracle must list searchable segment metas")
        .len()
}

/// The property the ticket singles out: a term living in more than one segment
/// must report the **sum** of its per-segment `doc_freq`, not any single
/// segment's local value.
///
/// Every other test in this file indexes its corpus in one `post_docs` call,
/// which commits once and so produces exactly ONE segment — and with one
/// segment `totals.insert(term, doc_freq)` and
/// `*totals.entry(term).or_insert(0) += doc_freq` are indistinguishable. (The
/// delete test is no help either: its second commit writes a tombstone into
/// the existing segment rather than adding a new one.) So this test commits
/// three separate single-doc batches, asserts against an independent tantivy
/// read that the index really did end up multi-segment, and only then asserts
/// the frequency. Without the segment-count assertion a future merge-policy
/// change could quietly collapse this back into the one-segment case and take
/// the guard with it.
///
/// `sharedzz` appears in all three docs and `gamma` in one, so a correct
/// implementation reports 3 and 1. An overwriting one reports 1 for both,
/// since each segment's local `doc_freq` for `sharedzz` is 1. All four tokens
/// are outside every stopword list in play and unchanged by the English
/// stemmer — `alpha`/`beta`/`gamma` per `twelve_term_corpus` above, and the
/// `zz` suffix per `deletable_corpus`'s `widgetzz` below. (A first draft used
/// `onlyonce`, which the Snowball stemmer folds to `onlyonc`.)
#[tokio::test]
async fn terms_doc_frequency_sums_across_segments() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), TERMS_SCHEMA_TOML).expect("app must build");

    for (i, extra) in ["gamma", "alpha", "beta"].iter().enumerate() {
        let (status, body) = post_docs(
            &app,
            &json!([{"id": format!("s{i}"), "body": format!("sharedzz {extra}")}]),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "indexing batch {i} must succeed, got {body}"
        );
    }

    let segments = segment_count_oracle(&dir.path().join("data"));
    assert!(
        segments > 1,
        "this test is only meaningful on a multi-segment index: three separate \
         committed batches must leave more than one searchable segment, but an \
         independent tantivy read found {segments}. If a merge policy change \
         made this single-segment, the cross-segment summing guard below is \
         no longer guarding anything and needs a different construction."
    );

    let (status, body) = get(&app, "terms?terms=true&terms.fl=body").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let flat = flat_terms(&body, "body");

    assert_eq!(
        term_count(flat, "sharedzz"),
        Some(3),
        "sharedzz is in one document in each of the {segments} segments, so its \
         document frequency must be the SUM across segments (3), not a single \
         segment's local doc_freq (1), got {flat:?}"
    );
    assert_eq!(
        term_count(flat, "gamma"),
        Some(1),
        "gamma is in exactly one document overall, so summing must not inflate \
         it either, got {flat:?}"
    );
    assert_eq!(
        body.pointer("/terms/body"),
        Some(&json!(["sharedzz", 3, "alpha", 1, "beta", 1, "gamma", 1])),
        "the whole per-field list must be the cross-segment merge, ordered \
         count-desc then term-asc, got {body}"
    );
}

// --- terms gating -----------------------------------------------------------

/// `terms=false` must produce no `terms` key at all: a Solr search component
/// whose gating boolean is false contributes nothing to the response.
///
/// No fixture pins this — the trace only sends `terms=true`, and the ticket
/// defers the `/terms` capture. It is asserted anyway because the handler
/// previously inserted `terms` unconditionally, which contradicted its own doc
/// comment ("Without it there is no `terms` block at all"), and because the
/// gated reading matches the one *captured* precedent for an optional block:
/// `facet_counts` is absent entirely unless `facet` is requested (finding 4).
/// The deferred capture is what would overturn this; see the handler's doc
/// comment.
#[tokio::test]
async fn terms_false_produces_no_terms_block() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=false&terms.fl=title").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a disabled component is not an error, got {body}"
    );
    assert!(
        body.get("terms").is_none(),
        "terms=false must suppress the terms block entirely, not render it \
         empty, got {body}"
    );
}

/// The same for an absent `terms` param — the component is off by default.
#[tokio::test]
async fn terms_absent_produces_no_terms_block() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms.fl=title").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the endpoint still 200s with the component off, got {body}"
    );
    assert!(
        body.get("terms").is_none(),
        "with no terms param there must be no terms block, got {body}"
    );
    assert!(
        body.get("responseHeader").is_some(),
        "suppressing the terms block must not also suppress responseHeader, \
         got {body}"
    );
}

/// The case that must NOT be swept up by the gating above: `terms=true` with
/// no `terms.fl` runs the component, which contributes an empty list. The
/// block is present and empty. `src/coverage.rs`'s `terms.terms` response
/// denominator probe issues exactly this request and requires `terms` to be an
/// object, so this pins the distinction the gating fix has to preserve.
#[tokio::test]
async fn terms_true_without_fl_produces_an_empty_terms_object() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.get("terms"),
        Some(&json!({})),
        "terms=true with no terms.fl must render an empty terms object, not \
         omit the block, got {body}"
    );
}

// --- non-text field ---------------------------------------------------------

/// A `terms.fl` naming a declared but non-text field must 400 rather than
/// enumerate the field's raw dictionary bytes.
///
/// An `int` field's term dictionary holds Tantivy's fixed-width
/// order-preserving encoding, not UTF-8. Decoding it lossily returned
/// replacement-character keys — and because distinct encoded terms can decode
/// to the *same* replacement string, their unrelated document frequencies were
/// summed into one `BTreeMap` key. That is a wrong answer served with a 200,
/// which is worse than a refusal. The two docs below carry `views` values
/// chosen to differ only in bytes that are not valid UTF-8 on their own, so a
/// lossy implementation returns a garbage single-key list here rather than
/// anything a caller could use.
#[tokio::test]
async fn terms_non_text_field_is_rejected_rather_than_lossily_decoded() {
    let (app, _dir) = terms_app(&json!([
        {"id": "n1", "body": "alpha", "views": 1},
        {"id": "n2", "body": "beta", "views": 300},
    ]))
    .await;

    let (status, body) = get(&app, "terms?terms=true&terms.fl=views").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-text terms.fl must 400, not return replacement-character terms \
         with a 200, got {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no /error/msg string in {body}"));
    assert!(
        msg.contains("views"),
        "error.msg should name the offending field, got {msg:?}"
    );
    assert!(
        body.get("terms").is_none(),
        "a rejected field must not also render a terms block, got {body}"
    );
    assert!(
        !body.to_string().contains('\u{fffd}'),
        "no U+FFFD replacement character should reach the response at all, \
         got {body}"
    );
}

/// A `string` field, on the other hand, must still be enumerable: it is
/// unanalyzed but its dictionary is raw UTF-8, and Solr's own TermsComponent
/// enumerates a `StrField` happily. This is the boundary of the rejection
/// above — a check written as "text_* only" instead of "UTF-8 only" would
/// wrongly refuse here.
#[tokio::test]
async fn terms_string_field_is_still_enumerable() {
    let (app, _dir) = terms_app(&json!([
        {"id": "Zed", "body": "alpha"},
        {"id": "abe", "body": "beta"},
    ]))
    .await;

    let (status, body) = get(&app, "terms?terms=true&terms.fl=id").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a string field's dictionary is UTF-8 and must be enumerable, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/id"),
        Some(&json!(["Zed", 1, "abe", 1])),
        "a string field is unanalyzed, so its terms are the literal stored \
         values, in byte-ascending order (uppercase before lowercase), got {body}"
    );
}

// --- deleted-doc doc frequency guard ---------------------------------------

/// Two docs sharing a rare, deliberately unstemmed/non-stopword term
/// (`widgetzz`) so its document frequency starts at exactly 2. Deleting one
/// of the two docs and committing (a hard commit + reader reload per
/// `CoreIndex::schedule_commit`'s doc comment, but *not* a merge -- nothing
/// in this test path calls one) must NOT drop that count to 1: Solr's
/// TermsComponent reads raw Lucene `docFreq`, which is untouched by deleting
/// a document until a merge physically rewrites the segment's postings. This
/// is the guard the ticket calls for: a future "fix" that filters doc
/// frequency by the live-docs bitmap must fail this test loudly.
fn deletable_corpus() -> Value {
    json!([
        {"id": "w1", "body": "widgetzz alpha"},
        {"id": "w2", "body": "widgetzz beta"},
    ])
}

#[tokio::test]
async fn terms_doc_frequency_includes_deleted_docs_without_a_merge() {
    let (app, _dir) = terms_app(&deletable_corpus()).await;

    let (status, body) = get(&app, "terms?terms=true&terms.fl=body").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let before = flat_terms(&body, "body").clone();
    assert_eq!(
        term_count(&before, "widgetzz"),
        Some(2),
        "widgetzz must start at document frequency 2 across w1/w2, got {before:?}"
    );

    let (status, body) = request(
        &app,
        "POST",
        "update?commit=true",
        Some(r#"{"delete":{"id":"w1"}}"#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "deleting w1 (with a commit, but no merge) must succeed, got {body}"
    );

    let (status, body) = get(&app, "terms?terms=true&terms.fl=body").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let after = flat_terms(&body, "body").clone();
    assert_eq!(
        term_count(&after, "widgetzz"),
        Some(2),
        "document frequency must still include the deleted (but not yet \
         merged-away) w1, so widgetzz must remain at 2, not drop to 1, got {after:?}"
    );
}
