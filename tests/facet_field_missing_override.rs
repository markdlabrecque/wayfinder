//! `f.<field>.facet.missing` — per-field override of `facet.missing` (issue
//! #140).
//!
//! `search_api_solr` sends `facet.missing` scoped per field
//! (`f.ss_type.facet.missing=true`), not as the bare global param — Wayfinder
//! implements only the global form today, so the module's request is silently
//! ignored: the missing-values bucket never appears for the field that asked
//! for it. This file pins the per-field override end to end.
//!
//! **This is also the first `f.<field>.*` override of any kind.** Nothing in
//! `src/lib.rs`/`src/facet.rs` parses that shape before this issue (confirmed
//! by reading `SELECT_PARAMS`'s own doc comment, which lists
//! `f.<field>.facet.*` per-field overrides among the params "still absent").
//! `SELECT_PARAMS` is a fixed allowlist of exact names
//! (`check_params`/`src/lib.rs`), so a literal `"f.category.facet.missing"`
//! entry cannot generalise to `f.<any field>.facet.missing` — the
//! `strict_params` test below pins the *outcome* (the param must not 400
//! under `strict_params = true`) without dictating a parsing strategy.
//!
//! Four fixtures captured against a one-off `solr:9` (port 8992, same schema
//! and 5-doc corpus as the reference `content` core), documented in
//! `solr-ref/FINDINGS.md` finding 97, settle
//! the open precedence question: **`f.<field>.facet.missing` always wins over
//! the global `facet.missing`, unconditionally** — not merely when the global
//! is unset. The feature suite reads these fixtures directly; Wayfinder does
//! not implement `f.<field>.facet.*`.
//!
//! The fourth open question — does `f.<field>.` key off the field or the
//! `{!key=...}` response label? — was already settled by issue #138's own
//! capture, not a new one here: it is the **field**
//! (`facet_local_params_key_f_field.json` / `_f_key.json`, captured before
//! this issue existed). `f_field_keys_off_the_field_not_the_local_label` and
//! `f_key_naming_the_local_label_has_no_effect` below are the first tests to
//! actually exercise those two fixtures.
//!
//! Out of scope, per the issue: generalising to `f.<field>.facet.limit` /
//! `.mincount` / `.sort` / `.prefix` and friends. Nothing here asserts
//! anything about them — only `facet.missing`.

// The `dead_code` allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use axum::Router;
use axum::http::StatusCode;
use serde_json::Value;
use tempfile::TempDir;

use common::{assert_matches_fixture, corpus, fixture, get, indexed_app, post_docs, request};

/// `facet_counts.facet_fields.<label>` as the flat alternating array Solr
/// uses, or `None` when the label is absent entirely.
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

// --- 1. the override alone (no global present) ------------------------------

/// `f.category.facet.missing=true` with no global `facet.missing` at all must
/// still add the null bucket — matches
/// `facet_missing_field_override_alone.json`, and byte-identically the
/// existing `facet_missing` fixture's `category` bucket (same corpus, same
/// hit set, same missing-value semantics), so the per-field form is not merely
/// accepted but produces exactly what the global form already produces.
#[tokio::test]
async fn field_override_alone_adds_the_missing_bucket() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(fixture_bucket("facet_missing_field_override_alone", "category").as_slice()),
        "the per-field override alone must add the null bucket; got {body}"
    );
    assert_matches_fixture(body, "facet_missing_field_override_alone");
}

/// Without any `facet.missing`/`f.<field>.facet.missing` at all, the bucket is
/// unchanged from `facet_basic` — a plain regression pin so the override
/// mechanism cannot be "always on".
///
/// **Not a new-behaviour test — green before this issue's implementation as
/// well as after.** Kept anyway, next to the change that could regress it,
/// same convention as issue #138's
/// `bare_facet_field_without_a_prefix_still_matches_facet_basic`.
#[tokio::test]
async fn no_missing_param_at_all_is_unaffected() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_matches_fixture(body, "facet_basic");
}

// --- 2. precedence: the per-field override always wins -----------------------

/// `facet.missing=true` (global) plus `f.category.facet.missing=false` (its
/// override) must drop the null bucket entirely — the override wins even
/// though the global alone would add it. Matches
/// `facet_missing_field_override_wins_over_global_true.json`.
#[tokio::test]
async fn field_override_false_wins_over_a_true_global() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category\
         &facet.missing=true&f.category.facet.missing=false&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(
            fixture_bucket(
                "facet_missing_field_override_wins_over_global_true",
                "category"
            )
            .as_slice()
        ),
        "a false per-field override must beat a true global — no null bucket; got {body}"
    );
    assert_matches_fixture(body, "facet_missing_field_override_wins_over_global_true");
}

/// The reverse: `facet.missing=false` (global) plus
/// `f.category.facet.missing=true` (its override) must ADD the null bucket —
/// the override wins even though the global alone would suppress it. This is
/// the direction the ticket's own premise names (`f.ss_type.facet.missing=true`
/// with no global sent at all is the common case, but a global explicitly set
/// to `false` is the sharper precedence probe). Matches
/// `facet_missing_field_override_wins_over_global_false.json`.
#[tokio::test]
async fn field_override_true_wins_over_a_false_global() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category\
         &facet.missing=false&f.category.facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(
            fixture_bucket(
                "facet_missing_field_override_wins_over_global_false",
                "category"
            )
            .as_slice()
        ),
        "a true per-field override must beat a false global — the null bucket must appear; \
         got {body}"
    );
    assert_matches_fixture(body, "facet_missing_field_override_wins_over_global_false");
}

// --- 3. the global still governs fields with no override ---------------------

/// Two `facet.field` values, only one overridden: `category` gets
/// `f.category.facet.missing=false` against a `facet.missing=true` global, and
/// must lose its null bucket — but `id` (no override at all) must still get
/// one from the global, present at count 0 since every document has an `id`
/// (the null bucket is unconditionally appended once active, per finding 41a
/// — its presence at zero is what proves the global is still live on `id`,
/// not merely absent everywhere). Matches
/// `facet_missing_field_override_mixed_multi_field.json`.
#[tokio::test]
async fn global_still_applies_to_a_field_with_no_override() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id\
         &facet.missing=true&f.category.facet.missing=false&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(
            fixture_bucket("facet_missing_field_override_mixed_multi_field", "category").as_slice()
        ),
        "category's override must still suppress its own null bucket; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "id").as_deref(),
        Some(fixture_bucket("facet_missing_field_override_mixed_multi_field", "id").as_slice()),
        "id has no override, so the true global must still add its null bucket (at count 0, \
         since every doc has an id); got {body}"
    );
    assert_matches_fixture(body, "facet_missing_field_override_mixed_multi_field");
}

// --- 4. an override naming a field that was never requested ------------------

/// `f.body.facet.missing=true` alongside `facet.field=category` only (`body`
/// is never passed to `facet.field`) must have no effect at all — no error,
/// no fabricated `body` bucket, and `category`'s own bucket is unperturbed.
/// Matches `facet_missing_field_override_unrelated_field_no_effect.json`.
///
/// **Passes vacuously today** (no `f.<field>.*` override exists yet, so
/// nothing has an effect) but is a real acceptance criterion once the
/// override lands — an implementation that applied any override it saw to
/// every requested field, rather than scoping it to the named field, would
/// regress this the moment it stopped being a no-op. Kept for that reason,
/// not removed as redundant with the vacuous pass.
#[tokio::test]
async fn override_naming_an_unrequested_field_has_no_effect() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&f.body.facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "body"),
        None,
        "an override naming a field never passed to facet.field must not fabricate a bucket \
         for it; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "category").as_deref(),
        Some(
            fixture_bucket(
                "facet_missing_field_override_unrelated_field_no_effect",
                "category"
            )
            .as_slice()
        ),
        "the requested field's own bucket must be unaffected by an override naming a different, \
         unrequested field; got {body}"
    );
    assert_matches_fixture(
        body,
        "facet_missing_field_override_unrelated_field_no_effect",
    );
}

// --- 5. interaction with #138's {!key=...} local-params label ---------------

/// Issue #138's own capture already settled this (before #140 existed):
/// `f.<field>.facet.missing` keys off the *field* being faceted, never the
/// `{!key=...}` response label. `facet.field={!key=mylabel}category` labels
/// the bucket `mylabel` but the underlying field is still `category`, so
/// `f.category.facet.missing=true` must fire — the null bucket must appear
/// under the *label* `mylabel`, since the key only ever renames the bucket,
/// never what drives which field's counts populate it (`src/facet.rs`'s
/// `split_facet_key` doc comment). Matches
/// `facet_local_params_key_f_field.json`.
#[tokio::test]
async fn f_field_keys_off_the_field_not_the_local_label() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dmylabel%7Dcategory&f.category.facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "mylabel").as_deref(),
        Some(fixture_bucket("facet_local_params_key_f_field", "mylabel").as_slice()),
        "f.category.facet.missing=true must add the null bucket under the response label \
         `mylabel`, since `category` is the field actually being faceted; got {body}"
    );
    assert_eq!(
        facet_bucket(&body, "category"),
        None,
        "the field name must not also appear as a second, separate label; got {body}"
    );
    assert_matches_fixture(body, "facet_local_params_key_f_field");
}

/// The mirror case: `f.mylabel.facet.missing=true` names the *local-params
/// key*, not a real field — `mylabel` resolves to nothing `check_facetable`
/// would accept, and Solr's own behaviour (captured) is to silently do
/// nothing with it, not to 400. Matches `facet_local_params_key_f_key.json`.
///
/// **Passes vacuously today**, same caveat as
/// `override_naming_an_unrequested_field_has_no_effect`: no override exists
/// yet, so anything is a no-op. Real acceptance criterion once the override
/// lands — an implementation that resolved `f.<X>` against the response
/// *label* rather than the field would flip this from a no-op to a wrongly
/// added null bucket under `mylabel`.
#[tokio::test]
async fn f_key_naming_the_local_label_has_no_effect() {
    let (app, _dir) = indexed_app().await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true\
         &facet.field=%7B%21key%3Dmylabel%7Dcategory&f.mylabel.facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        facet_bucket(&body, "mylabel").as_deref(),
        Some(fixture_bucket("facet_local_params_key_f_key", "mylabel").as_slice()),
        "an override naming the local-params key rather than the real field must have no \
         effect — no null bucket; got {body}"
    );
    assert_matches_fixture(body, "facet_local_params_key_f_key");
}

// --- 6. strict_params: a pattern, not a literal entry ------------------------

/// `SELECT_PARAMS` is a fixed allowlist of exact names (`check_params` does an
/// exact `contains` scan) — `f.category.facet.missing` cannot be added as a
/// literal entry and still generalise to `f.<any field>.facet.missing`, so
/// whatever satisfies this test must recognise the `f.<field>.facet.missing`
/// *shape*, not one specific field name. Deliberately field-name-agnostic: two
/// different field names are asserted, and neither is hardcoded into
/// `SELECT_PARAMS` today (confirmed by reading it) so an implementation that
/// added a single literal entry for `category` would still fail this test on
/// `id`.
#[tokio::test]
async fn strict_params_accepts_the_per_field_missing_override_for_any_field() {
    let (app, _dir) = indexed_app_with_config("strict_params = true\n").await;

    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.missing=true&wt=json",
    )
    .await;
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !msg.contains("unknown request parameter"),
        "f.category.facet.missing must not 400 as an unknown param under strict_params, got: {msg}"
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "f.category.facet.missing must pass strict mode, got {body}"
    );

    let (status2, body2) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=id&f.id.facet.missing=true&wt=json",
    )
    .await;
    let msg2 = body2
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !msg2.contains("unknown request parameter"),
        "f.id.facet.missing must not 400 as an unknown param under strict_params either — a \
         literal `f.category.facet.missing` allowlist entry would still fail this one; got: {msg2}"
    );
    assert_eq!(
        status2,
        StatusCode::OK,
        "f.id.facet.missing must pass strict mode, got {body2}"
    );
}

/// A genuinely unknown param (not the `f.<field>.facet.missing` shape at all)
/// must still 400 under `strict_params = true` — the pattern-matching
/// strategy this issue needs must not widen into "any dotted param starting
/// with `f.` is accepted", which would silently defeat `strict_params` for
/// every param Wayfinder does not actually implement.
///
/// `f.<field>.facet.prefix`, not `.limit`/`.mincount`/`.sort`: issue #296
/// (`tests/facet_perfield_settings.rs`) implements those three and adds them
/// to `PER_FIELD_PARAMS`, so this guard would otherwise flip from a 400 to a
/// 200 the moment #296 landed, even though the *shape-vs-endpoint* rule it
/// pins is still true. `.prefix` stays outside `PER_FIELD_PARAMS` on both
/// sides of #296 (per the ticket's own scope note on
/// `search_api_solr`'s `f.<field>.facet.range.*`), so it keeps testing a
/// genuinely unimplemented per-field param rather than expiring.
#[tokio::test]
async fn strict_params_still_rejects_an_unrelated_f_dot_param() {
    let (app, _dir) = indexed_app_with_config("strict_params = true\n").await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.prefix=abc&wt=json",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "f.<field>.facet.prefix is not part of this issue's scope and must still 400 under \
         strict_params, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("f.category.facet.prefix"),
        "error.msg must name the unknown param, got: {msg}"
    );
}

// --- 7. the shape is scoped to endpoints that honour the base param ----------

/// `check_params` accepts a per-field key only when its base param is in
/// *both* `PER_FIELD_PARAMS` and the endpoint's own allowlist. Dropping the
/// second half — accepting any key whose shape matches, regardless of endpoint
/// — leaks `f.<field>.facet.missing` into `/update`, `/mlt` and `/terms`, none
/// of which allow `facet.missing` at all (`UPDATE_PARAMS`, `MLT_PARAMS`,
/// `TERMS_PARAMS` in `src/lib.rs`). That leak is exactly the silent-wrong-
/// answer failure `PER_FIELD_PARAMS`' doc comment argues against, and until
/// this test it was asserted only in prose.
///
/// `/update` is checked first because it is the one that takes a body, so a
/// leak there means an indexing request carrying a nonsense param is accepted
/// and the param silently dropped.
#[tokio::test]
async fn strict_params_rejects_the_per_field_override_on_update() {
    let (app, _dir) = indexed_app_with_config("strict_params = true\n").await;
    let (status, body) = request(
        &app,
        "POST",
        "update?commit=true&f.category.facet.missing=true",
        Some("[]"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "/update does not honour `facet.missing`, so its per-field form must 400 \
         under strict_params rather than being accepted and ignored, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("f.category.facet.missing"),
        "error.msg must name the rejected param, got: {msg}"
    );
}

/// Same scoping, on the two other endpoints whose allowlists omit
/// `facet.missing`. Cheap to cover and it pins that the rule is a property of
/// `check_params`, not a one-off at `/update`.
#[tokio::test]
async fn strict_params_rejects_the_per_field_override_on_mlt_and_terms() {
    let (app, _dir) = indexed_app_with_config("strict_params = true\n").await;

    let (mlt_status, mlt_body) = get(
        &app,
        "mlt?q=id:doc1&mlt.fl=body&f.category.facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(
        mlt_status,
        StatusCode::BAD_REQUEST,
        "/mlt does not honour `facet.missing`, so its per-field form must 400 \
         under strict_params, got {mlt_body}"
    );
    let mlt_msg = mlt_body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        mlt_msg.contains("f.category.facet.missing"),
        "/mlt error.msg must name the rejected param, got: {mlt_msg}"
    );

    let (terms_status, terms_body) = get(
        &app,
        "terms?terms=true&terms.fl=body&f.category.facet.missing=true&wt=json",
    )
    .await;
    assert_eq!(
        terms_status,
        StatusCode::BAD_REQUEST,
        "/terms does not honour `facet.missing`, so its per-field form must 400 \
         under strict_params, got {terms_body}"
    );
    let terms_msg = terms_body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        terms_msg.contains("f.category.facet.missing"),
        "/terms error.msg must name the rejected param, got: {terms_msg}"
    );
}

/// A dotted field name end to end, through the real strict-mode
/// `check_params`. `src/schema.rs`'s dynamic patterns produce field names that
/// contain dots (issue #180), so anchoring the split on the first `.` instead
/// of the honoured suffix would truncate `ss_field.name` to `ss_field`, leave
/// a base param of `name.facet.missing` that is in no allowlist, and 400 a
/// param Wayfinder supports. `f.a.b.facet.missing` is the minimal shape of
/// that: no such field needs to exist for the *param check* to be the thing
/// under test, and an unknown field name is not itself a strict-mode error
/// (`override_naming_an_unrequested_field_has_no_effect` above relies on the
/// same).
#[tokio::test]
async fn strict_params_accepts_the_override_for_a_dotted_field_name() {
    let (app, _dir) = indexed_app_with_config("strict_params = true\n").await;
    let (status, body) = get(
        &app,
        "select?q=*:*&rows=0&facet=true&facet.field=category&f.a.b.facet.missing=true&wt=json",
    )
    .await;
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !msg.contains("unknown request parameter"),
        "a dotted field name must still match the per-field shape — splitting on \
         the first `.` would truncate it and 400 a supported param; got: {msg}"
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "f.a.b.facet.missing must pass strict mode, got {body}"
    );
}
