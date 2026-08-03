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
//! The captured update in `solr-ref/search-api/trace/00001.json` includes the
//! singular phrase `Quick thinking saves the day`, and the later terms trace
//! preserves `day`; together they pin Search API's Snowball behavior separately
//! from canonical `_default` text_en. The exact full source-to-term mapping is
//! not reconstructed here. What is asserted byte-for-byte is: the ten analyzed terms, each
//! one's document frequency, and count-desc/term-asc ordering (`dog`/`lazi`/
//! `quick` tied at 2 and already alphabetical; the seven count-1 singletons
//! `about, afternoon, archiv, brown, cat, day, document` are in alphabetical
//! order too — confirming the tie-break is term-ascending, not insertion
//! order). `terms_matches_trace_analyzed_terms_with_count_desc_term_asc_order`
//! below indexes a small hand-built corpus, independently verified (via a
//! throwaway harness using the exact `tantivy` version this crate pins,
//! `0.26.1`, and the explicit `search_api_text_en` custom chain declared in
//! `TERMS_SCHEMA_TOML`: `SimpleTokenizer` -> `LowerCaser` ->
//! `StopWordFilter(English)` -> `Stemmer(English)`) to tokenize into precisely
//! that same ten-term, same-counts, same-order set. This pins the captured
//! stemming outcome, not Search API's full Solr analyzer chain.
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
//! ## Settled by a capture (issue #308)
//!
//! The "undefined field in `terms.fl`" case was once an *inference* with no
//! fixture behind it, and this file inferred a 400 (the pre-query error
//! precedent). Finding 141 / `terms_prefix_unknown_field` has now settled it
//! the other way: `terms.fl=nosuchfield&terms.prefix=a` answers **HTTP 200**
//! with `{"terms":{"nosuchfield":[]}}` — the field's key is present with an
//! empty list, not a 400. `terms_undefined_field_yields_an_empty_list` below
//! pins that. This matters for #308's own purpose: stock
//! `search_api_autocomplete` names fulltext fields that an index may not
//! have, and a 400 there breaks autocomplete on any index missing one.
//!
//! What stays a 400 is a *defined but non-text* field
//! (`terms_non_text_field_is_rejected_rather_than_lossily_decoded`): no
//! fixture covers it either way, so its comment still says so, and it keeps
//! its 400. The undefined-vs-non-text split lives in `check_terms_field`.

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
type = "search_api_text_en"
stored = true

[[fields]]
name = "body"
type = "search_api_text_en"
stored = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

# The captured Search API configset uses SnowballPorter, unlike the canonical
# `_default` text_en Porter behavior covered by the differential core.
[[field_types]]
name = "search_api_text_en"
tokenizer = "simple"
[[field_types.filters]]
kind = "lowercase"
[[field_types.filters]]
kind = "stopwords"
language = "english"
[[field_types.filters]]
kind = "stemmer"
language = "english"
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

/// An undefined `terms.fl` answers **HTTP 200** with the field's key present
/// and an empty list — finding 141 / `terms_prefix_unknown_field`, the capture
/// that settled the inference this file used to make (a 400). See the
/// "Settled by a capture" note in the module docs above.
///
/// This is the case that matters for #308's purpose: stock
/// `search_api_autocomplete` names fulltext fields an index may not have, so a
/// 400 here breaks autocomplete on any such index.
#[tokio::test]
async fn terms_undefined_field_yields_an_empty_list() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=nosuchfield").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an undefined terms.fl must 200 with an empty list, not 400 -- \
         finding 141 / terms_prefix_unknown_field settled this, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/nosuchfield"),
        Some(&json!([])),
        "the undefined field's key must be present with an empty list, got {body}"
    );
    assert!(
        body.get("error").is_none(),
        "an undefined field is not an error, got {body}"
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
    assert_eq!(
        body.pointer("/error/metadata/3").and_then(Value::as_str),
        Some("wayfinder::TermsUnsupportedField"),
        "a declared non-text field must be rejected as non-text, not \
         coincidentally as undefined, got {body}"
    );
    assert!(
        msg.contains("non-text field"),
        "error.msg should say the field is non-text, not merely undefined, \
         got {msg:?}"
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

// --- dynamic fields (regression: check_terms_field/field_terms consult only
// WayfinderSchema::field, so a name that only matches a [[dynamic_fields]]
// rule 400s as "undefined field" even though CoreIndex::field_target resolves
// it fine for /select) ---------------------------------------------------

/// Two `[[dynamic_fields]]` rules shaped after the two the bug report's own
/// repro and fix note: `tm_X3b_en_*` (multi-valued, English-stemmed text --
/// the exact pattern `presets/search-api.toml:113-117` declares and the exact
/// one `tm_X3b_en_title` in the repro matches) and `is_*` (single-valued int,
/// `presets/search-api.toml` around the same block) for the non-text-dynamic
/// rejection test. `id` is the only static field, so every other name in this
/// file's dynamic tests is resolved purely through a `[[dynamic_fields]]`
/// rule, not a declared one -- the exact condition `check_terms_field`/
/// `field_terms` fail to handle today.
const DYNAMIC_TERMS_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[dynamic_fields]]
pattern = "tm_X3b_en_*"
type = "text_en"
multi_valued = true
stored = true

[[dynamic_fields]]
pattern = "is_*"
type = "int"
stored = true
fast = true
"#;

async fn dynamic_terms_app(corpus: &Value) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), DYNAMIC_TERMS_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, corpus).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the dynamic-field terms corpus must succeed, got {body}"
    );
    (app, dir)
}

/// The trace's exact corpus (see `trace_corpus` above) reindexed under a
/// dynamic name instead of the static `title` field, so a match against the
/// same expected term list isolates "does dynamic resolution work at all"
/// from "does the analyzer chain agree with the trace" -- the latter is
/// already pinned by `terms_matches_trace_analyzed_terms_with_count_desc_term_asc_order`.
fn dynamic_trace_corpus() -> Value {
    json!([
        {"id": "t1", "tm_X3b_en_title": ["quick brown lazy dog"]},
        {"id": "t2", "tm_X3b_en_title": ["lazy afternoon about archived document"]},
        {"id": "t3", "tm_X3b_en_title": ["quick dog cat day"]},
    ])
}

/// The defect: `terms.fl=tm_X3b_en_title` names nothing in `[[fields]]`, only
/// a `[[dynamic_fields]]` pattern -- exactly `presets/search-api.toml`'s own
/// `tm_X3b_en_*` rule, and exactly the request the bug report reproduced
/// 400ing (`GET /solr/search_api_capture/terms?terms=true&terms.fl=tm_X3b_en_title`)
/// even though `select?q=tm_X3b_en_title:lazy` on the same app resolves the
/// same name via `CoreIndex::field_target` and returns real hits. `/terms`
/// must resolve it the same way, not just statically-declared names.
#[tokio::test]
async fn terms_resolves_dynamic_field_name_via_dynamic_fields_rule() {
    let (app, _dir) = dynamic_terms_app(&dynamic_trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=tm_X3b_en_title").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a terms.fl naming a name only a [[dynamic_fields]] rule matches must \
         resolve, not 400 as \"undefined field\" the way a truly unknown name \
         does, got {body}"
    );

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
        body.pointer("/terms/tm_X3b_en_title"),
        Some(&expected),
        "the same corpus/analyzer chain as the static-field trace test above \
         must produce the same ten analyzed terms, counts, and count-desc/\
         term-asc order under a dynamically-resolved name, got {body}"
    );
}

/// The specific failure mode a naive fix invites: resolving `tm_X3b_en_title`
/// to its storage field (`CoreIndex::field_target`'s `_dynamic_text` catch-all)
/// and then keying the response by *that* name, or by whatever internal path
/// string the resolution step used, instead of the name the client actually
/// asked for in `terms.fl`.
#[tokio::test]
async fn terms_dynamic_field_key_is_the_requested_name_not_the_storage_field() {
    let (app, _dir) = dynamic_terms_app(&dynamic_trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=tm_X3b_en_title").await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let terms_obj = body
        .pointer("/terms")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("no /terms object in {body}"));
    let keys: Vec<&str> = terms_obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["tm_X3b_en_title"],
        "the terms block must be keyed by the name the client requested in \
         terms.fl, not by `_dynamic_text` or any other internal storage-field \
         name `field_target` resolves it to, got {body}"
    );
}

/// Two dynamic names matched by the *same* `[[dynamic_fields]]` rule, with
/// disjoint-but-overlapping vocabularies (`cat` legitimately appears in both
/// fields' own corpora -- that is not leakage, it is two independent
/// documents each containing "cat"). Because Tantivy stores every match of
/// this rule in one shared `_dynamic_text` JSON container with the field name
/// as a path prefix inside the term (the reviewer's probe:
/// `tm_X3b_en_title\0slazi`), an implementation that enumerates that shared
/// container's whole term dictionary for either name -- rather than filtering
/// by path -- reports one field's terms under the other's key too. `quick`/
/// `fox` only ever appear in `tm_X3b_en_title`; `lazi`/`dog` only in
/// `tm_X3b_en_body`. This is the test most likely to catch a wrong fix.
fn two_dynamic_fields_corpus() -> Value {
    json!([
        {"id": "x1", "tm_X3b_en_title": ["quick fox"], "tm_X3b_en_body": ["lazy dog"]},
        {"id": "x2", "tm_X3b_en_title": ["quick cat"], "tm_X3b_en_body": ["lazy cat"]},
    ])
}

#[tokio::test]
async fn terms_dynamic_fields_do_not_leak_across_the_shared_catch_all_container() {
    let (app, _dir) = dynamic_terms_app(&two_dynamic_fields_corpus()).await;
    let (status, body) = get(
        &app,
        "terms?terms=true&terms.fl=tm_X3b_en_title&terms.fl=tm_X3b_en_body",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    assert_eq!(
        body.pointer("/terms/tm_X3b_en_title"),
        Some(&json!(["quick", 2, "cat", 1, "fox", 1])),
        "tm_X3b_en_title's own vocabulary only, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/tm_X3b_en_body"),
        Some(&json!(["lazi", 2, "cat", 1, "dog", 1])),
        "tm_X3b_en_body's own vocabulary only, got {body}"
    );

    let title_flat = flat_terms(&body, "tm_X3b_en_title");
    for leaked in ["lazi", "dog"] {
        assert!(
            term_count(title_flat, leaked).is_none(),
            "tm_X3b_en_body's term {leaked:?} must not appear under \
             tm_X3b_en_title just because both share the `_dynamic_text` \
             catch-all container, got {body}"
        );
    }
    let body_flat = flat_terms(&body, "tm_X3b_en_body");
    for leaked in ["quick", "fox"] {
        assert!(
            term_count(body_flat, leaked).is_none(),
            "tm_X3b_en_title's term {leaked:?} must not appear under \
             tm_X3b_en_body, got {body}"
        );
    }
}

/// A name matching no `[[dynamic_fields]]` pattern and no `[[fields]]` entry
/// yields an empty list (200), exactly like any other undefined field after
/// finding 141 (`terms_prefix_unknown_field`). The guard that a dynamic-only
/// name must not be a rubber stamp now lives entirely in the *non-text*
/// rejection: `terms_dynamic_field_of_non_text_type_is_rejected` below still
/// 400s a dynamically-resolved `int`. An undefined name is no longer an error
/// at all.
#[tokio::test]
async fn terms_dynamic_name_matching_no_rule_yields_an_empty_list() {
    let (app, _dir) = dynamic_terms_app(&dynamic_trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=zz_no_such_prefix").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a name matching no rule is simply undefined, and an undefined field \
         yields an empty list (200) after finding 141, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/zz_no_such_prefix"),
        Some(&json!([])),
        "the requested name must be present as a key with an empty list, got {body}"
    );
    assert!(body.get("error").is_none(), "got {body}");
}

/// A name matched only by a `[[dynamic_fields]]` rule whose type is non-text
/// (`is_*` -> `int`, mirroring `presets/search-api.toml`'s own `is_*` rule)
/// must 400 the same way a declared non-text field does, not enumerate the
/// shared `_dynamic` catch-all's fixed-width numeric encoding.
#[tokio::test]
async fn terms_dynamic_field_of_non_text_type_is_rejected() {
    let (app, _dir) = dynamic_terms_app(&json!([
        {"id": "n1", "is_weight": 1},
        {"id": "n2", "is_weight": 300},
    ]))
    .await;

    let (status, body) = get(&app, "terms?terms=true&terms.fl=is_weight").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dynamically-resolved non-text field must 400, not return \
         replacement-character or otherwise garbage terms with a 200, got {body}"
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
        msg.contains("is_weight"),
        "error.msg should name the offending field, got {msg:?}"
    );
    assert_eq!(
        body.pointer("/error/metadata/3").and_then(Value::as_str),
        Some("wayfinder::TermsUnsupportedField"),
        "a dynamically-resolved non-text field must be rejected as non-text, \
         not coincidentally as undefined, got {body}"
    );
    assert!(
        msg.contains("non-text field"),
        "error.msg should say the field is non-text, not merely undefined, \
         got {msg:?}"
    );
    assert!(
        body.get("terms").is_none(),
        "a rejected field must not also render a terms block, got {body}"
    );
    assert!(
        !body.to_string().contains('\u{fffd}'),
        "no U+FFFD replacement character should reach the response at all, got {body}"
    );
}

/// The exact motivating request, against the exact shipped preset, not a
/// synthetic schema approximating it: `presets/search-api.toml` loaded
/// as-is, a corpus reproducing the trace's `tm_X3b_en_title` vocabulary, and
/// `GET .../terms?terms=true&terms.fl=tm_X3b_en_title` -- the request the
/// bug report's independent reviewer showed 400ing.
fn search_api_preset_toml() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/search-api.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "presets/search-api.toml must exist and be readable: {e} (path: {})",
            path.display()
        )
    })
}

#[tokio::test]
async fn terms_resolves_the_shipped_drupal_preset_tm_x3b_en_title_field() {
    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), &search_api_preset_toml())
        .expect("presets/search-api.toml must build a working app");
    let (status, resp) = post_docs(
        &app,
        &json!([
            {"id": "doc1", "tm_X3b_en_title": ["quick brown lazy dog"]},
            {"id": "doc2", "tm_X3b_en_title": ["lazy afternoon about archived document"]},
            {"id": "doc3", "tm_X3b_en_title": ["quick dog cat day"]},
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing into the shipped preset must succeed, got {resp}"
    );

    let (status, body) = get(&app, "terms?terms=true&terms.fl=tm_X3b_en_title").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "this is the exact request shape the bug report reproduced 400ing \
         against the shipped preset itself \
         (GET .../terms?terms=true&terms.fl=tm_X3b_en_title), not a synthetic \
         schema merely approximating it, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/tm_X3b_en_title"),
        Some(&json!([
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
        ])),
        "got {body}"
    );
}

// --- json.nl honesty ---------------------------------------------------------
//
// `TERMS_PARAMS` lists `json.nl` and accepts it with any value, but the
// handler always renders the flat `[term, count, ...]` shape regardless --
// unlike `src/facet.rs`'s `JsonNl::from_params`, which actually honours
// `map`/`arrarr`/`arrmap` for facet counts. The handler's own doc comment
// argues "listing a param here that the handler ignores would be worse than
// 400ing it, since it would silently answer the wrong question"; these tests
// hold the handler to that standard rather than merely restating the status
// quo. Chosen interpretation: `json.nl=flat` (and the default, absent
// `json.nl`) is accepted since flat is the only shape `/terms` ever renders;
// any other value this codebase already gives a documented meaning to
// (`map`/`arrarr`/`arrmap`) is a 400, not a silently-flat 200.

#[tokio::test]
async fn terms_json_nl_flat_is_accepted() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&json.nl=flat").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "json.nl=flat is the shape this handler actually renders, so it must \
         be accepted, got {body}"
    );
    assert!(body.get("terms").is_some(), "got {body}");
}

/// `json.nl=map` renders each field's (term, count) pairs as an object —
/// finding 142 / `terms_prefix_json_nl_map`. The `/terms` response is a Solr
/// NamedList exactly as facets are, so it honours `json.nl` through the same
/// `render_named_list` machinery (`src/facet.rs`); the old guard that 400d this
/// was a placeholder "until the named-list machinery landed" (issue #153), and
/// #308's fixture is what retires it.
#[tokio::test]
async fn terms_json_nl_map_renders_an_object() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&json.nl=map").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "json.nl=map must render, not 400 -- finding 142 settled this, got {body}"
    );
    // The outer `terms` object stays keyed by field name under every json.nl;
    // only the inner (term, count) list reshapes.
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!({
            "dog": 2, "lazi": 2, "quick": 2, "about": 1, "afternoon": 1,
            "archiv": 1, "brown": 1, "cat": 1, "day": 1, "document": 1,
        })),
        "json.nl=map must render the term/count pairs as an object keyed by \
         term, got {body}"
    );
}

/// `json.nl=arrarr` renders each pair as a two-element `[term, count]`
/// array — the same NamedList shape facets already render through
/// `render_named_list`. No `terms` fixture pins `arrarr` specifically (only
/// `map` is captured), but the shared machinery renders it identically to
/// facets, so this holds the handler to that rather than re-asserting the old
/// 400 placeholder.
#[tokio::test]
async fn terms_json_nl_arrarr_renders_nested_arrays() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&json.nl=arrarr").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!([
            ["dog", 2],
            ["lazi", 2],
            ["quick", 2],
            ["about", 1],
            ["afternoon", 1],
            ["archiv", 1],
            ["brown", 1],
            ["cat", 1],
            ["day", 1],
            ["document", 1],
        ])),
        "json.nl=arrarr must render each pair as a [term, count] array, got {body}"
    );
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

// --- terms.prefix (issue #308, finding 141) -------------------------------
//
// `terms.prefix` filters each field's term dictionary LITERALLY before the
// count-descending sort: no analyzer runs over the prefix, and the match is
// against the indexed (already-analyzed) term, so it is case-sensitive
// (`str::starts_with` on the raw term). An absent or empty prefix means no
// filter. The prefix is a single global param applied independently to every
// `terms.fl`.
//
// These assert behaviour on corpora whose analyzed terms are already pinned by
// the trace-shaped tests above; they do NOT assert against
// `solr-ref/responses/` values that depend on Solr's `text_en` stemming
// (`dai` vs Tantivy's `day`, finding 103 / issue #205). The differential
// harness (`tests/differential.rs`) compares against the captured fixtures;
// the `terms_*` rows there are the compatibility evidence.

/// Several matches, count-descending then term-ascending. `prefix=d` on the
/// trace's `title` keeps `dog`(2), `day`(1), `document`(1) -- `dog` first by
/// count, then `day`/`document` tie-broken alphabetically. Filtering happens
/// before the sort, so the count ordering of the survivors is unchanged.
#[tokio::test]
async fn terms_prefix_filters_before_the_count_sort() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.prefix=d").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["dog", 2, "day", 1, "document", 1])),
        "terms.prefix=d must keep only the d-prefixed terms, ordered \
         count-desc then term-asc, got {body}"
    );
}

/// A single match.
#[tokio::test]
async fn terms_prefix_single_match() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.prefix=da").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["day", 1])),
        "terms.prefix=da matches only `day` (`document` starts with `do`), \
         got {body}"
    );
}

/// No match is not an error: HTTP 200 with an empty list (finding 141,
/// `terms_prefix_body_none`).
#[tokio::test]
async fn terms_prefix_no_match_returns_empty_with_200() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.prefix=zzz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a prefix matching nothing is not an error, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!([])),
        "a prefix matching no term must yield an empty list, got {body}"
    );
}

/// A count tie breaks term-ascending: `prefix=a` keeps `about`/`afternoon`/
/// `archiv`, all at 1, in alphabetical order (finding 141, `terms_prefix_tie`).
#[tokio::test]
async fn terms_prefix_count_tie_breaks_term_ascending() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.prefix=a").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["about", 1, "afternoon", 1, "archiv", 1])),
        "terms.prefix=a keeps the three a-terms tied at count 1, in \
         term-ascending order, got {body}"
    );
}

/// The prefix is matched against the indexed term, case-sensitive: `D` matches
/// nothing even though `d` matches three terms (finding 141,
/// `terms_prefix_case`) -- the component reads the dictionary, it does not run
/// the field's analyzer over the prefix.
#[tokio::test]
async fn terms_prefix_is_case_sensitive() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.prefix=D").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!([])),
        "terms.prefix=D (uppercase) must match none of the lowercase-indexed \
         terms -- the prefix is not analyzed, got {body}"
    );
}

/// An empty `terms.prefix=` means no filter at all (finding 141,
/// `terms_prefix_empty`): the default `terms.limit=10` still applies, so this
/// is exactly the no-prefix list.
#[tokio::test]
async fn terms_prefix_empty_means_no_filter() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.prefix=").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!([
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
        ])),
        "terms.prefix= (empty) must be equivalent to no prefix: the full \
         default-limit-10 list, got {body}"
    );
}

/// `terms.prefix` works on a `string` field's raw dictionary too: `id` values
/// are unanalyzed, so the prefix matches the literal stored value (finding 141,
/// `terms_prefix_string_field`).
fn string_id_corpus() -> Value {
    json!([
        {"id": "apple", "body": "x"},
        {"id": "apricot", "body": "x"},
        {"id": "banana", "body": "x"},
        {"id": "cherry", "body": "x"},
    ])
}

#[tokio::test]
async fn terms_prefix_on_a_string_field() {
    let (app, _dir) = terms_app(&string_id_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=id&terms.prefix=ap").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/id"),
        Some(&json!(["apple", 1, "apricot", 1])),
        "terms.prefix on a string field filters the literal stored values, \
         got {body}"
    );
}

/// One prefix, applied independently per `terms.fl` (finding 141,
/// `terms_prefix_two_fields`). `prefix=c` on title+body keeps each field's own
/// `cat`; `prefix=qu` matches title's `quick` but nothing in body, so body's
/// list is empty while title's is not.
#[tokio::test]
async fn terms_prefix_is_applied_per_field_independently() {
    let (app, _dir) = terms_app(&multi_field_corpus()).await;

    let (status, body) = get(
        &app,
        "terms?terms=true&terms.fl=title&terms.fl=body&terms.prefix=c",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["cat", 1])),
        "title's c-prefixed term only, got {body}"
    );
    assert_eq!(
        body.pointer("/terms/body"),
        Some(&json!(["cat", 1])),
        "body's c-prefixed term only -- the same prefix applied to a different \
         field's dictionary, got {body}"
    );

    let (status, body) = get(
        &app,
        "terms?terms=true&terms.fl=title&terms.fl=body&terms.prefix=qu",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["quick", 2])),
        "got {body}"
    );
    assert_eq!(
        body.pointer("/terms/body"),
        Some(&json!([])),
        "body has no qu-prefixed term, so its list is empty while title's is \
         not -- per-field independence, got {body}"
    );
}

// --- terms.limit (issue #308, finding 142) --------------------------------
//
// `terms.limit` truncates each field's list AFTER the count-descending sort,
// defaults to 10 (`TERMS_DEFAULT_LIMIT`) when absent, and a negative value is
// the "unlimited" sentinel. `0` means zero, not "default".

/// `terms.limit` truncates the already-sorted list: `prefix=d&limit=1` keeps
/// only `dog`, the highest-count survivor (finding 142, `terms_limit_below`).
#[tokio::test]
async fn terms_limit_truncates_after_the_sort() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(
        &app,
        "terms?terms=true&terms.fl=title&terms.prefix=d&terms.limit=1",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["dog", 2])),
        "terms.limit=1 keeps only the first (highest-count) term, applied \
         after the sort, got {body}"
    );
}

/// A limit above the match count returns all matches with no padding
/// (finding 142, `terms_limit_above`).
#[tokio::test]
async fn terms_limit_above_match_count_does_not_pad() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(
        &app,
        "terms?terms=true&terms.fl=title&terms.prefix=d&terms.limit=99",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["dog", 2, "day", 1, "document", 1])),
        "terms.limit=99 returns all matches rather than padding, got {body}"
    );
}

/// `terms.limit=0` means zero, not "default" (finding 142, `terms_limit_zero`).
#[tokio::test]
async fn terms_limit_zero_means_zero_not_default() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(
        &app,
        "terms?terms=true&terms.fl=title&terms.prefix=d&terms.limit=0",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!([])),
        "terms.limit=0 must yield an empty list, not the default 10, got {body}"
    );
}

/// A negative `terms.limit` is the "unlimited" sentinel, not a clamp-to-zero:
/// `limit=-1` on the twelve-term corpus returns all twelve where the default
/// of 10 would have dropped `theta`/`zeta` (finding 142, `terms_limit_negative`).
///
/// `-1` is the only negative value captured; per the spec any negative is
/// treated as unlimited, and the handler's comment names that single captured
/// value as the extent of the evidence.
#[tokio::test]
async fn terms_limit_negative_means_unlimited() {
    let (app, _dir) = terms_app(&twelve_term_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.limit=-1").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let flat = flat_terms(&body, "title");
    assert_eq!(
        flat.len(),
        24,
        "terms.limit=-1 must return all 12 (term, count) pairs, not the \
         default 10, got {flat:?}"
    );
    for dropped in ["theta", "zeta"] {
        assert!(
            term_count(flat, dropped).is_some(),
            "terms.limit=-1 must keep {dropped:?}, which the default limit of \
             10 would have dropped, got {flat:?}"
        );
    }
}

/// With no `terms.prefix`, `terms.limit` applies to the whole dictionary:
/// `limit=2` gives the top two of the trace's `title` -- `dog` and `lazi`, the
/// two lowest-sorting of the three count-2 terms (finding 142,
/// `terms_limit_no_prefix`).
#[tokio::test]
async fn terms_limit_without_prefix_truncates_the_whole_dictionary() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(&app, "terms?terms=true&terms.fl=title&terms.limit=2").await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        body.pointer("/terms/title"),
        Some(&json!(["dog", 2, "lazi", 2])),
        "terms.limit=2 with no prefix keeps the top two of the whole \
         dictionary, got {body}"
    );
}

/// `terms.limit=abc` is the one error case in the set: HTTP 400 with an
/// **empty but present** `terms:{}` object alongside `error` (finding 142,
/// `terms_limit_invalid`). Solr has already emitted the component's container
/// when the integer parse fails, so Wayfinder's error envelope must reproduce
/// that sibling rather than a bare error. The *shape* -- status 400,
/// `error.code` 400, `metadata` present, `terms` present and empty -- is what
/// must match; `error.msg` is normalised away by the differential harness
/// (finding 10), so its wording is not pinned here.
#[tokio::test]
async fn terms_limit_invalid_returns_400_with_empty_terms_sibling() {
    let (app, _dir) = terms_app(&trace_corpus()).await;
    let (status, body) = get(
        &app,
        "terms?terms=true&terms.fl=body&terms.limit=abc&omitHeader=true",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-numeric terms.limit must 400, got {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400),
        "got {body}"
    );
    assert!(
        body.pointer("/error/metadata")
            .and_then(Value::as_array)
            .is_some(),
        "the error must carry a metadata array (Solr's error-class shape), \
         got {body}"
    );
    assert_eq!(
        body.get("terms"),
        Some(&json!({})),
        "the 400 must carry an empty but present terms object alongside \
         error -- Solr emits the component's container before the parse fails, \
         got {body}"
    );
}
