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
//!    One divergence *was* found and is a real finding, not hidden: Tantivy's
//!    `StopWordFilter(Language::English)` does **not** remove `over` (present
//!    in "the quick brown fox jumps **over** the lazy dog"), where Lucene's
//!    classic English stopword list (Solr's own `text_en` default) does. This
//!    file's corpora avoid `over` and any other word whose stopword-list
//!    membership is uncertain, so no assertion here depends on that
//!    divergence — but it is a real Tantivy-vs-Solr gap worth a follow-up on
//!    the `solr-ref/manifest.tsv` capture the ticket defers (its "Not in this
//!    ticket" section already flags an analyzer-difference risk for exactly
//!    this reason).
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
/// only ever populates `title`) needing to grow extra vocabulary.
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
