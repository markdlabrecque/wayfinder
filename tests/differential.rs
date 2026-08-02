//! Differential harness (issue #1, PRD §8): runs the query set in
//! `solr-ref/manifest.tsv` and diffs the response against a known-good
//! side, failing on any difference outside the explicit, logged normaliser
//! in `tests/common/diff.rs`.
//!
//! Two modes:
//! - **Hermetic (default, plain `cargo test`):** every manifest entry
//!   against an in-process Wayfinder (`common::indexed_app`), diffed against
//!   the committed fixture in `solr-ref/responses/`. No network, no Docker.
//! - **Live (`WAYFINDER_DIFF_SOLR=1 cargo test --test differential`):** same
//!   query set, same differ, expected side comes from a live Solr over HTTP.
//!   Requires `solr-ref/capture.sh` to have been run first (leaves the
//!   container up with schema + corpus already loaded) — this harness does
//!   not reimplement docker orchestration. Base URL from
//!   `WAYFINDER_DIFF_SOLR_URL`, default `http://localhost:8983/solr/content`.
//!   Gated by the env var alone (not `#[ignore]` as well), so it stays a
//!   plain `#[test]` that no-ops under default `cargo test`.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use common::diff::{
    Diff, ManifestEntry, ManifestErrorEntry, ManifestMultipartEntry, RankedDoc, diff,
    diff_ranked_ids, fetch_live_full, fetch_live_multipart, fetch_live_status, live_reachable,
    load_manifest, load_manifest_errors, load_manifest_multipart, normalize, normalize_extract,
    ranked_docs, score_tolerance,
};
use common::key_order::fixture_text;
use common::{fixture, get, indexed_app, post_docs, request_full};
use serde_json::{Value, json};
use tempfile::TempDir;

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest.tsv")
}

fn manifest_errors_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest-errors.tsv")
}

fn manifest_multipart_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/manifest-multipart.tsv")
}

fn extract_inputs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/extract-inputs")
}

/// `/update/extract` (issue #258) needs no particular field schema — it
/// never indexes anything under `extractOnly=true` — so this reuses
/// `common::SCHEMA_TOML` directly rather than defining a bespoke one like
/// `facets_app`/`keyorder_app` do for their own manifest rows.
async fn extract_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), common::SCHEMA_TOML).expect("app must build");
    (app, dir)
}

/// Permanent, ratified `/update/extract` divergences (issue #258 spec) — the
/// `manifest-multipart.tsv` counterpart of `ACCEPTED_DIVERGENCES` above.
/// Named distinctly (rather than folded into `ACCEPTED_DIVERGENCES`) because
/// that list's own lookup is checked against `manifest-errors.tsv` row names
/// elsewhere in this file; reusing it verbatim for multipart rows would
/// conflict with that scoping.
///
/// Every success row gets an `X-Parsed-By` entry (`file_metadata`, every
/// capture); `extract_html_only_xml` additionally gets `shape="rect"` and an
/// `X-Parsed-By` `<meta>` element inside its XHTML `file` string, and
/// `extract_plain_text_xml` gets the `<meta>` element too (both wrap their
/// text in Tika's XHTML envelope). The plain-text `extractFormat=text` rows
/// have no `<meta>` markup in `file` at all — a plain-text body, not XHTML —
/// so they only need the `file_metadata` entry.
const ACCEPTED_DIVERGENCES_MULTIPART: &[(&str, &str)] = &[
    (
        "extract_plain_text_xml",
        "issue #258: X-Parsed-By names Java Tika/parser class names Wayfinder has no honest \
         equivalent for, in both the XHTML file's meta element and file_metadata",
    ),
    (
        "extract_plain_text_text",
        "issue #258: X-Parsed-By in file_metadata only (extractFormat=text has no XHTML meta \
         markup in file to strip)",
    ),
    (
        "extract_html_only_xml",
        "issue #258: X-Parsed-By in both the XHTML file's meta element and file_metadata, plus \
         shape=\"rect\" that Tika's own HTML parser injects onto every <a> element",
    ),
    (
        "extract_html_only_text",
        "issue #258: X-Parsed-By in file_metadata only",
    ),
    (
        "extract_latin1_text",
        "issue #258: X-Parsed-By in file_metadata only",
    ),
    (
        "extract_utf8_bom_text",
        "issue #258: X-Parsed-By in file_metadata only",
    ),
    (
        "extract_declared_charset_text",
        "issue #258: X-Parsed-By in file_metadata only",
    ),
];

/// Rows where Wayfinder's HTTP *status* itself diverges from the captured
/// one, keyed by fixture name to the status Wayfinder answers and why.
///
/// Separate from `ACCEPTED_DIVERGENCES_MULTIPART` because the two are
/// different claims: that list waives body fields the normaliser then strips
/// from an otherwise identical envelope, while a status divergence means the
/// two responses are not the same kind of answer at all and no normaliser
/// can or should reconcile them.
///
/// Self-expiring: the runner asserts the captured status still differs from
/// the listed one, so an entry that stops diverging fails and names itself
/// for deletion.
const DIVERGENT_STATUS_MULTIPART: &[(&str, u16, &str)] = &[(
    "extract_corrupt_pdf",
    415,
    "issue #258: Solr's Tika parses this malformed PDF and throws, which is a 500; \
     Wayfinder has no PDF extractor at all, so it never reaches a parse attempt and \
     answers 415 unsupported format. Retire this entry when the PDF extractor lands \
     (PRD phase 2b) -- at that point Wayfinder can fail *inside* a parser and the \
     captured 500 becomes reachable",
)];

fn divergent_status_multipart(name: &str) -> Option<(u16, &'static str)> {
    DIVERGENT_STATUS_MULTIPART
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, status, reason)| (*status, *reason))
}

fn accepted_divergence_multipart_reason(name: &str) -> Option<&'static str> {
    ACCEPTED_DIVERGENCES_MULTIPART
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

// --- duplicated schema/corpus for manifest-errors.tsv's `facets`/`keyorder`
// rows (same precedent tests/json_key_order.rs documents for its own copies:
// tests/common/ is compiled once per integration-test binary, so sharing
// these across binaries is not straightforward, and this file needs its own
// in-process apps to run those rows hermetically). Every Wayfinder test app
// names its core `content` — the Solr-side core name (`facets`/`keyorder`)
// only ever appears in the manifest-errors row's URL, which the runner below
// rewrites before issuing the request.

const FACETS_SCHEMA_TOML: &str = r#"
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
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[fields]]
name = "note"
type = "string"
stored = true
"#;

fn facets_corpus() -> Value {
    json!([
        {"id":"r1","views":5, "created":"2020-01-02T00:00:00Z","note":"alpha"},
        {"id":"r2","views":15,"created":"2020-01-03T00:00:00Z","note":"beta"},
        {"id":"r3","views":25,"created":"2020-01-03T00:00:00Z","note":"alpha"},
        {"id":"r4","views":35,"created":"2020-01-05T00:00:00Z"}
    ])
}

async fn facets_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FACETS_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &facets_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the facets corpus must succeed, got {body}"
    );
    (app, dir)
}

const KEYORDER_SCHEMA_TOML: &str = r#"
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

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "tag"
type = "string"
stored = true
fast = true
multi_valued = true
"#;

fn keyorder_corpus() -> Value {
    json!([
        {"id":"k1","views":5,  "tag":["zebra","apple"]},
        {"id":"k2","views":15, "tag":["zebra","apple"]},
        {"id":"k3","views":45, "tag":["zebra","mango"]},
        {"id":"k4","views":95, "tag":["zebra","apple"]},
        {"id":"k5","views":105,"tag":["mango","banana"]},
        {"id":"k6","views":155,"tag":["apple"]},
        {"id":"k7","views":195,"tag":["apple"]},
        {"id":"k8","views":125,"tag":["zebra"]}
    ])
}

async fn keyorder_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), KEYORDER_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &keyorder_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the key-order corpus must succeed, got {body}"
    );
    (app, dir)
}

// --- duplicated schema/corpus for manifest-errors.tsv's `sortdebt` rows
// (issue #32, post-rebase). Mirrors `tests/sort.rs::SORTDEBT_SCHEMA_TOML` /
// `sortdebt_doc` exactly, same duplication precedent as `FACETS_SCHEMA_TOML`
// above — `tests/common/` cannot be shared across integration-test binaries.
// Unlike `facets`/`keyorder`, this schema names its core `sortdebt` (not
// `content`) to match the manifest-errors row's own request path verbatim —
// `tests/sort.rs`'s own comment explains why: the captured fixtures and the
// task spec want a schema literally named `sortdebt` at `/solr/sortdebt/...`.
// So `sortdebt/...` rows are NOT rewritten in `app_and_request_url` below,
// unlike `facets/...`/`keyorder/...`.

const SORTDEBT_SCHEMA_TOML: &str = r#"
[core]
name = "sortdebt"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "category"
type = "string"
stored = true
fast = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "weight"
type = "float"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[fields]]
name = "nums"
type = "int"
stored = true
fast = true
multi_valued = true
"#;

/// One doc of the `s1..s6` corpus — identical to `tests/sort.rs::sortdebt_doc`.
fn sortdebt_doc(id: &str) -> Value {
    match id {
        "s1" => json!({
            "id": "s1", "category": "alpha", "views": 30, "weight": 1.5,
            "created": "2021-03-01T00:00:00Z", "nums": [10, 90]
        }),
        "s2" => json!({
            "id": "s2", "category": "beta", "views": 10, "weight": 3.5,
            "created": "2021-01-01T00:00:00Z", "nums": [50, 60]
        }),
        "s3" => json!({
            "id": "s3", "category": "gamma", "views": 20, "weight": 2.5,
            "created": "2021-05-01T00:00:00Z", "nums": [20, 80]
        }),
        "s4" => json!({
            "id": "s4", "category": "delta", "weight": 0.5,
            "created": "2021-02-01T00:00:00Z", "nums": [70]
        }),
        "s5" => json!({"id": "s5", "category": "epsilon", "views": 40}),
        "s6" => json!({
            "id": "s6", "category": "zeta", "views": -5, "weight": -1.5,
            "created": "1969-06-01T00:00:00Z", "nums": [-10, 5]
        }),
        other => panic!("no such sortdebt corpus doc: {other}"),
    }
}

/// `POST /solr/sortdebt/update?commit=true` — cannot be `common::post_docs`,
/// which is hardcoded to `common::CORE` (`"content"`). Mirrors
/// `tests/sort.rs::sortdebt_post_docs`.
async fn sortdebt_post_docs(app: &Router, docs: &Value) -> (StatusCode, Value) {
    common::request_full(
        app,
        "POST",
        "sortdebt/update?commit=true",
        Some(&docs.to_string()),
    )
    .await
}

/// Builds a fresh `sortdebt`-schema app and indexes all six `s1..s6` docs in
/// one commit — the single-segment case every manifest-errors row here needs
/// (the multi-segment tests in `tests/sort.rs` reuse these same fixtures
/// against a differently-segmented index, not a different expected result).
async fn sortdebt_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), SORTDEBT_SCHEMA_TOML).expect("app must build");
    let docs: Value = Value::Array(
        ["s1", "s2", "s3", "s4", "s5", "s6"]
            .iter()
            .map(|id| sortdebt_doc(id))
            .collect(),
    );
    let (status, body) = sortdebt_post_docs(&app, &docs).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the sortdebt corpus must succeed, got {body}"
    );
    (app, dir)
}

// --- duplicated schema/corpus for manifest-errors.tsv's `facets33` rows
// (issue #33, post-rebase). Mirrors `tests/faceting.rs::DEBT_SCHEMA_TOML` /
// `debt_corpus` exactly. Unlike `sortdebt`, this schema names its core
// `content` (same as `facets`/`keyorder` above), so `facets33/...` rows ARE
// rewritten in `app_and_request_url` below.

const FACETS33_SCHEMA_TOML: &str = r#"
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
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "price"
type = "double"
stored = true
fast = true

[[fields]]
name = "rating"
type = "float"
stored = true
fast = true

[[fields]]
name = "stamp"
type = "date"
stored = true
fast = true

[[fields]]
name = "tag"
type = "string"
stored = true
fast = true

[[fields]]
name = "note"
type = "string"
stored = true
"#;

/// The 5-doc corpus `capture.sh`'s issue-33 block indexes into `facets33` —
/// identical to `tests/faceting.rs::debt_corpus`.
fn facets33_corpus() -> Value {
    json!([
        {"id":"r1","views":5, "price":5.0, "rating":5.0,
         "stamp":"2020-01-02T00:00:00.123Z","tag":"apple","note":"alpha"},
        {"id":"r2","views":15,"price":7.5, "rating":7.5,
         "stamp":"2020-01-02T00:00:00.456Z","tag":"apple"},
        {"id":"r3","views":25,"price":5.0, "rating":5.0,
         "stamp":"2020-01-03T12:34:56.789Z","tag":"banana"},
        {"id":"r4","views":35,"price":12.0,"stamp":"2020-01-05T00:00:00Z"},
        {"id":"r5","views":45,"price":0.25}
    ])
}

async fn facets33_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FACETS33_SCHEMA_TOML).expect("app must build");
    let (status, body) = post_docs(&app, &facets33_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the facets33 corpus must succeed, got {body}"
    );
    (app, dir)
}

// --- duplicated schema/corpus for manifest-errors.tsv's `update9` rows
// (issue #9). Mirrors `tests/update_pipeline.rs::UPDATE9_SCHEMA_TOML` /
// `update9_corpus` exactly — same duplication precedent as `sortdebt`/
// `facets33` above. Like `sortdebt` (and unlike `facets`/`keyorder`/
// `facets33`), this schema names its core `update9` literally, matching the
// manifest-errors rows' own request paths verbatim, so `update9/...` rows are
// NOT rewritten in `app_and_request_url` below.
//
// The `update9/...` rows replay IN MANIFEST ORDER against this ONE app
// instance below (same statefulness `sortdebt`'s rows rely on for their own
// ordering-sensitive assertions), which is exactly what lets
// `update_select_after_delete_id` etc. reproduce the mutation sequence
// `solr-ref/capture.sh`'s tail block captured.

const UPDATE9_SCHEMA_TOML: &str = r#"
[core]
name = "update9"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "category"
type = "string"
stored = true
fast = true
multi_valued = true

[[fields]]
name = "title"
type = "string"
stored = true
fast = true

[[fields]]
name = "nick"
type = "string"
stored = true
fast = true

[[fields]]
name = "alias"
type = "string"
stored = true
fast = true

[[copy_fields]]
source = "nick"
dest = "alias"

[[dynamic_fields]]
pattern = "*_dt"
type = "date"
stored = true
fast = true
"#;

/// The exact `u1..u5` seed corpus `capture.sh`'s reset step reseeds before
/// every run — identical to `tests/update_pipeline.rs::update9_corpus`.
fn update9_corpus() -> Value {
    json!([
        {"id":"u1","body":"quick brown fox","category":["keep"]},
        {"id":"u2","body":"lazy dog","category":["temp"]},
        {"id":"u3","body":"lazy afternoon","category":["temp"]},
        {"id":"u4","body":"garden path","category":["keep"]},
        {"id":"u5","body":"nothing much here","category":["temp","keep"]}
    ])
}

/// `POST /solr/update9/update?commit=true` — cannot be `common::post_docs`,
/// which is hardcoded to `common::CORE`.
async fn update9_post_docs(app: &Router, docs: &Value) -> (StatusCode, Value) {
    common::request_full(
        app,
        "POST",
        "update9/update?commit=true",
        Some(&docs.to_string()),
    )
    .await
}

/// Builds a fresh `update9`-schema app and seeds `update9_corpus()`, matching
/// `capture.sh`'s reset-and-reseed step before its own captures run.
async fn update9_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), UPDATE9_SCHEMA_TOML).expect("app must build");
    let (status, body) = update9_post_docs(&app, &update9_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "seeding the update9 corpus must succeed, got {body}"
    );
    (app, dir)
}

/// The `stats` core (issue #5): named `stats` literally, matching the
/// `stats/...` leading segment `solr-ref/capture.sh`'s issue-#5 block wrote
/// into `manifest-errors.tsv` — same "own name, unrewritten" treatment as
/// `sortdebt`/`update9` rather than the rewritten-to-`content` treatment
/// `facets`/`keyorder`/`facets33` get, since none of those needed both a
/// non-`content` schema AND matched their manifest rows' own core segment
/// simultaneously the way this one does.
///
/// `views` (missing on `st6`) and `price` (missing on `st5`) mirror
/// `tests/stats.rs::STATS_SCHEMA_TOML` exactly — kept as an independent copy
/// rather than shared, per that file's own doc comment on why the range/
/// facet-debt/sortdebt cores each define their own schema string instead of
/// exporting one from `common`.
const STATS_SCHEMA_TOML: &str = r#"
[core]
name = "stats"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true

[[fields]]
name = "price"
type = "double"
stored = true
fast = true
"#;

/// The exact `st1..st6` corpus `solr-ref/capture.sh`'s issue-#5 block
/// indexes: `views` missing on `st6`, `price` missing on `st5`.
fn stats_corpus() -> Value {
    json!([
        {"id":"st1","views":10,"price":1.5},
        {"id":"st2","views":20,"price":2.5},
        {"id":"st3","views":30,"price":3.5},
        {"id":"st4","views":40,"price":4.5},
        {"id":"st5","views":50},
        {"id":"st6","price":5.5}
    ])
}

/// `POST /solr/stats/update?commit=true` — cannot be `common::post_docs`,
/// which is hardcoded to `common::CORE`.
async fn stats_post_docs(app: &Router, docs: &Value) -> (StatusCode, Value) {
    common::request_full(
        app,
        "POST",
        "stats/update?commit=true",
        Some(&docs.to_string()),
    )
    .await
}

/// Builds a fresh `stats`-schema app and indexes the full `st1..st6` corpus
/// in one commit, matching `solr-ref/capture.sh`'s issue-#5 block.
async fn stats_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), STATS_SCHEMA_TOML).expect("app must build");
    let (status, body) = stats_post_docs(&app, &stats_corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the stats corpus must succeed, got {body}"
    );
    (app, dir)
}

/// The issue-#99 capture uses a dedicated three-document core. Its version
/// values are deliberately time-seeded, so the manifest row is run here for
/// its request/envelope shape while `tests/version_field.rs` verifies the
/// local metrics against these actual indexed values.
const VERSION99_SCHEMA_TOML: &str = r#"
[core]
name = "version99"
unique_key = "id"
default_field = "id"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true
"#;

async fn version99_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), VERSION99_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::request_full(
        &app,
        "POST",
        "version99/update?commit=true",
        Some(r#"[{"id":"v1"},{"id":"v2"},{"id":"v3"}]"#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the version99 corpus must succeed, got {body}"
    );
    (app, dir)
}

/// The issue-#104 `hl.fragsize` capture uses a dedicated single-document core
/// whose `body` is long enough that "whole field" and "fragmented" are
/// visibly different answers — the shared 5-doc corpus's four-word `body` is
/// not (see `tests/highlighting.rs`'s `long_field_app`).
const FRAGSIZE_SCHEMA_TOML: &str = r#"
[core]
name = "fragsize104"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "body"
type = "text_en"
stored = true
"#;

/// The document indexed here is byte-for-byte the one indexed into the live
/// `solr:9` container the `fragsize104` fixtures were captured from, so the
/// hermetic replay compares like with like.
async fn fragsize_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), FRAGSIZE_SCHEMA_TOML).expect("app must build");
    let (status, body) = common::request_full(
        &app,
        "POST",
        "fragsize104/update?commit=true",
        Some(
            r#"[{"id":"long1","body":"quick prototype notes from the engineering standup this morning. the team reviewed the roadmap for the next quarter and discussed several open risks around supply chain timing. afterwards everyone broke for lunch and reconvened at two in the afternoon to continue the planning session for the rest of the week."}]"#,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the fragsize104 corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Issue #223's configured-spellchecker capture uses two dictionaries with
/// deliberately different nearby terms, so repeated-dictionary precedence is
/// visible. This mirrors `tests/spellcheck.rs` and the appended capture block.
const SPELLCHECK_223_SCHEMA_TOML: &str = r#"
[core]
name = "spellcheck_223"
unique_key = "id"
default_field = "spellcheck_en"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "spellcheck_en"
type = "text_en"
stored = true
multi_valued = true

[[fields]]
name = "spellcheck_und"
type = "text_en"
stored = true
multi_valued = true
"#;

async fn spellcheck_223_app() -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = common::app_with_schema(dir.path(), SPELLCHECK_223_SCHEMA_TOML)
        .expect("spellcheck app must build");
    let (status, body) = common::request_full(
        &app,
        "POST",
        "spellcheck_223/update?commit=true",
        Some(
            r#"[
                {"id":"s1","spellcheck_en":["quick quick quick rocket rocket"],"spellcheck_und":["quack quack quack garden"]},
                {"id":"s2","spellcheck_en":["quick brown fox"],"spellcheck_und":["quack garden"]}
            ]"#,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the spellcheck_223 corpus must succeed, got {body}"
    );
    (app, dir)
}

/// Ratified, **permanent** divergences from captured Solr behaviour — the
/// opposite of `EXPECTED_DIVERGENCES` below, which is a self-expiring to-do
/// list for unbuilt features. Every entry here cites the PRD/findings
/// section that ratifies it (findings doc's "Expected-divergence list"
/// section explains the distinction). Printed during the manifest-errors
/// run so the accepted set is visible, not silent.
const ACCEPTED_DIVERGENCES: &[(&str, &str)] = &[
    (
        "err_missing_core",
        "finding 15 / PRD ratified-divergence 1: an unknown core is Solr's 404 HTML easter \
         egg, Wayfinder's normal 404 JSON error envelope — status only, body deliberately \
         not JSON on the fixture side",
    ),
    (
        "update_unknown_field_schemaless",
        "PRD ratified-divergence 3: Wayfinder has no schemaless mode and no \
         schemaless_probe core; hermetically this 404s where the fixture is 200",
    ),
    (
        "facet_non_docvalues_text",
        "finding 105 / PRD ratified-divergence 2: Wayfinder 400s a facet on an unfacetable \
         (non-docValues) field where Solr 200s with empty counts",
    ),
    (
        "facet_non_docvalues_text_enum",
        "finding 105 / PRD ratified-divergence 2, facet.method=enum variant of the same field",
    ),
    (
        "facet_stored_only_field",
        "finding 105 / PRD ratified-divergence 2, stored-only (non-indexed) field variant",
    ),
    (
        "update_unknown_core",
        "finding 49 / same divergence family as err_missing_core: an unknown core on POST \
         /update is Solr's 404 HTML easter egg, Wayfinder's normal 404 JSON error envelope",
    ),
    (
        "ping_unknown_core",
        "finding 49 / same divergence family as err_missing_core: an unknown core on GET \
         /admin/ping is Solr's 404 HTML easter egg, Wayfinder's normal 404 JSON error envelope",
    ),
    (
        "ping_unknown_core_delete",
        "finding 49: DELETE on an unknown core's /admin/ping is a Jetty-level 405 with an \
         empty body in Solr; Wayfinder stays method-agnostic and serves its normal JSON 404 \
         there too — noted, not matched",
    ),
];

fn accepted_divergence_reason(name: &str) -> Option<&'static str> {
    ACCEPTED_DIVERGENCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

/// Ratified, **permanent** divergence for `fl=score`'s BM25 float magnitude —
/// PRD ratified-divergence 4. Unlike `ACCEPTED_DIVERGENCES` above (whole-entry
/// pass/fail), this list only waives the `response.docs[*].score` *value*
/// within `RANKED_RELEVANCE_ENTRIES` below; everything else about these two
/// entries — HTTP status, doc ranking order (`response.docs[].id`), and
/// `response.maxScore`'s presence/type — is still checked for real by
/// `hermetic_whole_query_set_matches_committed_fixtures`. See PRD's "Ratified
/// divergences from captured Solr behaviour", entry 4, for the measured
/// ratios and rationale (issue #34).
const RANKED_SCORE_VALUE_RATIFIED: &[(&str, &str)] = &[
    (
        "select_term_scored",
        "PRD ratified-divergence 4: Tantivy's BM25 score for doc2 (~0.875) vs. Solr's captured \
         0.457 is a scoring-formula difference, not a wiring bug; ranking order and maxScore \
         shape are still verified",
    ),
    (
        "select_quick_scored",
        "PRD ratified-divergence 4: Tantivy's BM25 score for doc3 (~0.940) vs. Solr's captured \
         0.413 is a scoring-formula difference, not a wiring bug; ranking order and maxScore \
         shape are still verified",
    ),
];

fn ranked_score_value_ratified_reason(name: &str) -> Option<&'static str> {
    RANKED_SCORE_VALUE_RATIFIED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

/// `manifest-errors.tsv`'s own self-expiring to-do list — the counterpart of
/// `EXPECTED_DIVERGENCES` below for `manifest.tsv`, but scoped to this file's
/// runner since the two loops have different app-selection and check logic.
///
/// Issue #35 (the `facet_unknown_field` / `facet_err_query_single` /
/// `facet_err_field_single` / `facet_err_query_field` /
/// `facet_err_query_vs_unfacetable` divergences found in issues #31 and #33)
/// is fixed: `src/lib.rs::select` now builds the `response` block before
/// computing `facet_counts`, and attaches it to a `facet.query`/`facet.field`
/// error the same way Solr's own fixtures for those cases do. A
/// `facet.range` error still carries no `response` (Solr detects it before
/// the base query ever runs — `facet_err_range_single`/`_query_range`/
/// `_field_range`/`_all_three` are unaffected and were never listed here),
/// which is why the fix distinguishes the two in `src/facet.rs`'s
/// `PreQueryFacetError` marker rather than attaching `response` to every
/// facet error uniformly.
///
/// Issue #5 (stats component) is fixed: `stats=true`/`stats.field` land in
/// `src/stats.rs`, wired into `src/lib.rs::select`, so `stats_views`,
/// `stats_multi_fields`, `stats_zero`, and `stats_zero_fq` all now match the
/// captured fixtures for real and their entries are removed rather than left
/// here — this list is empty until the next unbuilt-feature entry needs it.
/// Issue #59 (`/admin/info/system` version handshake): the reported
/// `lucene.solr-spec-version` is deliberately configured (default `9.0.0`,
/// PRD open question 2) rather than mirroring the captured Solr's own
/// `9.10.1`, and the rest of the envelope (`jvm`/`system` stats — uptime,
/// memory, load, hostnames, command-line args) is inherently unreproducible
/// host-specific volatility, same category as `ping`'s `rid` problem below.
/// Both are permanent, not a to-do: there is no real Wayfinder host JVM/OS to
/// introspect meaningfully, and the version is a config choice, not a bug.
const EXPECTED_DIVERGENCES_MANIFEST_ERRORS: &[(&str, &str)] = &[
    (
        "admin_info_system",
        "issue #59: `responseHeader`, `mode`, `solr_home`, `core_root`, and the top-level key set \
         are compared exactly and do match. The suppressed diffs are: `lucene.solr-spec-version` \
         (a deliberate config-driven choice, default 9.0.0 per PRD open question 2, not a mirror of \
         the captured Solr's 9.10.1) and `lucene.solr-impl-version`/`lucene-impl-version` (build \
         hash + date, unreproducible); `jvm.*` (memory/uptime/vendor/processors, real host JVM \
         stats Wayfinder has no equivalent of); and `system.*` (host CPU/memory/load stats) — same \
         permanent category as `ping`'s `rid` in EXPECTED_DIVERGENCES below",
    ),
    (
        "stats_version_max",
        "issue #99: Solr `_version_` values are update-log/time-derived and Wayfinder deliberately \
         seeds per-core values from Unix-epoch milliseconds, so exact metrics cannot be fixture-stable; \
         tests/version_field.rs checks this row's envelope and the actual fast-field maximum",
    ),
];

fn expected_divergence_manifest_errors_reason(name: &str) -> Option<&'static str> {
    EXPECTED_DIVERGENCES_MANIFEST_ERRORS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

// --- normaliser: dropped fields pass and are logged -----------------------

#[test]
fn normalize_drops_qtime_and_logs_touched_path() {
    let v = json!({"responseHeader": {"status": 0, "QTime": 42}});
    let n = normalize(v);

    assert!(
        n.value["responseHeader"].get("QTime").is_none(),
        "QTime must be dropped"
    );
    assert!(
        n.touched.contains(&"responseHeader.QTime".to_string()),
        "dropping QTime must be recorded in touched paths, got {:?}",
        n.touched
    );
}

#[test]
fn differing_qtime_does_not_appear_as_a_diff() {
    let expected = normalize(json!({
        "responseHeader": {"QTime": 1},
        "response": {"numFound": 0, "docs": []}
    }));
    let actual = normalize(json!({
        "responseHeader": {"QTime": 99},
        "response": {"numFound": 0, "docs": []}
    }));

    let report = diff(&expected.value, &actual.value);

    assert!(
        report.diffs.is_empty(),
        "differing QTime must not be a diff after normalisation, got {:?}",
        report.diffs
    );
}

#[test]
fn normalize_drops_error_msg_and_metadata_but_keeps_code() {
    let v = json!({"error": {"code": 400, "msg": "undefined field x", "metadata": ["a", "b"]}});
    let n = normalize(v);

    assert!(
        n.value["error"].get("msg").is_none(),
        "error.msg must be dropped"
    );
    assert!(
        n.value["error"].get("metadata").is_none(),
        "error.metadata must be dropped"
    );
    assert_eq!(n.value["error"]["code"], 400, "error.code must be kept");
    assert!(
        n.touched.contains(&"error.msg".to_string()),
        "dropping error.msg must be recorded, got {:?}",
        n.touched
    );
    assert!(
        n.touched.contains(&"error.metadata".to_string()),
        "dropping error.metadata must be recorded, got {:?}",
        n.touched
    );
}

#[test]
fn differing_error_msg_and_metadata_do_not_appear_as_a_diff() {
    let expected = normalize(json!({
        "error": {"code": 400, "msg": "undefined field x", "metadata": ["error-class", "A"]}
    }));
    let actual = normalize(json!({
        "error": {"code": 400, "msg": "a totally different message", "metadata": ["error-class", "B", "root-error-class", "C"]}
    }));

    let report = diff(&expected.value, &actual.value);

    assert!(
        report.diffs.is_empty(),
        "differing free-text error.msg/error.metadata must not be a diff, got {:?}",
        report.diffs
    );
}

#[test]
fn differing_error_code_is_still_a_diff() {
    let expected = normalize(json!({"error": {"code": 400, "msg": "x", "metadata": []}}));
    let actual = normalize(json!({"error": {"code": 500, "msg": "y", "metadata": []}}));

    let report = diff(&expected.value, &actual.value);

    assert!(
        !report.diffs.is_empty(),
        "error.code must still be compared and a mismatch must be a diff"
    );
}

// --- score tolerance --------------------------------------------------------

#[test]
fn score_within_tolerance_passes_and_is_logged() {
    let tol = score_tolerance();
    let expected = json!({"response": {"docs": [{"id": "doc1", "score": 1.2345}]}});
    let actual = json!({"response": {"docs": [{"id": "doc1", "score": 1.2345 + tol / 2.0}]}});

    let report = diff(&expected, &actual);

    assert!(
        report.diffs.is_empty(),
        "score within tolerance must pass, got diffs {:?}",
        report.diffs
    );
    assert!(
        report.touched.iter().any(|p| p.contains("score")),
        "score comparison must be logged in touched even when it passes, got {:?}",
        report.touched
    );
}

#[test]
fn score_outside_tolerance_fails() {
    let tol = score_tolerance();
    let expected = json!({"response": {"docs": [{"id": "doc1", "score": 1.0}]}});
    let actual = json!({"response": {"docs": [{"id": "doc1", "score": 1.0 + tol * 10.0}]}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "score outside tolerance must be reported as a diff"
    );
}

// --- real diffs must fail ---------------------------------------------------

#[test]
fn diff_fails_on_numfound_off_by_one() {
    let expected = json!({"response": {"numFound": 5, "start": 0, "docs": []}});
    let actual = json!({"response": {"numFound": 6, "start": 0, "docs": []}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "numFound off by one must be reported as a diff"
    );
    assert!(
        report.diffs.iter().any(|d| d.path.contains("numFound")),
        "diff must name the numFound path, got {:?}",
        report.diffs
    );
}

#[test]
fn diff_fails_on_doc_reordered() {
    let expected = json!({"response": {"docs": [{"id": "doc1"}, {"id": "doc2"}]}});
    let actual = json!({"response": {"docs": [{"id": "doc2"}, {"id": "doc1"}]}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "a reordered doc list must be reported as a diff by the generic differ"
    );
}

#[test]
fn diff_fails_on_facet_count_changed() {
    let expected =
        json!({"facet_counts": {"facet_fields": {"category": ["animals", 2, "classic", 2]}}});
    let actual =
        json!({"facet_counts": {"facet_fields": {"category": ["animals", 3, "classic", 2]}}});

    let report = diff(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "a changed facet count must be reported as a diff"
    );
}

// --- ranked-ID-list mode -----------------------------------------------------

fn ranked(id: &str, score: Option<f64>) -> RankedDoc {
    RankedDoc {
        id: id.to_string(),
        score,
    }
}

#[test]
fn ranked_id_order_difference_fails_even_with_identical_membership() {
    let expected = vec![ranked("doc2", None), ranked("doc1", None)];
    let actual = vec![ranked("doc1", None), ranked("doc2", None)];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "identical membership in a different order must fail ranked-ID comparison"
    );
}

#[test]
fn ranked_id_order_matching_passes() {
    let docs = vec![
        ranked("doc2", None),
        ranked("doc1", None),
        ranked("doc3", None),
    ];

    let report = diff_ranked_ids(&docs, &docs.clone());

    assert!(
        report.diffs.is_empty(),
        "identical order must pass, got {:?}",
        report.diffs
    );
}

#[test]
fn ranked_docs_extracts_ordered_id_score_pairs_from_an_envelope() {
    let envelope = json!({
        "response": {"docs": [{"id": "doc2", "score": 1.0}, {"id": "doc1", "score": 0.5}]}
    });

    assert_eq!(
        ranked_docs(&envelope),
        vec![ranked("doc2", Some(1.0)), ranked("doc1", Some(0.5))]
    );
}

#[test]
fn ranked_docs_extracts_ids_with_no_score_when_the_envelope_has_none() {
    let envelope = json!({
        "response": {"docs": [{"id": "doc2"}, {"id": "doc1"}]}
    });

    assert_eq!(
        ranked_docs(&envelope),
        vec![ranked("doc2", None), ranked("doc1", None)]
    );
}

// --- ranked-ID score tolerance (issue #31 follow-up 1-2) ---------------------
//
// `diff_ranked_ids` used to compare id order only. Score comparison was the
// spec's original intent (PRD §8: "ranked-ID-list mode with score
// tolerance") but no fixture's `fl` included `score`, so this path was dead
// code exercised only by synthetic tests. `select_term_scored`/
// `select_quick_scored` (issue #31) close that gap — see the
// fixture-derived tests below.

#[test]
fn ranked_id_score_within_tolerance_passes_and_is_logged() {
    let tol = score_tolerance();
    let expected = vec![ranked("doc1", Some(1.2345))];
    let actual = vec![ranked("doc1", Some(1.2345 + tol / 2.0))];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        report.diffs.is_empty(),
        "score within tolerance must pass, got {:?}",
        report.diffs
    );
    assert!(
        report.touched.iter().any(|p| p.contains("score")),
        "the score comparison must be logged in touched even when it passes, got {:?}",
        report.touched
    );
}

#[test]
fn ranked_id_score_outside_tolerance_fails_naming_the_score_path() {
    let tol = score_tolerance();
    let expected = vec![ranked("doc1", Some(1.0))];
    let actual = vec![ranked("doc1", Some(1.0 + tol * 10.0))];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "score outside tolerance must be reported as a diff"
    );
    assert!(
        report.diffs.iter().any(|d| d.path.contains("score")),
        "the diff must name the score path, got {:?}",
        report.diffs
    );
}

#[test]
fn ranked_id_score_present_vs_missing_is_a_diff() {
    let expected = vec![ranked("doc1", Some(1.0))];
    let actual = vec![ranked("doc1", None)];

    let report = diff_ranked_ids(&expected, &actual);

    assert!(
        !report.diffs.is_empty(),
        "a score present on one side and missing on the other must be a diff, not silently \
         skipped"
    );
}

/// The tolerance path exercised against **real** fixture data, not just
/// synthetic id/score pairs (issue #31 follow-up 2): loads the actual
/// `select_term_scored` fixture, perturbs its real BM25 scores by known
/// amounts relative to `score_tolerance()`, and diffs the fixture against
/// itself. `score_tolerance()/2` must pass (and log the score path in
/// `touched`); `10 * score_tolerance()` must fail, naming the score path.
#[test]
fn ranked_id_score_tolerance_exercised_against_real_select_term_scored_fixture() {
    let expected = fixture("select_term_scored");
    let expected_docs = ranked_docs(&expected);
    assert!(
        !expected_docs.is_empty(),
        "select_term_scored fixture must have ranked docs to exercise this path"
    );
    assert!(
        expected_docs.iter().all(|d| d.score.is_some()),
        "select_term_scored fixture's docs must all carry a real score, got {:?}",
        expected_docs
    );
    let tol = score_tolerance();

    let within: Vec<RankedDoc> = expected_docs
        .iter()
        .map(|d| ranked(&d.id, d.score.map(|s| s + tol / 2.0)))
        .collect();
    let within_report = diff_ranked_ids(&expected_docs, &within);
    assert!(
        within_report.diffs.is_empty(),
        "a perturbation of score_tolerance()/2 against the real fixture must pass, got {:?}",
        within_report.diffs
    );
    assert!(
        within_report.touched.iter().any(|p| p.contains("score")),
        "the real-fixture perturbation within tolerance must still be logged in touched, \
         got {:?}",
        within_report.touched
    );

    let outside: Vec<RankedDoc> = expected_docs
        .iter()
        .map(|d| ranked(&d.id, d.score.map(|s| s + tol * 10.0)))
        .collect();
    let outside_report = diff_ranked_ids(&expected_docs, &outside);
    assert!(
        !outside_report.diffs.is_empty(),
        "a perturbation of 10 * score_tolerance() against the real fixture must fail"
    );
    assert!(
        outside_report
            .diffs
            .iter()
            .any(|d| d.path.contains("score")),
        "the failure must name the score path, got {:?}",
        outside_report.diffs
    );
}

// --- params key order (documents existing serde_json behaviour) ------------

#[test]
fn params_object_equality_is_key_order_insensitive_by_construction() {
    // PRD §8 / findings fact 6: `responseHeader.params` key order is not
    // request order in Solr. No normaliser code is needed for this —
    // `serde_json::Value::Object` already compares as an order-independent
    // map. This test pins that fact rather than exercising our own code
    // (spec: "assert it rather than writing code for it").
    let a: Value = serde_json::from_str(r#"{"q":"*:*","wt":"json","rows":"10"}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"rows":"10","wt":"json","q":"*:*"}"#).unwrap();
    assert_eq!(a, b, "JSON object equality must not depend on key order");
}

// --- manifest loader ---------------------------------------------------------

#[test]
fn load_manifest_parses_every_line_of_the_real_manifest() {
    let path = manifest_path();
    let raw = std::fs::read_to_string(&path).expect("read solr-ref/manifest.tsv");
    let expected_count = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();

    let entries = load_manifest(&path);

    assert_eq!(
        entries.len(),
        expected_count,
        "loader must parse every non-blank, non-comment line"
    );
    assert!(
        entries.contains(&ManifestEntry {
            name: "ping".to_string(),
            status: 200,
            path: "admin/ping?wt=json".to_string(),
        }),
        "loader must parse the ping entry, got {:?}",
        entries
    );
    assert!(
        entries
            .iter()
            .any(|e| e.name == "err_bad_sort" && e.status == 400),
        "loader must parse error entries with their non-200 status"
    );
}

#[test]
fn manifest_covers_terms_but_deliberately_excludes_admin_mbeans() {
    let entries = load_manifest(&manifest_path());

    assert!(
        entries.iter().any(|entry| {
            entry.name == "terms_body"
                && entry.status == 200
                && entry.path == "terms?terms=true&terms.fl=body&omitHeader=true&wt=json"
        }),
        "/terms returns real index data and must have differential coverage"
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.path.starts_with("admin/mbeans")),
        "/admin/mbeans deliberately serves an honest subset and must not become a permanently \
         waived whole-response differential row (PRD section 5, v2.75)"
    );
}

#[test]
fn load_manifest_skips_blanks_and_comments_and_tolerates_trailing_columns() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let path = dir.path().join("manifest.tsv");
    std::fs::write(
        &path,
        "\n# a leading comment\nfoo\t200\tselect?q=*:*&wt=json\n\n# another comment\nbar\t400\tselect?q=bad&wt=json\textra\tcolumns\n",
    )
    .expect("write temp manifest");

    let entries = load_manifest(&path);

    assert_eq!(entries.len(), 2, "blank lines and comments must be skipped");
    assert_eq!(
        entries[0],
        ManifestEntry {
            name: "foo".to_string(),
            status: 200,
            path: "select?q=*:*&wt=json".to_string(),
        }
    );
    assert_eq!(entries[1].name, "bar");
    assert_eq!(entries[1].status, 400);
    assert_eq!(
        entries[1].path, "select?q=bad&wt=json",
        "extra trailing columns beyond path must be tolerated (ignored), not error"
    );
}

// --- manifest-errors loader (issue #31, item 3) ------------------------------

#[test]
fn load_manifest_errors_parses_every_line_of_the_real_manifest_errors() {
    let path = manifest_errors_path();
    let raw = std::fs::read_to_string(&path).expect("read solr-ref/manifest-errors.tsv");
    let expected_count = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();

    let entries = load_manifest_errors(&path);

    assert_eq!(
        entries.len(),
        expected_count,
        "loader must parse every non-blank, non-comment line of manifest-errors.tsv"
    );
    assert!(
        entries.contains(&ManifestErrorEntry {
            name: "err_missing_core".to_string(),
            status: 404,
            method: "GET".to_string(),
            url: "nosuchcore/select?q=*:*&wt=json".to_string(),
            body: None,
            base_url: None,
        }),
        "loader must parse err_missing_core with no body/base-url columns, got {:?}",
        entries
    );
    assert!(
        entries.contains(&ManifestErrorEntry {
            name: "err_update_bad_json".to_string(),
            status: 400,
            method: "POST".to_string(),
            url: "content/update?commit=true&wt=json".to_string(),
            body: Some("{not json".to_string()),
            base_url: None,
        }),
        "loader must parse err_update_bad_json's body column, got {:?}",
        entries
    );
    assert!(
        entries
            .iter()
            .any(|e| e.name == "update_unknown_field_schemaless"
                && e.base_url.as_deref() == Some("http://localhost:8983/solr")),
        "loader must parse the 6th (base-url) column when present, got {:?}",
        entries
    );
}

#[test]
fn load_manifest_errors_skips_blanks_and_comments_and_tolerates_missing_columns() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let path = dir.path().join("manifest-errors.tsv");
    std::fs::write(
        &path,
        "\n# a leading comment\n\
         foo\t404\tGET\tnosuchcore/select?q=*:*&wt=json\t\t\n\
         \n# another comment\n\
         bar\t400\tPOST\tcontent/update?wt=json\t[]\thttp://localhost:8984/solr\n",
    )
    .expect("write temp manifest-errors");

    let entries = load_manifest_errors(&path);

    assert_eq!(entries.len(), 2, "blank lines and comments must be skipped");
    assert_eq!(
        entries[0],
        ManifestErrorEntry {
            name: "foo".to_string(),
            status: 404,
            method: "GET".to_string(),
            url: "nosuchcore/select?q=*:*&wt=json".to_string(),
            body: None,
            base_url: None,
        },
        "empty body/base-url columns must parse as None, not Some(\"\")"
    );
    assert_eq!(
        entries[1],
        ManifestErrorEntry {
            name: "bar".to_string(),
            status: 400,
            method: "POST".to_string(),
            url: "content/update?wt=json".to_string(),
            body: Some("[]".to_string()),
            base_url: Some("http://localhost:8984/solr".to_string()),
        }
    );
}

// --- hermetic whole-query-set run -------------------------------------------

/// The subset of manifest entries that are free-text relevance queries (PRD
/// §8: "compare ranked ID lists, not just result sets"). `select_term` is
/// the current free-text `q=` entry; `select_fq_multi` is a filter query, not
/// relevance, so it is diffed generically like everything else.
/// `select_term_scored`/`select_quick_scored` (issue #31) add score-bearing
/// entries so the ranked+score path runs against real data, not just
/// synthetic tests. Those two carry a ratified, permanent BM25-magnitude
/// divergence (`RANKED_SCORE_VALUE_RATIFIED` above, PRD ratified-divergence
/// 4) — see the loop below for how that's handled differently from
/// `select_term`.
const RANKED_RELEVANCE_ENTRIES: &[&str] =
    &["select_term", "select_term_scored", "select_quick_scored"];

/// Manifest entries with a *known, currently real* Wayfinder-vs-Solr
/// divergence, each caused by an unbuilt feature rather than a harness bug
/// (escalated and accepted by the orchestrator — see this issue's handoff).
/// Excluded from the pass/fail loop below, but only ever as a documented,
/// self-expiring to-do: every reason names the issue that owns the fix, and
/// the guard at the end of the test loop below FAILS the moment any of these
/// entries stops diverging — that means the feature landed and the entry
/// must be deleted from this list, not that the harness can go quiet about
/// it. `ping` gets no normaliser carve-out for its unreproducible `rid`
/// value; encoding that in the normaliser would risk hiding a real
/// `params` diff on every other entry, so it lives here instead, alongside
/// the rest.
// Sort (issue #2) used to be listed here: #11 landed sort validation, which made
// `err_bad_sort` match, and #2 landed the ordering itself, which made
// `select_sort` — plus the sixteen `select_sort_*` / `err_sort_*` entries added
// with it — match too. Both are gone from this list, as designed.
// Faceting (issue #3) used to hold seven entries here — `facet_mincount`,
// `facet_limit`, `facet_missing`, `facet_query`, `facet_json_nl_map`,
// `facet_zero`, `facet_all_filtered`. Real fast-field aggregation over the whole
// term dictionary made all seven match, so they are gone too.
// `select_term_scored`/`select_quick_scored` (issue #31) used to say Wayfinder
// silently dropped `fl=score`. Issue #34 implemented `fl=score` (per-doc
// `score` key, correct key order, `response.maxScore`) and the *ranking*
// (doc id order) now matches Solr exactly for both entries. The score
// *magnitudes* still don't (Tantivy's own BM25 numerically disagrees with
// Solr's BM25Similarity), but that's no longer parked here as a to-do: it's
// ratified permanently as PRD ratified-divergence 4 and handled by
// `RANKED_SCORE_VALUE_RATIFIED` above, so both entries are gone from this
// list.
// the canonical `content` core, findings 52+). `hl`/`hl.fl` and friends are
// now implemented (`src/highlight.rs`) and every `hl_*` row matches, so all
// eight entries that were parked here are gone.
const EXPECTED_DIVERGENCES: &[(&str, &str)] = &[
    (
        "ping",
        "`responseHeader.params` carries Solr ping-handler artifacts incl. a per-run `rid` counter no implementation can reproduce; see the same carve-out in `tracer_bullet.rs::ping_reports_ok`",
    ),
    (
        "admin_system",
        "issue #59: `responseHeader`, `mode`, the top-level key set, and `core.schema` \
         (finding 78's version-detection field, `\"drupal-4.4.0-solr-9.x-0\"`) are compared \
         exactly and do match. The suppressed diffs are: `lucene.solr-spec-version` (a \
         deliberate config-driven choice, default 9.0.0 per PRD open question 2, not a mirror \
         of the captured Solr's 9.10.1) and `lucene.solr-impl-version`/`lucene-impl-version` \
         (build hash + date, unreproducible); `jvm.*` (memory/uptime/vendor/processors, real \
         host JVM stats Wayfinder has no equivalent of); `system.*` (host CPU/memory/load \
         stats); and `core.host`/`core.now`/`core.start`/`core.directory.*` (hostname, \
         timestamps, real filesystem paths on the capture host) — same permanent category as \
         `admin_info_system` in EXPECTED_DIVERGENCES_MANIFEST_ERRORS above and `ping`'s `rid`",
    ),
];

/// The `EXPECTED_DIVERGENCES` reason for `name`, or `None` if `name` is not
/// in the list. Every entry has a mandatory reason by construction (the list
/// is `&[(&str, &str)]`) — this just looks one up by name.
fn expected_divergence_reason(name: &str) -> Option<&'static str> {
    EXPECTED_DIVERGENCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, reason)| *reason)
}

#[tokio::test]
async fn hermetic_whole_query_set_matches_committed_fixtures() {
    let (app, _dir) = indexed_app().await;
    let entries = load_manifest(&manifest_path());
    assert!(!entries.is_empty(), "manifest must not be empty");

    let manifest_names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    for (name, reason) in EXPECTED_DIVERGENCES {
        assert!(
            manifest_names.contains(name),
            "EXPECTED_DIVERGENCES entry `{name}` (reason: {reason}) does not match any \
             manifest entry — fix the name or remove the stale entry"
        );
    }

    let mut failures = Vec::new();
    eprintln!("--- differential run: every manifest entry ---");
    for entry in &entries {
        // `mlt_*` rows need the dedicated 20-doc MLT corpus (real shared
        // vocabulary across topic clusters), not this loop's 5-doc
        // tracer-bullet corpus (`indexed_app()`) — same rationale as
        // `FACETS_SCHEMA_TOML` above for `manifest-errors.tsv` rows that need
        // a non-canonical corpus. `tests/mlt.rs`'s own
        // `hermetic_mlt_manifest_entries_match_committed_fixtures` runs these
        // ten rows against that corpus instead, so this is not lost
        // coverage, just routed to the right harness (issue #6).
        if entry.name.starts_with("mlt_") {
            continue;
        }
        // `edismax_*` rows need the dedicated 10-doc edismax corpus
        // (`title`+`body` text_en fields, `eA`-`eD`/`pA`/`pB`/`mmA`-`mmD`),
        // not this loop's 5-doc tracer-bullet corpus — same rationale as
        // `mlt_*` above. `tests/edismax.rs`'s own
        // `hermetic_edismax_manifest_entries_match_committed_fixtures` runs
        // these rows against that corpus instead (issue #7).
        if entry.name.starts_with("edismax_") {
            continue;
        }
        let (status, actual) = get(&app, &entry.path).await;
        let divergence_reason = expected_divergence_reason(&entry.name);

        if status.as_u16() != entry.status {
            let msg = format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            );
            eprintln!("{msg}");
            match divergence_reason {
                Some(reason) => eprintln!("  (expected divergence: {reason})"),
                None => failures.push(msg),
            }
            continue;
        }

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);
        eprintln!(
            "{}: normaliser touched {:?}",
            entry.name, expected_n.touched
        );

        if RANKED_RELEVANCE_ENTRIES.contains(&entry.name.as_str()) {
            let expected_docs = ranked_docs(&expected_n.value);
            let actual_docs = ranked_docs(&actual_n.value);
            let ranked_report = diff_ranked_ids(&expected_docs, &actual_docs);
            eprintln!(
                "{}: ranked-id diffs: {:?}, touched {:?}",
                entry.name, ranked_report.diffs, ranked_report.touched
            );

            if let Some(reason) = ranked_score_value_ratified_reason(&entry.name) {
                // PRD ratified-divergence 4: BM25 score *magnitude* is not
                // required to match, but ranking order and maxScore shape
                // still are — this is not a blanket waiver for the entry.
                let order_diffs: Vec<_> = ranked_report
                    .diffs
                    .iter()
                    .filter(|d| d.path == "response.docs[].id")
                    .collect();
                if !order_diffs.is_empty() {
                    failures.push(format!(
                        "{}: doc ranking order diverges, which PRD ratified-divergence 4 does \
                         NOT waive: {:?}",
                        entry.name, order_diffs
                    ));
                }

                let is_score_path =
                    |d: &&Diff| d.path.starts_with("response.docs[") && d.path.ends_with("].score");
                // Only a *magnitude* mismatch (both sides present, values
                // differ) is the ratified divergence. A `score` key missing
                // on one side is a wiring gap, not a scoring-formula
                // difference, and must still fail — otherwise this waiver
                // would silently pass a response that dropped `score`
                // entirely.
                let is_present_on_both =
                    |d: &&Diff| d.expected != "<missing>" && d.actual != "<missing>";
                let score_magnitude_diffs: Vec<_> = ranked_report
                    .diffs
                    .iter()
                    .filter(|d| is_score_path(d) && is_present_on_both(d))
                    .collect();
                if !score_magnitude_diffs.is_empty() {
                    eprintln!(
                        "  (ratified BM25-magnitude divergence: {reason}): {score_magnitude_diffs:?}"
                    );
                }

                let other_diffs: Vec<_> = ranked_report
                    .diffs
                    .iter()
                    .filter(|d| {
                        d.path != "response.docs[].id"
                            && !(is_score_path(d) && is_present_on_both(d))
                    })
                    .collect();
                if !other_diffs.is_empty() {
                    failures.push(format!(
                        "{}: ranked-id diffs outside the ratified score-value/order paths \
                         (includes any score-presence mismatch, which is not ratified): {:?}",
                        entry.name, other_diffs
                    ));
                }

                // `diff_ranked_ids`/`ranked_docs` only ever look at
                // `response.docs[]`, so they never notice if `maxScore` is
                // missing or the wrong type — assert that structurally here,
                // without pinning its exact (ratified-divergent) value.
                match actual_n
                    .value
                    .pointer("/response/maxScore")
                    .and_then(|v| v.as_f64())
                {
                    Some(max_score) => eprintln!(
                        "  (response.maxScore present and numeric, value not pinned per PRD \
                         ratified-divergence 4: {max_score})"
                    ),
                    None => failures.push(format!(
                        "{}: response.maxScore missing or not a number",
                        entry.name
                    )),
                }
            } else {
                match divergence_reason {
                    Some(reason) if ranked_report.diffs.is_empty() => failures.push(format!(
                        "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), \
                         but it now matches — the underlying feature has landed, so remove this \
                         entry from EXPECTED_DIVERGENCES in tests/differential.rs",
                        entry.name
                    )),
                    Some(reason) => eprintln!("  (expected divergence: {reason})"),
                    None if !ranked_report.diffs.is_empty() => failures.push(format!(
                        "{}: ranked-id diffs: {:?}",
                        entry.name, ranked_report.diffs
                    )),
                    None => {}
                }
            }
        } else {
            let report = diff(&expected_n.value, &actual_n.value);
            eprintln!(
                "{}: {} diffs, touched (tolerance-applied) {:?}",
                entry.name,
                report.diffs.len(),
                report.touched
            );
            if !report.diffs.is_empty() {
                eprintln!("  diffs: {:?}", report.diffs);
            }

            match divergence_reason {
                Some(reason) if report.diffs.is_empty() => failures.push(format!(
                    "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), \
                     but it now matches — the underlying feature has landed, so remove this \
                     entry from EXPECTED_DIVERGENCES in tests/differential.rs",
                    entry.name
                )),
                Some(reason) => eprintln!("  (expected divergence: {reason})"),
                None if !report.diffs.is_empty() => {
                    failures.push(format!("{}: {:?}", entry.name, report.diffs))
                }
                None => {}
            }
        }
    }

    eprintln!(
        "--- expected-divergence list (excluded from pass/fail above, each self-expiring) ---"
    );
    for (name, reason) in EXPECTED_DIVERGENCES {
        eprintln!("  {name}: {reason}");
    }

    assert!(
        failures.is_empty(),
        "hermetic differential failures against solr-ref fixtures:\n{}",
        failures.join("\n")
    );
}

// --- live Solr round trip (gated) -------------------------------------------

/// Live counterpart of the hermetic run, gated by `WAYFINDER_DIFF_SOLR=1` so
/// plain `cargo test` never touches the network or requires Docker. Run
/// `solr-ref/capture.sh` first — it leaves the container up with the schema
/// and corpus already loaded; this test does not orchestrate Docker itself.
///
/// `#[ignore]` is deliberately *not* also used here — the spec calls for one
/// gating mechanism, not both, so this stays a plain `#[test]` that no-ops
/// (and passes) when the env var is unset.
#[test]
fn live_solr_matches_committed_query_set() {
    if std::env::var("WAYFINDER_DIFF_SOLR").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping live Solr differential: run solr-ref/capture.sh, then set \
             WAYFINDER_DIFF_SOLR=1 to enable (WAYFINDER_DIFF_SOLR=1 cargo test --test differential)"
        );
        return;
    }

    let base_url = std::env::var("WAYFINDER_DIFF_SOLR_URL")
        .unwrap_or_else(|_| "http://localhost:8983/solr/content".to_string());

    let entries = load_manifest(&manifest_path());
    let mut failures = Vec::new();
    for entry in &entries {
        // `EXPECTED_DIVERGENCES` applies here exactly as it does hermetically,
        // and for a sharper reason: this mode compares live Solr against
        // *captured Solr*, so `ping`'s per-run `rid` counter differs from one
        // Solr run to the next. A listed entry failing here is the list being
        // right, not the harness finding a bug.
        let divergence_reason = expected_divergence_reason(&entry.name);

        let (status, actual) = common::diff::fetch_live(&base_url, &entry.path);
        if status != entry.status {
            let msg = format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            );
            match divergence_reason {
                Some(reason) => eprintln!("{msg}\n  (expected divergence: {reason})"),
                None => failures.push(msg),
            }
            continue;
        }

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);

        // Extended (issue #31) so RANKED_RELEVANCE_ENTRIES rows use the
        // ranked+score path here too — previously this loop generic-diffed
        // everything, unlike the hermetic run above.
        let diffs_empty = if RANKED_RELEVANCE_ENTRIES.contains(&entry.name.as_str()) {
            let expected_docs = ranked_docs(&expected_n.value);
            let actual_docs = ranked_docs(&actual_n.value);
            let ranked_report = diff_ranked_ids(&expected_docs, &actual_docs);
            let empty = ranked_report.diffs.is_empty();
            if !empty {
                eprintln!("{}: ranked-id diffs: {:?}", entry.name, ranked_report.diffs);
            }
            empty
        } else {
            let report = diff(&expected_n.value, &actual_n.value);
            let empty = report.diffs.is_empty();
            if !empty {
                eprintln!("{}: {:?}", entry.name, report.diffs);
            }
            empty
        };

        match (diffs_empty, divergence_reason) {
            // Self-expiring in this mode too — for divergences inherent to
            // *Solr itself* (`ping`'s per-run `rid`): an entry that stops
            // diverging must be removed, or the list quietly becomes a lie
            // here while the hermetic run still polices it.
            //
            // This does NOT hold for any `RANKED_RELEVANCE_ENTRIES` member
            // that's also in `EXPECTED_DIVERGENCES`: this loop's "actual"
            // side is a *live re-fetch of Solr itself*, not Wayfinder
            // (`fetch_live` always hits `WAYFINDER_DIFF_SOLR_URL`, Solr's own
            // port) — see the comment above. A Wayfinder-only feature gap
            // trivially "matches" here on every run, since real Solr is
            // deterministic against its own historical capture, regardless of
            // whether the feature has landed. Self-expiry here would misfire
            // unconditionally, so it is skipped for those — the hermetic loop
            // is the one that actually exercises Wayfinder and owns the
            // self-expiry signal for this class of entry.
            //
            // Currently unreachable: `select_term_scored`/`select_quick_scored`
            // (issue #31/#34) used to be the only `RANKED_RELEVANCE_ENTRIES`
            // members with an `EXPECTED_DIVERGENCES` entry, and both are gone
            // from that list now that `fl=score` has landed and their
            // remaining BM25-magnitude divergence is ratified permanently
            // (PRD ratified-divergence 4, `RANKED_SCORE_VALUE_RATIFIED`) —
            // not tracked as a to-do here. The arm stays for the next
            // Wayfinder-feature-gap `RANKED_RELEVANCE_ENTRIES` entry, if one
            // shows up.
            (true, Some(reason)) if RANKED_RELEVANCE_ENTRIES.contains(&entry.name.as_str()) => {
                eprintln!(
                    "{}: matches live Solr (expected — this loop compares Solr against its own \
                     capture, not Wayfinder; self-expiry for {reason} is decided by the \
                     hermetic run)",
                    entry.name
                );
            }
            (true, Some(reason)) => failures.push(format!(
                "{}: EXPECTED_DIVERGENCES says this should still diverge ({reason}), but it \
                 matches live Solr — remove this entry from EXPECTED_DIVERGENCES in \
                 tests/differential.rs",
                entry.name
            )),
            (false, Some(reason)) => eprintln!("{}: (expected divergence: {reason})", entry.name),
            (false, None) => failures.push(format!("{}: differs from live Solr", entry.name)),
            (true, None) => {}
        }
    }

    assert!(
        failures.is_empty(),
        "live Solr differential failures:\n{}",
        failures.join("\n")
    );
}

// --- manifest-errors.tsv wired into the harness (issue #31, item 3) --------
//
// `manifest-errors.tsv` (added by issue #11 for the non-core-relative-GET
// error fixtures) was covered only by `tests/error_shapes.rs`'s unit tests
// until now. This runs EVERY row against an in-process Wayfinder, per-row
// app selection by the URL's leading core segment: `content/...` ->
// `common::indexed_app()`; `facets/...` -> `facets_app()`; `keyorder/...` ->
// `keyorder_app()`; `facets33/...` -> `facets33_app()` (issue #33, post-
// rebase). All four name their core `content`, so the leading segment is
// rewritten to `content` before the request is issued.
//
// `sortdebt/...` (issue #32, post-rebase) is different: `sortdebt_app()`'s
// schema names its core `sortdebt` literally (see the comment on
// `SORTDEBT_SCHEMA_TOML` above), matching the manifest-errors row's own
// path — so it is issued unrewritten against `sortdebt_app`.
//
// A row whose leading segment names none of the above (`nosuchcore/...`,
// `schemaless_probe/...`) is not rewritten at all — that mismatch (a core
// Wayfinder genuinely does not have) is exactly the shape of the
// `err_missing_core`/`update_unknown_field_schemaless` `ACCEPTED_DIVERGENCES`
// rows, so it is issued against the default content app with its literal,
// unrewritten URL, and checked by the narrower rule those two entries define
// rather than the full differ.

/// Selects the app for `entry` by its URL's leading core segment and
/// returns `(app, request_url)`, where `request_url` has that segment
/// rewritten to `content` for every core except `sortdebt`, `stats`,
/// `version99`, `fragsize104`, and `spellcheck_223` (which keep their own
/// names — see the module comment above).
/// An unrecognised segment is returned unrewritten against `content_app`.
#[allow(clippy::too_many_arguments)] // one hermetic app per manifest-errors core
fn app_and_request_url<'a>(
    entry: &ManifestErrorEntry,
    content_app: &'a Router,
    facets_app: &'a Router,
    keyorder_app: &'a Router,
    sortdebt_app: &'a Router,
    facets33_app: &'a Router,
    update9_app: &'a Router,
    stats_app: &'a Router,
    version99_app: &'a Router,
    fragsize_app: &'a Router,
    spellcheck_app: &'a Router,
) -> (&'a Router, String) {
    match entry.url.split_once('/') {
        Some(("content", rest)) => (content_app, format!("content/{rest}")),
        Some(("facets", rest)) => (facets_app, format!("content/{rest}")),
        Some(("keyorder", rest)) => (keyorder_app, format!("content/{rest}")),
        Some(("facets33", rest)) => (facets33_app, format!("content/{rest}")),
        Some(("sortdebt", _)) => (sortdebt_app, entry.url.clone()),
        Some(("update9", _)) => (update9_app, entry.url.clone()),
        Some(("stats", _)) => (stats_app, entry.url.clone()),
        Some(("version99", _)) => (version99_app, entry.url.clone()),
        Some(("fragsize104", _)) => (fragsize_app, entry.url.clone()),
        Some(("spellcheck_223", _)) => (spellcheck_app, entry.url.clone()),
        _ => (content_app, entry.url.clone()),
    }
}

#[tokio::test]
async fn manifest_errors_every_row_runs_against_the_matching_hermetic_app() {
    let entries = load_manifest_errors(&manifest_errors_path());
    let raw = std::fs::read_to_string(manifest_errors_path()).expect("read manifest-errors.tsv");
    let expected_count = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();
    assert_eq!(
        entries.len(),
        expected_count,
        "loader must parse every row of manifest-errors.tsv"
    );
    assert!(!entries.is_empty(), "manifest-errors.tsv must not be empty");

    for (name, reason) in ACCEPTED_DIVERGENCES {
        assert!(
            entries.iter().any(|e| e.name == *name),
            "ACCEPTED_DIVERGENCES entry `{name}` (reason: {reason}) does not match any \
             manifest-errors row — fix the name or remove the stale entry"
        );
    }

    let (content_app, _content_dir) = indexed_app().await;
    let (facets_app, _facets_dir) = facets_app().await;
    let (keyorder_app, _keyorder_dir) = keyorder_app().await;
    let (sortdebt_app, _sortdebt_dir) = sortdebt_app().await;
    let (facets33_app, _facets33_dir) = facets33_app().await;
    let (update9_app, _update9_dir) = update9_app().await;
    let (stats_app, _stats_dir) = stats_app().await;
    let (version99_app, _version99_dir) = version99_app().await;
    let (fragsize_app, _fragsize_dir) = fragsize_app().await;
    let (spellcheck_app, _spellcheck_dir) = spellcheck_223_app().await;

    let mut ran = 0usize;
    let mut diffed = 0usize;
    let mut failures = Vec::new();
    eprintln!("--- manifest-errors differential run ---");
    eprintln!("accepted (permanent, ratified) divergences:");
    for (name, reason) in ACCEPTED_DIVERGENCES {
        eprintln!("  {name}: {reason}");
    }

    for entry in &entries {
        let (app, url) = app_and_request_url(
            entry,
            &content_app,
            &facets_app,
            &keyorder_app,
            &sortdebt_app,
            &facets33_app,
            &update9_app,
            &stats_app,
            &version99_app,
            &fragsize_app,
            &spellcheck_app,
        );
        // `update_select_commitwithin_visible` follows a `commitWithin=500`
        // row with no settle delay in this hermetic replay, unlike
        // `capture.sh`'s 3s sleep before it captured that fixture — a bounded
        // retry (re-request until the diff is empty or ~5s) mirrors that
        // settle sleep deterministically instead of flaking on a race.
        let (status, actual) = if entry.name == "update_select_commitwithin_visible" {
            let start = Instant::now();
            loop {
                let (status, actual) =
                    request_full(app, &entry.method, &url, entry.body.as_deref()).await;
                let expected_n = normalize(fixture(&entry.name));
                let actual_n = normalize(actual.clone());
                let report = diff(&expected_n.value, &actual_n.value);
                if report.diffs.is_empty() || start.elapsed() >= Duration::from_secs(5) {
                    break (status, actual);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        } else {
            request_full(app, &entry.method, &url, entry.body.as_deref()).await
        };

        if let Some(reason) = accepted_divergence_reason(&entry.name) {
            match entry.name.as_str() {
                "err_missing_core" => {
                    // Keep the check honest: the fixture must genuinely be
                    // non-JSON (Solr's 404 HTML easter egg), or this
                    // "accepted" entry could rot into a false excuse.
                    let text = fixture_text(&entry.name);
                    assert!(
                        serde_json::from_str::<Value>(&text).is_err(),
                        "{}: fixture must be non-JSON for this accepted divergence to still \
                         apply ({reason})",
                        entry.name
                    );
                    if status.as_u16() != entry.status {
                        failures.push(format!(
                            "{}: HTTP status {} vs fixture status {} ({reason})",
                            entry.name, status, entry.status
                        ));
                    }
                }
                "update_unknown_field_schemaless" => {
                    assert_eq!(
                        entry.status, 200,
                        "{}: fixture must be 200 for this accepted divergence to still name \
                         the gap ({reason})",
                        entry.name
                    );
                    if status.as_u16() == 200 {
                        failures.push(format!(
                            "{}: Wayfinder answered 200 — the documented schemaless \
                             divergence no longer holds, remove this ACCEPTED_DIVERGENCES \
                             entry and update the PRD ({reason})",
                            entry.name
                        ));
                    }
                }
                "facet_non_docvalues_text"
                | "facet_non_docvalues_text_enum"
                | "facet_stored_only_field" => {
                    if status.as_u16() == entry.status {
                        failures.push(format!(
                            "{}: Wayfinder matched the fixture's status — the documented \
                             unfacetable-field divergence no longer holds, remove this \
                             ACCEPTED_DIVERGENCES entry and update finding 105 ({reason})",
                            entry.name
                        ));
                    }
                }
                "update_unknown_core" | "ping_unknown_core" => {
                    // Same shape as err_missing_core (finding 49): the 404
                    // HTML easter egg is endpoint-agnostic, extending to
                    // POST /update and GET /admin/ping unchanged.
                    let text = fixture_text(&entry.name);
                    assert!(
                        serde_json::from_str::<Value>(&text).is_err(),
                        "{}: fixture must be non-JSON for this accepted divergence to still \
                         apply ({reason})",
                        entry.name
                    );
                    if status.as_u16() != entry.status {
                        failures.push(format!(
                            "{}: HTTP status {} vs fixture status {} ({reason})",
                            entry.name, status, entry.status
                        ));
                    }
                }
                "ping_unknown_core_delete" => {
                    // Solr's fixture here is a Jetty-level 405 with an empty
                    // body, not the 404 easter-egg page — a different shape
                    // from the other two, so this checks Wayfinder's own
                    // method-agnostic JSON 404 rather than the fixture's
                    // status (finding 49: noted, not matched).
                    assert_eq!(
                        entry.status, 405,
                        "{}: fixture must be 405 for this accepted divergence to still name \
                         the gap ({reason})",
                        entry.name
                    );
                    if status.as_u16() != 404 {
                        failures.push(format!(
                            "{}: expected Wayfinder's method-agnostic JSON 404, got {} \
                             ({reason})",
                            entry.name, status
                        ));
                    }
                }
                other => unreachable!(
                    "ACCEPTED_DIVERGENCES entry `{other}` has no matching check arm in this test"
                ),
            }
            // Counted here, past the actual accepted-divergence check above,
            // not as the loop's first statement — see the tautology this
            // guards against in the comment on the final assertion below.
            ran += 1;
            continue;
        }

        if status.as_u16() != entry.status {
            failures.push(format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            ));
            ran += 1;
            continue;
        }

        let expected = fixture(&entry.name);
        let mut expected_n = normalize(expected);
        let mut actual_n = normalize(actual);
        // `update_select_overwrite_false` alone: two live docs share
        // uniqueKey `u7` (a deliberate `overwrite=false` duplicate), and no
        // query field can tie-break them — orchestrator ruling, issue #9,
        // recorded in `docs/solr-ref-findings.md`'s finding-46-49 block:
        // even with no background merges, tantivy 0.26.1's `SegmentRegister`
        // (`src/indexer/segment_register.rs`) holds segments in a
        // `std::collections::HashMap<SegmentId, SegmentEntry>`, so segment
        // ordinals — and therefore `AllScoredHits`'s ascending-`DocAddress`
        // tie-break for two equally-scored docs — are per-process-random,
        // not insertion order. Solr's own captured order for this exact
        // pair is equally a Lucene-internals accident, not a wire contract.
        // Sorting `response.docs` identically on both sides before the real
        // differ runs is the narrowest possible relaxation: every other
        // field (`responseHeader`, `numFound`/`start`, and each doc's full
        // content) is still compared exactly, and this row still counts
        // toward `diffed` below — nothing contractual is hidden, only the
        // doc *sequence* is treated as a set for this one row.
        if entry.name == "update_select_overwrite_false" {
            for v in [&mut expected_n.value, &mut actual_n.value] {
                if let Some(docs) = v
                    .pointer_mut("/response/docs")
                    .and_then(Value::as_array_mut)
                {
                    docs.sort_by_key(ToString::to_string);
                }
            }
        }
        let report = diff(&expected_n.value, &actual_n.value);
        // The differ-bound counter: only rows that actually reach `diff()`
        // count here, so a bug that hollowed out this branch (while leaving
        // `ran` incrementing elsewhere) would still be caught below.
        diffed += 1;
        eprintln!(
            "{}: {} diffs, touched (tolerance-applied) {:?}",
            entry.name,
            report.diffs.len(),
            report.touched
        );
        if !report.diffs.is_empty() {
            eprintln!("  diffs: {:?}", report.diffs);
        }

        match expected_divergence_manifest_errors_reason(&entry.name) {
            Some(reason) if report.diffs.is_empty() => failures.push(format!(
                "{}: EXPECTED_DIVERGENCES_MANIFEST_ERRORS says this should still diverge \
                 ({reason}), but it now matches — the underlying fix has landed, so remove \
                 this entry from EXPECTED_DIVERGENCES_MANIFEST_ERRORS in tests/differential.rs",
                entry.name
            )),
            Some(reason) => eprintln!("  (expected divergence: {reason})"),
            None if !report.diffs.is_empty() => {
                failures.push(format!("{}: {:?}", entry.name, report.diffs))
            }
            None => {}
        }
        ran += 1;
    }

    for (name, reason) in EXPECTED_DIVERGENCES_MANIFEST_ERRORS {
        assert!(
            entries.iter().any(|e| e.name == *name),
            "EXPECTED_DIVERGENCES_MANIFEST_ERRORS entry `{name}` (reason: {reason}) does not \
             match any manifest-errors row — fix the name or remove the stale entry"
        );
    }

    // The weakness to guard against (issue #31): a loader that parses rows
    // but a loop that never executes them would be green and worthless.
    // `ran` is incremented only after each branch's real check has run (not
    // as the loop's first statement), and `diffed` is incremented only where
    // `diff()` is actually called — every non-accepted-divergence row, since
    // `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` entries still go through the
    // real differ, just without failing the suite on a match. A loop
    // hollowed out to just count rows, without doing the checks, would leave
    // `diffed` short of this target even if `ran` alone looked fine.
    assert_eq!(
        ran,
        entries.len(),
        "every manifest-errors row must be exercised, not just loaded"
    );
    assert_eq!(
        diffed,
        entries.len() - ACCEPTED_DIVERGENCES.len(),
        "every non-accepted-divergence manifest-errors row must go through the real differ"
    );
    assert!(
        failures.is_empty(),
        "manifest-errors differential failures:\n{}",
        failures.join("\n")
    );
}

/// Live counterpart, gated by `WAYFINDER_DIFF_SOLR=1` exactly like
/// `live_solr_matches_committed_query_set`. Each row uses its own effective
/// base URL (column 6, defaulting to the canonical `http://localhost:8983/solr`)
/// and method/body. A row whose base URL does not answer a quick
/// reachability probe is a PRINTED, named skip — the per-issue containers on
/// 8984/8985/8986 are not guaranteed to be up — but a row on the default
/// 8983 base must actually run; that base is the canonical container this
/// whole harness depends on.
#[test]
fn live_solr_matches_committed_manifest_errors() {
    if std::env::var("WAYFINDER_DIFF_SOLR").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping live Solr manifest-errors differential: set WAYFINDER_DIFF_SOLR=1 to \
             enable (WAYFINDER_DIFF_SOLR=1 cargo test --test differential)"
        );
        return;
    }

    const DEFAULT_BASE: &str = "http://localhost:8983/solr";

    let entries = load_manifest_errors(&manifest_errors_path());
    let mut failures = Vec::new();
    // Mirrors the hermetic run's anti-vacuity counters (issue #31 review
    // follow-up): `ran`/`diffed` are only incremented past each branch's
    // real check, never as the loop's first statement, so a hollowed-out
    // loop body cannot satisfy the assertions below by construction.
    // `skipped` tracks legitimate, printed skips for an absent per-issue
    // container — those rows are neither `ran` nor a failure.
    let mut ran = 0usize;
    let mut diffed = 0usize;
    let mut skipped = 0usize;
    for entry in &entries {
        let base_url = entry.base_url.as_deref().unwrap_or(DEFAULT_BASE);
        let divergence_reason = accepted_divergence_reason(&entry.name);

        if !live_reachable(base_url) {
            if base_url == DEFAULT_BASE {
                failures.push(format!(
                    "{}: default base {base_url} did not answer a reachability probe — the \
                     canonical container must be up for this row to run",
                    entry.name
                ));
            } else {
                eprintln!(
                    "{}: skipping — {base_url} did not answer a reachability probe (per-issue \
                     container may be absent)",
                    entry.name
                );
                skipped += 1;
            }
            continue;
        }

        if let Some(reason) = divergence_reason {
            // Accepted divergences are checked hermetically above (including
            // `err_missing_core`'s honesty check that the fixture is
            // genuinely non-JSON); live mode here only re-confirms the status
            // code matched. Deliberately a status-only fetch, not
            // `fetch_live_full`: `err_missing_core`'s body is Solr's 404 HTML
            // easter egg, which `fetch_live_full`'s JSON parse would panic on.
            let status =
                fetch_live_status(base_url, &entry.method, &entry.url, entry.body.as_deref());
            if status != entry.status {
                failures.push(format!(
                    "{}: HTTP status {} vs expected {} (accepted divergence: {reason})",
                    entry.name, status, entry.status
                ));
            } else {
                eprintln!("{}: (accepted divergence: {reason})", entry.name);
            }
            ran += 1;
            continue;
        }

        let (status, actual) =
            fetch_live_full(base_url, &entry.method, &entry.url, entry.body.as_deref());

        if status != entry.status {
            failures.push(format!(
                "{}: HTTP status {} vs expected {}",
                entry.name, status, entry.status
            ));
            ran += 1;
            continue;
        }

        // `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` (issue #35's
        // `facet_unknown_field`) is not self-expired here for the same reason
        // `live_solr_matches_committed_query_set` skips it for
        // `RANKED_RELEVANCE_ENTRIES`: this loop's "actual" side is a live
        // re-fetch of Solr itself, not Wayfinder, so a Wayfinder-only gap
        // trivially "matches" on every run regardless of whether the fix has
        // landed. The hermetic run owns that signal.
        let expected_divergence_reason = expected_divergence_manifest_errors_reason(&entry.name);

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);
        let report = diff(&expected_n.value, &actual_n.value);
        diffed += 1;
        match (report.diffs.is_empty(), expected_divergence_reason) {
            (true, Some(reason)) => eprintln!(
                "{}: matches live Solr (expected — this loop compares Solr against its own \
                 capture, not Wayfinder; self-expiry for {reason} is decided by the hermetic \
                 run)",
                entry.name
            ),
            (false, Some(reason)) => eprintln!("{}: (expected divergence: {reason})", entry.name),
            (false, None) => failures.push(format!("{}: {:?}", entry.name, report.diffs)),
            (true, None) => {}
        }
        ran += 1;
    }

    // Same weakness as the hermetic run: a loader that parses rows but a
    // loop that never executes them (or that stops short after a container
    // outage) would be green and worthless. Every row is either `ran`,
    // legitimately `skipped` (a named, printed per-issue-container skip), or
    // a counted failure above — accounted for exactly, not just non-zero.
    // `diffed` covers every `ran` row except the accepted-divergence ones,
    // which deliberately skip the real differ (checked by status only,
    // hermetically checked in full above) — all of which live on the
    // always-reachable default base, so none of them can end up in
    // `skipped`.
    assert_eq!(
        ran + skipped,
        entries.len(),
        "every manifest-errors row must be run or legitimately skipped, not silently dropped"
    );
    assert_eq!(
        diffed,
        ran - ACCEPTED_DIVERGENCES.len(),
        "every non-accepted-divergence row that ran must go through the real differ"
    );
    assert!(
        diffed > 0,
        "the live manifest-errors run must exercise the real differ against at least one row \
         when the canonical 8983 container (required to be reachable) is up"
    );

    assert!(
        failures.is_empty(),
        "live Solr manifest-errors differential failures:\n{}",
        failures.join("\n")
    );
}

// --- manifest-multipart.tsv wired into the harness (issue #258) -----------
//
// A third hermetic runner, alongside the plain-GET `manifest.tsv` run above
// and the JSON-body `manifest-errors.tsv` run just above this comment: every
// row is a multipart/form-data POST to `/update/extract`, built by
// `common::request_multipart` from a file under `solr-ref/extract-inputs/`,
// diffed against the committed `solr-ref/responses/<name>.json` fixture
// through the same `normalize()` plus a second, extraction-specific
// `normalize_extract()` pass (declared and tested for real, not vacuously,
// in the `normalize_extract_*` tests below).

#[test]
fn load_manifest_multipart_parses_every_line_of_the_real_manifest_multipart() {
    let path = manifest_multipart_path();
    let raw = std::fs::read_to_string(&path).expect("read solr-ref/manifest-multipart.tsv");
    let expected_count = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();

    let entries = load_manifest_multipart(&path);

    assert_eq!(
        entries.len(),
        expected_count,
        "loader must parse every non-blank, non-comment line of manifest-multipart.tsv"
    );
    assert!(
        entries.contains(&ManifestMultipartEntry {
            name: "extract_plain_text_xml".to_string(),
            status: 200,
            url: "content/update/extract?extractOnly=true&resource.name=sample.txt&wt=json"
                .to_string(),
            part_name: "file".to_string(),
            input_file: "sample.txt".to_string(),
            mime: String::new(),
        }),
        "loader must parse extract_plain_text_xml with no mime column, got {:?}",
        entries
    );
    assert!(
        entries.contains(&ManifestMultipartEntry {
            name: "extract_declared_charset_text".to_string(),
            status: 200,
            url: "content/update/extract?extractOnly=true&extractFormat=text&resource.name=sample-latin1.txt&wt=json"
                .to_string(),
            part_name: "file".to_string(),
            input_file: "sample-latin1.txt".to_string(),
            mime: "text/plain; charset=ISO-8859-1".to_string(),
        }),
        "loader must parse extract_declared_charset_text's mime column, got {:?}",
        entries
    );
    assert!(
        entries
            .iter()
            .any(|e| e.name == "extract_corrupt_pdf" && e.status == 500),
        "loader must parse the corrupt-pdf 500 row, got {:?}",
        entries
    );
}

/// Proves `normalize_extract` strips exactly the two ratified markers named
/// in `ACCEPTED_DIVERGENCES_MULTIPART` above (`X-Parsed-By` and
/// `shape="rect"`) and nothing else. Written against hand-built `Value`s
/// (not the real fixtures) so it pins the normaliser's own behaviour in
/// isolation from any particular capture.
#[test]
fn normalize_extract_strips_x_parsed_by_and_shape_rect_and_records_what_it_touched() {
    let v = json!({
        "responseHeader": {"status": 0, "QTime": 4},
        "file": "\n<html>\n<meta name=\"X-Parsed-By\" content=\"org.apache.tika.parser.DefaultParser\" />\n<a href=\"x\" shape=\"rect\">x</a>\n</html>\n",
        "file_metadata": ["resourceName", ["a.txt", "a.txt"], "X-Parsed-By", ["org.apache.tika.parser.DefaultParser", "org.apache.tika.parser.csv.TextAndCSVParser"]]
    });

    let n = normalize_extract(v);

    let file = n.value["file"].as_str().expect("file must stay a string");
    assert!(
        !file.contains("X-Parsed-By"),
        "X-Parsed-By meta element must be stripped from file, got {file:?}"
    );
    assert!(
        !file.contains("shape=\"rect\""),
        "shape=\"rect\" must be stripped from file, got {file:?}"
    );
    assert!(
        file.contains("<a href=\"x\">x</a>"),
        "the rest of the <a> element must survive untouched, got {file:?}"
    );

    let metadata = n.value["file_metadata"]
        .as_array()
        .expect("file_metadata must stay an array");
    assert!(
        !metadata.iter().any(|v| v.as_str() == Some("X-Parsed-By")),
        "X-Parsed-By key must be removed from file_metadata, got {metadata:?}"
    );
    assert!(
        metadata.iter().any(|v| v.as_str() == Some("resourceName")),
        "unrelated file_metadata keys must survive, got {metadata:?}"
    );

    assert!(
        n.touched.iter().any(|t| t.contains("X-Parsed-By meta")),
        "touched must record the file meta-element strip, got {:?}",
        n.touched
    );
    assert!(
        n.touched.iter().any(|t| t.contains("shape=\"rect\"")),
        "touched must record the shape=\"rect\" strip, got {:?}",
        n.touched
    );
    assert!(
        n.touched
            .iter()
            .any(|t| t.contains("file_metadata") && t.contains("X-Parsed-By")),
        "touched must record the file_metadata strip, got {:?}",
        n.touched
    );
}

/// The over-normalisation guard the spec explicitly asks for: a real
/// difference in `file` (not one of the two ratified markers) must still be
/// visible to `diff()` after `normalize_extract`, proving the normaliser does
/// not swallow anything beyond what it declares.
#[test]
fn normalize_extract_does_not_hide_a_real_difference_in_file() {
    let expected = json!({"file": "hello world", "file_metadata": []});
    let actual = json!({"file": "goodbye world", "file_metadata": []});

    let expected_n = normalize_extract(expected);
    let actual_n = normalize_extract(actual);

    let report = diff(&expected_n.value, &actual_n.value);
    assert!(
        !report.diffs.is_empty(),
        "a real content difference in file must still be reported after normalize_extract, \
         not silently normalised away"
    );
}

/// A fixture with neither `X-Parsed-By` nor `shape=\"rect\"` anywhere must
/// come back with an empty `touched` list — the normaliser must not fire
/// unconditionally.
#[test]
fn normalize_extract_touches_nothing_when_no_ratified_marker_is_present() {
    let v = json!({"file": "plain content, nothing special", "file_metadata": ["a", ["1", "1"]]});
    let n = normalize_extract(v);
    assert!(
        n.touched.is_empty(),
        "normalize_extract must not touch anything when no ratified marker is present, got {:?}",
        n.touched
    );
}

/// The hermetic runner: every `manifest-multipart.tsv` row, POSTed through
/// the real `/solr/{core}/update/extract` route (issue #258) against an
/// in-process Wayfinder, diffed against its committed fixture.
///
/// Expected to fail today for a structural reason, not a normalisation
/// reason: the route does not exist yet, so every row currently gets a 404
/// (or whatever axum's router produces for an unmatched path) instead of its
/// fixture's captured status.
#[tokio::test]
async fn extract_multipart_manifest_matches_captured_fixtures() {
    let entries = load_manifest_multipart(&manifest_multipart_path());
    assert!(
        !entries.is_empty(),
        "manifest-multipart.tsv must not be empty, or this test is vacuous"
    );

    let (app, _dir) = extract_app().await;
    let mut failures = Vec::new();
    let mut ran = 0usize;

    for entry in &entries {
        let input_path = extract_inputs_dir().join(&entry.input_file);
        let bytes = std::fs::read(&input_path)
            .unwrap_or_else(|e| panic!("read extract input {}: {e}", input_path.display()));

        let (status, actual) = common::request_multipart(
            &app,
            &entry.url,
            &entry.part_name,
            &entry.input_file,
            &entry.mime,
            &bytes,
        )
        .await;

        // A recorded status divergence replaces the captured status as this
        // row's expectation, and is checked *as* a divergence: the captured
        // status must still differ (or the entry is stale), Wayfinder's
        // status must be exactly the recorded one (not merely "some other
        // error"), and the body must still be a well-formed Wayfinder error
        // envelope agreeing with it.
        if let Some((expected_status, reason)) = divergent_status_multipart(&entry.name) {
            if entry.status == expected_status {
                failures.push(format!(
                    "{}: DIVERGENT_STATUS_MULTIPART says Wayfinder answers \
                     {expected_status} where the capture is {}, but they now agree \
                     ({reason}) -- remove this entry",
                    entry.name, entry.status
                ));
                continue;
            }
            if status.as_u16() != expected_status {
                failures.push(format!(
                    "{}: recorded status divergence expects {expected_status}, got {}, \
                     body: {actual}",
                    entry.name,
                    status.as_u16()
                ));
                continue;
            }
            let actual_n = normalize(actual.clone());
            let code = actual_n.value["error"]["code"].as_u64();
            let header = actual_n.value["responseHeader"]["status"].as_u64();
            if code != Some(u64::from(expected_status))
                || header != Some(u64::from(expected_status))
            {
                failures.push(format!(
                    "{}: status-divergent row must still answer Wayfinder's normal error \
                     envelope, got error.code={code:?} responseHeader.status={header:?} \
                     in {actual}",
                    entry.name
                ));
                continue;
            }
            ran += 1;
            continue;
        }

        if status.as_u16() != entry.status {
            failures.push(format!(
                "{}: HTTP status {} vs expected {}, body: {actual}",
                entry.name,
                status.as_u16(),
                entry.status
            ));
            continue;
        }
        ran += 1;

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);

        let divergence_reason = accepted_divergence_multipart_reason(&entry.name);
        if entry.status == 200 {
            // Prove the raw (pre-normalize_extract) envelopes really do
            // differ for every accepted-divergence row, so this is not a
            // vacuous waiver: the divergence must still genuinely exist.
            let raw_report = diff(&expected_n.value, &actual_n.value);
            if let Some(reason) = divergence_reason
                && raw_report.diffs.is_empty()
            {
                failures.push(format!(
                    "{}: ACCEPTED_DIVERGENCES_MULTIPART says this should still diverge \
                     before normalize_extract ({reason}), but the raw envelopes already \
                     match — remove this entry",
                    entry.name
                ));
            }

            let expected_n2 = normalize_extract(expected_n.value.clone());
            let actual_n2 = normalize_extract(actual_n.value.clone());
            let report = diff(&expected_n2.value, &actual_n2.value);
            eprintln!(
                "{}: {} diffs after normalize_extract, touched {:?}",
                entry.name,
                report.diffs.len(),
                actual_n2.touched
            );
            if !report.diffs.is_empty() {
                failures.push(format!(
                    "{}: diffs remain after normalize_extract: {:?}",
                    entry.name, report.diffs
                ));
            }
        } else {
            // The corrupt-pdf 500 envelope has no file/file_metadata fields
            // to normalize_extract; normalize() alone is the same treatment
            // every other error-envelope fixture gets.
            let report = diff(&expected_n.value, &actual_n.value);
            if !report.diffs.is_empty() {
                failures.push(format!("{}: {:?}", entry.name, report.diffs));
            }
        }
    }

    assert_eq!(
        ran,
        entries.len(),
        "every manifest-multipart row must reach its expected status to be diffed at all \
         (a status mismatch above short-circuits the diff for that row)"
    );

    assert!(
        failures.is_empty(),
        "hermetic multipart differential failures against solr-ref fixtures:\n{}",
        failures.join("\n")
    );
}

/// Live counterpart, gated by `WAYFINDER_DIFF_SOLR=1` exactly like
/// `live_solr_matches_committed_query_set` above. Requires
/// `solr-ref/capture.sh`'s #258 block to have been run first (the
/// `extract258` core / port-9020 container).
#[test]
fn live_solr_matches_committed_manifest_multipart() {
    if std::env::var("WAYFINDER_DIFF_SOLR").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping live Solr multipart differential: run solr-ref/capture.sh, then set \
             WAYFINDER_DIFF_SOLR=1 to enable"
        );
        return;
    }

    let base_url = std::env::var("WAYFINDER_DIFF_SOLR_EXTRACT_URL")
        .unwrap_or_else(|_| "http://localhost:9020/solr".to_string());

    if !live_reachable(&base_url) {
        eprintln!(
            "skipping live Solr multipart differential: {base_url} is not reachable (run \
             solr-ref/capture.sh's #258 block first)"
        );
        return;
    }

    let entries = load_manifest_multipart(&manifest_multipart_path());
    let mut failures = Vec::new();

    for entry in &entries {
        let input_path = extract_inputs_dir().join(&entry.input_file);
        let (status, actual) = fetch_live_multipart(
            &base_url,
            &entry.url,
            &entry.part_name,
            &input_path,
            &entry.input_file,
            &entry.mime,
        );

        if status != entry.status {
            failures.push(format!(
                "{}: HTTP status {status} vs expected {}",
                entry.name, entry.status
            ));
            continue;
        }

        let expected = fixture(&entry.name);
        let expected_n = normalize(expected);
        let actual_n = normalize(actual);
        let report = diff(&expected_n.value, &actual_n.value);
        if !report.diffs.is_empty() {
            eprintln!(
                "{}: (comparing live Solr against its own capture) {:?}",
                entry.name, report.diffs
            );
            failures.push(format!(
                "{}: differs from live Solr: {:?}",
                entry.name, report.diffs
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "live Solr multipart differential failures:\n{}",
        failures.join("\n")
    );
}

// --- diagnostic-string hygiene (issue #277) ---------------------------------
//
// A reflow that leaves source indentation *inside* a string literal turns a
// wrapped multi-line diagnostic into one that renders with a run of several
// consecutive spaces mid-sentence. Test A below checks every diagnostic
// constant table at runtime (their reason strings are always constructed,
// whether or not the divergence path is hit). Test B checks the three
// `failures.push(format!(...))` literals in the multipart runner, which are
// inline and only rendered when the differential runner actually fails --
// not hermetically triggerable -- by scanning this file's own source text
// instead.

/// Returns the byte offset of the first run of two or more consecutive ASCII
/// spaces in `s`, or `None` if there is none.
fn find_double_space(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    (0..bytes.len().saturating_sub(1)).find(|&i| bytes[i] == b' ' && bytes[i + 1] == b' ')
}

fn char_boundary_floor(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn char_boundary_ceil(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Appends a failure to `failures` for every reason string in `entries`
/// (already reduced to `(name, reason)` pairs) that contains a run of two or
/// more consecutive spaces, naming the table, the entry, and a window around
/// the offending substring.
fn collect_double_space_reasons(
    table_name: &str,
    entries: &[(&str, &str)],
    failures: &mut Vec<String>,
) {
    for (name, reason) in entries {
        if let Some(pos) = find_double_space(reason) {
            let start = char_boundary_floor(reason, pos.saturating_sub(24));
            let end = char_boundary_ceil(reason, (pos + 24).min(reason.len()));
            failures.push(format!(
                "{table_name} entry `{name}` reason has a run of 2+ consecutive spaces at byte \
                 {pos}: ...{}...",
                &reason[start..end]
            ));
        }
    }
}

/// Test A (broad, runtime): none of the six diagnostic constant tables'
/// reason strings may contain a run of two or more consecutive spaces. A
/// wrapped literal that left its own source indentation inside the string is
/// exactly how this regresses -- see issue #277.
#[test]
fn diagnostic_constant_tables_have_no_double_spaces() {
    let mut failures = Vec::new();

    collect_double_space_reasons(
        "ACCEPTED_DIVERGENCES_MULTIPART",
        &ACCEPTED_DIVERGENCES_MULTIPART
            .iter()
            .map(|(n, r)| (*n, *r))
            .collect::<Vec<_>>(),
        &mut failures,
    );
    collect_double_space_reasons(
        "DIVERGENT_STATUS_MULTIPART",
        &DIVERGENT_STATUS_MULTIPART
            .iter()
            .map(|(n, _, r)| (*n, *r))
            .collect::<Vec<_>>(),
        &mut failures,
    );
    collect_double_space_reasons(
        "ACCEPTED_DIVERGENCES",
        &ACCEPTED_DIVERGENCES
            .iter()
            .map(|(n, r)| (*n, *r))
            .collect::<Vec<_>>(),
        &mut failures,
    );
    collect_double_space_reasons(
        "RANKED_SCORE_VALUE_RATIFIED",
        &RANKED_SCORE_VALUE_RATIFIED
            .iter()
            .map(|(n, r)| (*n, *r))
            .collect::<Vec<_>>(),
        &mut failures,
    );
    collect_double_space_reasons(
        "EXPECTED_DIVERGENCES_MANIFEST_ERRORS",
        &EXPECTED_DIVERGENCES_MANIFEST_ERRORS
            .iter()
            .map(|(n, r)| (*n, *r))
            .collect::<Vec<_>>(),
        &mut failures,
    );
    collect_double_space_reasons(
        "EXPECTED_DIVERGENCES",
        &EXPECTED_DIVERGENCES
            .iter()
            .map(|(n, r)| (*n, *r))
            .collect::<Vec<_>>(),
        &mut failures,
    );

    assert!(
        failures.is_empty(),
        "diagnostic constant table reason strings must be single-spaced (a reflow that left \
         source indentation inside the literal renders as a multi-space mid-sentence run):\n{}",
        failures.join("\n")
    );
}

/// Lines exempt from Test B, keyed by a distinctive **substring of the line**
/// rather than by line number: this file is a documented hot file that every
/// branch appends to, and a line-number table silently slides onto unrelated
/// lines the moment anything above it grows.
///
/// A line is exempt if it contains the key, which also (deliberately) exempts
/// the entry below that spells the key out.
const DOUBLE_SPACE_LINT_EXEMPT_CONTENT: &[(&str, &str)] = &[(
    r#"{"id":"k1","views":5,  "tag":["zebra","apple"]},"#,
    "keyorder_corpus's JSON fixture data lines up the shorter view-count \
     rows with extra padding so the tag column stays aligned -- readability \
     formatting for test data, not a diagnostic string",
)];

/// Returns the byte offset, within `line`, of the first run of two or more
/// consecutive spaces that is *not* the leading indentation of an output
/// line, or `None` if there is none.
///
/// `line` must already have its source indentation stripped. Inside a string
/// literal, a run of spaces that begins immediately after the opening quote
/// or immediately after a `\n` escape is that output line's own leading
/// indentation -- structurally the same thing as source indentation, and the
/// established idiom in this file for a sub-message printed under the
/// preceding `eprintln!`. Everything else, including a run in the *middle* of
/// a string literal, is reported.
fn first_offending_double_space(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    // Byte offset at which an output line's leading indentation may begin.
    let mut indent_starts_at: Option<usize> = None;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => {
                if bytes.get(i + 1) == Some(&b'n') {
                    indent_starts_at = Some(i + 2);
                }
                i += 2;
            }
            b'"' => {
                in_string = !in_string;
                indent_starts_at = if in_string { Some(i + 1) } else { None };
                i += 1;
            }
            b' ' if bytes.get(i + 1) == Some(&b' ') => {
                if indent_starts_at != Some(i) {
                    return Some(i);
                }
                while bytes.get(i) == Some(&b' ') {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Test B (narrow, source text): guards the three inline
/// `failures.push(format!(...))` literals in the multipart status-divergence
/// runner, which Test A cannot reach because they are never constructed by a
/// hermetic pass (triggering them means making the differential runner
/// actually fail). Every non-comment line in this file must have single
/// spaces after its own leading indentation and after the leading
/// indentation of each output line inside a string literal.
///
/// ponytail: this is a source-text lint scoped to this one file, and it reads
/// the source with a one-line-at-a-time scanner rather than a Rust lexer. It
/// would not catch the same defect in a diagnostic literal built by
/// concatenation (e.g. `format!("{a}{b}")` where `a` or `b` supplies the
/// run); it treats a run right after an opening quote or a `\n` escape as
/// deliberate output indentation, so a genuine reflow artefact that lands in
/// exactly that position is missed; and its quote tracking restarts on each
/// line, so it can misjudge string state inside a multi-line raw string.
#[test]
fn source_text_has_no_double_spaces_outside_leading_indentation() {
    let source = include_str!("differential.rs");

    let mut failures = Vec::new();
    for (zero_based, line) in source.lines().enumerate() {
        let line_no = zero_based + 1;
        let trimmed = line.trim_start_matches(' ');
        if trimmed.is_empty() || trimmed.starts_with("//") {
            // Leading indentation is exempt by construction (trimmed already
            // has it stripped); comment lines are exempt too, since this
            // lint targets diagnostic strings that actually get printed, not
            // comment formatting (including the module doc's markdown
            // bullet continuations, which indent under the bullet text using
            // the same convention).
            continue;
        }
        if DOUBLE_SPACE_LINT_EXEMPT_CONTENT
            .iter()
            .any(|(key, _)| trimmed.contains(key))
        {
            continue;
        }
        if let Some(pos) = first_offending_double_space(trimmed) {
            failures.push(format!(
                "line {line_no} has a run of 2+ consecutive spaces at column {pos} of its \
                 trimmed content (i.e. outside leading indentation): {trimmed:?}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "tests/differential.rs has lines with an embedded multi-space run outside their \
         leading indentation -- a wrapped format!/push string literal likely carries source \
         indentation mid-sentence (see issue #277); if a new occurrence is a legitimate \
         construct, add a narrowly scoped, named entry to DOUBLE_SPACE_LINT_EXEMPT_CONTENT \
         instead of widening this check:\n{}",
        failures.join("\n")
    );
}
