//! `facet.field={!key=label}field` — the local-params key prefix (issue #138).
//!
//! `search_api_solr` always sends `facet.field` with a `{!key=X}` prefix. In
//! every captured module request `X` is identical to the field name, so the
//! module traces alone cannot distinguish "uses the key as the response label"
//! from "strips the prefix and uses the field name". Five fixtures captured
//! against a one-off `solr:9` (commit `b4cf4b0`, documented in the block at the
//! end of `solr-ref/capture.sh`) settle it: **the key is the response label.**
//!
//! Three further shapes were captured the same way in commit `1a7e47d`, so
//! nothing in this file rests on generalising the module's own shape:
//! `_as_other_field` (a key naming a *different* declared field),
//! `_unterminated`, and `_empty_remainder`.
//!
//! Expected values here come from those fixtures and from the pre-existing
//! `facet_basic` / `facet_multi_field` captures, never from what Wayfinder
//! happens to produce. Six of the eight have `solr-ref/manifest.tsv` rows, so
//! the differential harness replays them too — but the differential's error
//! tolerance already lets `facet_local_params_key_unknown` "match" while
//! Wayfinder names the *raw value* in the message rather than the remainder, so
//! the message contract is pinned here directly (see
//! `unknown_field_behind_a_prefix_400s_naming_the_remainder` and
//! `a_prefix_with_an_empty_remainder_400s_on_the_empty_field_name`).
//!
//! Deliberately out of scope, per the issue: `f.<field>.facet.*` per-field
//! overrides (#140 — the `facet_local_params_key_f_field` / `_f_key` fixtures
//! are its evidence and carry no manifest row), local-params prefixes on
//! `facet.query` / `facet.pivot` / `fq`, and inline facet params inside the
//! block (`{!key=x facet.mincount=2}`). Nothing here asserts anything about
//! them.
//!
//! Two tests here are *not* new-behaviour tests, and say so at the test:
//! `bare_facet_field_without_a_prefix_still_matches_facet_basic` and
//! `an_unterminated_block_is_a_400_like_the_fixture` are pins that pass before
//! the implementation as well as after — the first because the un-prefixed path
//! already works, the second because Wayfinder already 400s on that input (by a
//! different route than Solr, which the test's comment spells out). Everything
//! else must go from red to green.

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::Value;
use tempfile::TempDir;

use common::{assert_matches_fixture, corpus, fixture, get, indexed_app, post_docs};

/// `facet_counts.facet_fields.<label>` as the flat alternating array Solr uses,
/// or `None` when the label is absent entirely.
fn facet_bucket(body: &Value, label: &str) -> Option<Vec<Value>> {
    body.pointer(&format!("/facet_counts/facet_fields/{label}"))
        .map(|v| {
            v.as_array()
                .unwrap_or_else(|| {
                    panic!("facet_counts.facet_fields.{label} must be a flat array, got: {body}")
                })
                .clone()
        })
}

/// The flat counts array a fixture recorded under `label`.
fn fixture_bucket(fixture_name: &str, label: &str) -> Vec<Value> {
    facet_bucket(&fixture(fixture_name), label).unwrap_or_else(|| {
        panic!("fixture {fixture_name} has no facet_counts.facet_fields.{label}")
    })
}

/// Asserts the narrow error contract `tests/error_shapes.rs` establishes (code
/// and `responseHeader.status` mirror the HTTP status, `/select` errors echo
/// params, `error.msg` is free text and never compared verbatim), plus that no
/// facet block was emitted alongside the error.
fn assert_facet_error_envelope(status: StatusCode, body: &Value) -> String {
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected a 400, got {status} / {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_i64),
        Some(400),
        "error.code must mirror the HTTP status (finding 10), got {body}"
    );
    assert_eq!(
        body.pointer("/responseHeader/status")
            .and_then(Value::as_i64),
        Some(400),
        "responseHeader.status must mirror the HTTP status (finding 10), got {body}"
    );
    assert!(
        body.pointer("/responseHeader/params").is_some(),
        "/select errors echo params (finding 13), got {body}"
    );
    assert!(
        body.get("facet_counts").is_none(),
        "an errored facet request must not also emit facet_counts, got {body}"
    );
    body.pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("error.msg must be present, got {body}"))
        .to_string()
}

/// An app on the tracer-bullet schema/corpus but with an arbitrary server
/// config, for the `strict_params` guard. `common::indexed_app` always uses
/// `ServerConfig` defaults.
async fn indexed_app_with_config(config_toml: &str) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, config_toml).expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, &corpus()).await;
    assert_eq!(status, StatusCode::OK, "indexing must succeed, got {body}");
    (app, dir)
}

// --- 1. the key labels the bucket ------------------------------------------

/// `solr-ref/manifest.tsv` row `facet_local_params_key`, replayed verbatim: the
/// counts are `category`'s, and the label is `mylabel`.
#[tokio::test]
async fn differing_key_labels_the_bucket_matching_the_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    // Spelled out as well as diffed, because these two facts are the issue's
    // whole acceptance criterion and a whole-envelope diff reports them as one
    // opaque mismatch.
    assert_eq!(
        facet_bucket(&body, "mylabel").as_deref(),
        Some(fixture_bucket("facet_local_params_key", "mylabel").as_slice()),
        "the local key must be the response label, carrying `category`'s counts; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "category"),
        None,
        "the faceted field name must not appear as a label when a differing key was given; \
         got {body}"
    );

    assert_matches_fixture(body, "facet_local_params_key");
}

// --- 2. the module's own shape: key == field name --------------------------

/// `solr-ref/manifest.tsv` row `facet_local_params_key_same`. This is what
/// every captured `search_api_solr` request looks like, and it is a visual
/// no-op against the un-prefixed form — which is exactly why it cannot be the
/// only test.
#[tokio::test]
async fn key_equal_to_the_field_name_matches_the_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcategory%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body, "facet_local_params_key_same");
}

// --- 3. regression pin: the un-prefixed form is untouched ------------------

/// A pin, not new behaviour: this is the path that already works, and the point
/// is that adding prefix handling does not perturb it. Expected to be green
/// before and after the implementation — `tests/faceting.rs` has the same
/// fixture comparison, and this copy exists so a regression shows up in *this*
/// suite, next to the change that could cause it.
#[tokio::test]
async fn bare_facet_field_without_a_prefix_still_matches_facet_basic() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body, "facet_basic");
}

/// The prefixed and un-prefixed forms must count the *same* terms; only the
/// label differs. Cross-checked between two fixtures rather than within one, so
/// a labelling implementation that also perturbed the counts is caught.
#[tokio::test]
async fn a_prefix_changes_only_the_label_not_the_counts() {
    let (app, _dir) = indexed_app().await;
    let (bare_status, bare) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;
    let (keyed_status, keyed) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(bare_status, StatusCode::OK, "got {bare}");
    assert_eq!(keyed_status, StatusCode::OK, "got {keyed}");
    assert_eq!(
        facet_bucket(&keyed, "mylabel"),
        facet_bucket(&bare, "category"),
        "the keyed form must produce the same counts as the bare form under a different label"
    );
    // And both equal the captured values, so this is not two identical wrongs.
    assert_eq!(
        facet_bucket(&bare, "category").as_deref(),
        Some(fixture_bucket("facet_basic", "category").as_slice()),
    );
}

// --- 4. unknown field behind a prefix -------------------------------------

/// `solr-ref/manifest.tsv` row `facet_local_params_key_unknown`: real Solr 400s
/// with `msg` `undefined field: "nosuchfield"` — the **remainder** is named,
/// not the key (`k`) and not the raw value (`{!key=k}nosuchfield`).
///
/// Two notes on why the assertions are shaped this way:
///
/// - `error.msg` is free text under `tests/error_shapes.rs`'s contract, so the
///   wording is not frozen; what is frozen is *which token* it names, which is
///   the observable part of Solr's behaviour here.
/// - the differential harness cannot cover this: it already reports 0 diffs for
///   this row, because its error tolerance ignores `error.msg` while Wayfinder
///   names the whole raw value. Without the "does not leak the block" half
///   below, this test would pass on a substring match against the unparsed
///   value.
///
/// Also worth recording: captured Solr 400s here even though an un-prefixed
/// unknown `facet.field` is a 200 with an empty array
/// (`facet_unknown_field.json`, `tests/faceting.rs` section on unfacetable
/// fields). Wayfinder already 400s in both cases, so the prefixed case moves it
/// *towards* Solr, and the pre-existing divergence is unchanged by this issue.
#[tokio::test]
async fn unknown_field_behind_a_prefix_400s_naming_the_remainder() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dk%7Dnosuchfield&wt=json",
    )
    .await;

    let want_code = fixture("facet_local_params_key_unknown")["error"]["code"].as_i64();
    assert_eq!(want_code, Some(400), "fixture sanity: it records a 400");

    let msg = assert_facet_error_envelope(status, &body);
    assert!(
        msg.contains("nosuchfield"),
        "error.msg must name the remainder `nosuchfield`, as the fixture's \
         `undefined field: \\\"nosuchfield\\\"` does; got: {msg}"
    );
    assert!(
        msg.contains("undefined field"),
        "error.msg must be Wayfinder's own undefined-field refusal, not an incidental 400 from \
         somewhere else; got: {msg}"
    );
    // The leak guard: naming the raw value would satisfy the substring check
    // above while proving nothing was parsed.
    assert!(
        !msg.contains("{!"),
        "error.msg must name the parsed remainder, not the raw local-params value; got: {msg}"
    );
    assert!(
        !msg.contains("key="),
        "error.msg must not echo the local-params block; got: {msg}"
    );
}

// --- 5. the label is the key, not the field -------------------------------

/// The acceptance criterion the *module's* own requests cannot prove — they
/// always send a key identical to the field name — made impossible to satisfy
/// by stripping the prefix and using the field name.
///
/// Proved by `solr-ref/responses/facet_local_params_key_as_other_field.json`
/// (`solr-ref/manifest.tsv` row `facet_local_params_key_as_other_field`, so the
/// differential harness replays it too): `{!key=body}category` is a 200 whose
/// single bucket is labelled `body` and carries `category`'s counts.
///
/// `body` is the decisive choice of key because it is a real field in the
/// tracer-bullet schema and is *not* `fast`, so it can never be faceted here
/// (`tests/faceting.rs`'s `Refusal::NotFast`). Three wrong implementations all
/// fail against this fixture: using the field name labels the bucket
/// `category`; re-resolving the label as a field to facet 400s on `body`; and
/// counting `body` instead of `category` produces different terms.
#[tokio::test]
async fn the_label_is_the_key_even_when_it_names_a_different_real_field() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dbody%7Dcategory&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the key is a label, never a field to resolve — `body` must not be looked up at all; \
         got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "body").as_deref(),
        Some(fixture_bucket("facet_local_params_key_as_other_field", "body").as_slice()),
        "the bucket labelled with the key must carry the *faceted field*'s captured counts; \
         got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "category"),
        None,
        "the field name must not also appear as a label; got {body}"
    );
    assert_matches_fixture(body, "facet_local_params_key_as_other_field");
}

// --- 6. strict_params ------------------------------------------------------

/// `facet.field` is already in `SELECT_PARAMS`, so the name is allowed; this
/// pins that the *value* form is not what strict mode inspects, because a
/// param-name allowlist that also validated values would 400 every request
/// `search_api_solr` sends.
#[tokio::test]
async fn strict_params_accepts_the_local_params_form_of_facet_field() {
    let (app, _dir) = indexed_app_with_config("strict_params = true\n").await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7Dcategory&wt=json",
    )
    .await;
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !msg.contains("unknown request parameter"),
        "the local-params value form must not make an allowed param look unknown, got: {msg}"
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "the local-params form of an allowed param must pass strict mode, got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "mylabel").as_deref(),
        Some(fixture_bucket("facet_local_params_key", "mylabel").as_slice()),
        "strict mode must not change the labelling either; got {body}"
    );
}

// --- 7. several values, only some prefixed --------------------------------

/// The module sends several `facet.field` values per request, and each is
/// parsed independently. Expected counts composed from two captures:
/// `facet_basic` for `category` and `facet_multi_field` for `id`, both taken
/// against this same 5-doc corpus with the same `q=*:*` hit set.
#[tokio::test]
async fn mixed_prefixed_and_bare_values_each_get_their_own_label() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dmylabel%7Dcategory&facet.field=id&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "mylabel").as_deref(),
        Some(fixture_bucket("facet_basic", "category").as_slice()),
        "the prefixed value keeps its key as the label; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "id").as_deref(),
        Some(fixture_bucket("facet_multi_field", "id").as_slice()),
        "the bare value alongside it is unaffected; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "category"),
        None,
        "the prefixed value's field name must not leak in as a third label; got {body}"
    );
}

// --- 8. malformed and degenerate blocks -----------------------------------
//
// Both shapes below are now captured (`solr-ref/manifest.tsv` rows
// `facet_local_params_key_unterminated` and
// `facet_local_params_key_empty_remainder`), so the expectations are ground
// truth rather than self-consistency. Both are 400s in Solr and 400s in
// Wayfinder today; `error.msg` is free text under `tests/error_shapes.rs`'s
// contract and the differential harness tolerates it, so what each test can
// pin beyond the status is called out individually.

/// **Status pin, green before the implementation as well as after.**
/// `solr-ref/responses/facet_local_params_key_unterminated.json`:
/// `{!key=mylabel category` (no closing brace) is a 400 in Solr, with root
/// error class `org.apache.solr.search.SyntaxError` and msg
/// `org.apache.solr.search.SyntaxError: Expected identifier at pos 22
/// str='{!key=mylabel category'` — so Solr does *not* take the value verbatim
/// as a field name, it fails parsing the block.
///
/// Wayfinder reaches the same 400 by the other route: `parse_block` treats an
/// unterminated `{!...` as "not a local-params block at all", so the whole value
/// stays a field name and no such field exists. The wording therefore diverges,
/// which is tolerated — `error.msg` is never compared verbatim
/// (`tests/error_shapes.rs`'s contract) and the differential harness applies the
/// same tolerance, so the fixture pins the status and the no-facet-block shape
/// only. Nothing here can distinguish the two routes, and the issue does not ask
/// it to.
///
/// So what this test pins is exactly two things: the fixture's 400 status, and
/// that no bucket is fabricated under the would-be key `mylabel`. It does *not*
/// discriminate a `split('}')`-style brace-scanning prefix strip from real block
/// parsing — this value contains no `}` at all, so both return it byte-for-byte
/// unchanged and both 400 by the undefined-field route. The cases that do
/// discriminate need a `}` outside a well-formed block, and live as unit tests
/// beside `split_facet_key` in `src/facet.rs`'s `mod tests`
/// (`a_value_that_is_not_a_block_is_its_own_label_and_field` for `cat}egory`,
/// `a_quoted_brace_inside_the_block_does_not_end_it` for `{!key='a} b'}category`).
#[tokio::test]
async fn an_unterminated_block_is_a_400_like_the_fixture() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%20category&wt=json",
    )
    .await;
    let want_code = fixture("facet_local_params_key_unterminated")["error"]["code"].as_i64();
    assert_eq!(want_code, Some(400), "fixture sanity: it records a 400");
    assert_facet_error_envelope(status, &body);
    assert_eq!(
        facet_bucket(&body, "mylabel"),
        None,
        "an unterminated block must not produce a bucket labelled with its would-be key; \
         got {body}"
    );
}

/// `solr-ref/responses/facet_local_params_key_empty_remainder.json`:
/// `{!key=mylabel}` with nothing after the block is a 400 whose msg is
/// `undefined field: ""` — the **empty remainder** is what gets validated, not
/// the raw value and not the key. Same message contract as
/// `unknown_field_behind_a_prefix_400s_naming_the_remainder`: the wording is
/// free, but naming the unparsed value instead of the remainder is a divergence
/// the harness's `error.msg` tolerance would hide.
#[tokio::test]
async fn a_prefix_with_an_empty_remainder_400s_on_the_empty_field_name() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7D&wt=json",
    )
    .await;
    let want_code = fixture("facet_local_params_key_empty_remainder")["error"]["code"].as_i64();
    assert_eq!(want_code, Some(400), "fixture sanity: it records a 400");

    let msg = assert_facet_error_envelope(status, &body);
    assert!(
        msg.contains("undefined field"),
        "error.msg must be the undefined-field refusal the fixture records, got: {msg}"
    );
    assert!(
        !msg.contains("{!"),
        "the fixture validates the parsed (empty) remainder — `undefined field: \\\"\\\"` — so \
         the message must not name the unparsed value instead; got: {msg}"
    );
    assert_eq!(
        facet_bucket(&body, "mylabel"),
        None,
        "an empty remainder must not fabricate a bucket labelled with the key; got {body}"
    );
}
