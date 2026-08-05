//! Expiring guard for the `q.op` / `qt` descope (#356, PRD §5
//! "`solr_document` datasource — out of Wayfinder's world").
//!
//! `search_api_solr` 4.4.0 emits `q.op` and `qt` ONLY on the `solr_document`
//! datasource path — the datasource for indexing/searching documents in a
//! **foreign Solr core that Drupal does not own**
//! (`SearchApiSolrBackend.php:1808-1830`, finding 190). The query builder
//! branches on `Utility::hasIndexJustSolrDocumentDatasource($index)`: a normal
//! Drupal-owned datasource gets the index/site filter, and only the `else` —
//! the index is *just* `solr_document` — emits `addParam('qt', ...)` (1814) and
//! `addParam('q.op', 'OR')` unless already set (1828).
//!
//! A Wayfinder core is Drupal-owned by construction: #301 settled one core per
//! site as the supported topology (PR #323; the server serves a single core per
//! process, PRD open question 1). The `solr_document` / `SolrMultisiteDocument`
//! datasources therefore have no Wayfinder to point at, the two params that only
//! that path emits never reach a request Wayfinder serves, and so they stay
//! absent from `SELECT_PARAMS` and 400 under `strict_params = true`. Admitting
//! them would be a wrong half-measure either way: `qt` is meaningless for a
//! server with one select handler, and `q.op` is not inert (real OR/AND
//! default-operator semantics), so admitting it unimplemented would be a
//! silently wrong answer — and there is no served client to implement it for.
//!
//! Per CLAUDE.md's rule for deliberate skips, this guard must fail the day the
//! evidence stops holding. When it goes red, the fix is **not** to weaken it —
//! it is to revisit PRD §5's `solr_document` decision (#356) with the new
//! evidence: if the params stop being confined to the `solr_document` branch,
//! or a captured trace sends one, decide whether to admit `q.op` (with real
//! operator semantics) and `qt`, then delete this guard.
//!
//! Three evidence channels, mirroring `tests/version_write_descope_guard.rs`
//! and `tests/hl353_regex_descope_guard.rs`, plus an executable assertion that
//! the params still 400 under `strict_params`:
//!
//!   1. The vendored `search_api_solr` 4.4.0 source still confines both params
//!      to the `solr_document` branch, and the only other `q.op` occurrence is
//!      still dead example code.
//!   2. No captured client request across the 28 committed traces in
//!      `solr-ref/search-api/trace/` sends `q.op` or `qt`.
//!   3. PRD §5 records the descope.
//!   4. `q.op` and `qt` still 400 under `strict_params = true` — they have not
//!      been silently added to `SELECT_PARAMS`.

// The dead-code allow for partially-used shared helpers is an inner attribute
// inside `tests/common/mod.rs`; repeating it here is a clippy error under
// `-D warnings`.
mod common;

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use serde_json::Value;
use tempfile::TempDir;

use common::{SCHEMA_TOML, corpus, get, post_docs};

const SOURCE: &str = include_str!(
    "../coverage/search_api_solr_4.4.0_source/src/Plugin/search_api/backend/SearchApiSolrBackend.php"
);
const TRACE_DIR: &str = "solr-ref/search-api/trace";
const PRD: &str = include_str!("../docs/PRD.md");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// --- source channel: both params stay confined to the solr_document branch --

/// The branch gate. The whole descope rests on `q.op`/`qt` living inside the
/// `else` of `if (!Utility::hasIndexJustSolrDocumentDatasource($index))` — i.e.
/// only when the index is just the `solr_document` datasource. If this gate
/// moves or is restructured, re-derive where the params are emitted before
/// trusting the rest of this guard.
#[test]
fn source_still_branches_on_has_index_just_solr_document_datasource() {
    assert!(
        SOURCE.contains("if (!Utility::hasIndexJustSolrDocumentDatasource($index))"),
        "the `solr_document` branch gate left the vendored source. PRD §5's #356 descope rests on \
         `q.op`/`qt` being emitted only inside the `else` of \
         `if (!Utility::hasIndexJustSolrDocumentDatasource($index))`; if that gate moved, \
         re-derive where the params are emitted before trusting the rest of this guard."
    );
}

/// The substring from the branch gate up to the next statement — the window
/// that contains the live `qt` and `q.op` emissions. Ends at
/// `$search_api_language_ids`, the first statement after the if/else closes.
fn solr_document_block() -> &'static str {
    let start = SOURCE
        .find("if (!Utility::hasIndexJustSolrDocumentDatasource($index))")
        .expect("gate must be present (see source_still_branches_on_...)");
    let rest = &SOURCE[start..];
    let end = rest
        .find("$search_api_language_ids")
        .expect("the post-block statement must still follow the if/else");
    &rest[..end]
}

/// Both live emissions stay inside the `solr_document` branch. If either
/// `addParam` moves out of this window, it is no longer confined to the
/// foreign-core datasource path and the descope premise moved.
#[test]
fn source_emits_both_params_only_inside_the_solr_document_branch() {
    let block = solr_document_block();
    assert!(
        block.contains("addParam('qt', $config['request_handler'])"),
        "the live `qt` emission is no longer inside the `solr_document` branch. PRD §5's #356 \
         descope rests on `qt` being emitted only on that path; if it moved, revisit the descope."
    );
    assert!(
        block.contains("addParam('q.op', 'OR')"),
        "the live `q.op` emission is no longer inside the `solr_document` branch. PRD §5's #356 \
         descope rests on `q.op` being emitted only on that path; if it moved, revisit the descope."
    );
}

/// `qt` is emitted exactly once in the whole source — the single live call in
/// the `solr_document` branch (the dead example block emits `q.op`, not `qt`).
/// A second `qt` anywhere would be a new live path this descope does not cover.
#[test]
fn source_emits_qt_exactly_once() {
    let count = SOURCE.matches("addParam('qt'").count();
    assert_eq!(
        count, 1,
        "the source now emits `qt` via addParam {count} time(s). PRD §5's #356 descope records \
         exactly one live `qt` emission (inside the `solr_document` branch); a second one is a new \
         path the descope does not cover — revisit #356, do not weaken this guard."
    );
}

/// `q.op` is emitted exactly twice: once live in the `solr_document` branch,
/// once inside the dead `/* We keep this as an example. */` block. A third
/// occurrence would be a new live path; the dead example being removed would
/// drop this to one — both are signals to re-derive, not to relax the count.
#[test]
fn source_emits_q_op_exactly_twice_live_plus_dead_example() {
    let count = SOURCE.matches("addParam('q.op'").count();
    assert_eq!(
        count, 2,
        "the source now emits `q.op` via addParam {count} time(s). PRD §5's #356 descope records \
         exactly two: one live (the `solr_document` branch) and one inside the dead \
         `/* We keep this as an example. */` block. A different count means the premise moved — \
         re-derive whether `q.op` is still confined to the `solr_document` path, then update this \
         guard rather than relaxing it blindly."
    );
}

/// The second `q.op` occurrence is dead code, not a live path. The example
/// block opens with `/* We keep this as an example.` and closes at the next
/// `*/`; the `addParam('q.op'` inside it must stay inside that comment. The day
/// the marker is gone (the block was uncommented or restructured), `q.op` may
/// have become live traffic — revisit the descope.
#[test]
fn source_second_q_op_is_still_dead_example_code() {
    let start = SOURCE
        .find("/* We keep this as an example.")
        .expect("the dead `q.op` example block must still be present");
    let rest = &SOURCE[start..];
    let end = rest
        .find("*/")
        .expect("the dead example block must still close with `*/`");
    let block = &rest[..end];
    assert!(
        block.contains("addParam('q.op'"),
        "the `q.op` inside `/* We keep this as an example. ... */` is no longer there, or the \
         block was uncommented. If it became live, `q.op` is no longer confined to the \
         `solr_document` path — revisit PRD §5's #356 descope."
    );
}

// --- trace channel: no captured request sends q.op or qt ------------------

fn trace_files() -> Vec<PathBuf> {
    let dir = root().join(TRACE_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

fn load(path: &Path) -> Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn trace_corpus_is_the_28_traces_the_decision_was_checked_against() {
    let count = trace_files().len();
    assert_eq!(
        count, 28,
        "the #356 descope was checked against exactly 28 committed traces in \
         solr-ref/search-api/trace/; the corpus now has {count}. If new traces were added, re-check \
         every `q.op`/`qt` needle against their request sides before treating this guard as still \
         valid — see issue #356."
    );
}

/// No captured client request — query string, headers, or body — sends `q.op`
/// or `qt`. The capture site used a Drupal-owned datasource (the normal path),
/// so neither param is ever emitted; this guard trips the day a trace does.
#[test]
fn no_trace_request_sends_q_op_or_qt() {
    for file in trace_files() {
        let capture = load(&file);
        // Serialize the whole request object so the scan covers the path
        // (query string), headers, and a JSON/form body uniformly.
        let request = serde_json::to_string(&capture["request"])
            .unwrap_or_else(|e| panic!("{}: serialize request: {e}", file.display()));
        assert!(
            !request.contains("q.op"),
            "{}: a captured request now sends `q.op`. PRD §5's #356 descope rests on the \
             `solr_document` datasource being out of Wayfinder's world, so no served request \
             carries it — that premise no longer holds and the descope must be revisited.",
            file.display()
        );
        // `qt` as a Solr request param, not a substring of some other value:
        // match the bare param name bordered by a query separator on either
        // side (`?qt=`, `&qt=`, or `\"qt\"` in a JSON body).
        let sends_qt =
            request.contains("?qt=") || request.contains("&qt=") || request.contains("\"qt\"");
        assert!(
            !sends_qt,
            "{}: a captured request now sends the `qt` param. PRD §5's #356 descope rests on the \
             `solr_document` datasource being out of Wayfinder's world, so no served request \
             carries it — that premise no longer holds and the descope must be revisited.",
            file.display()
        );
    }
}

/// Positive control for the request-side scan: the corpus DOES carry real
/// `/select` traffic (`q=`), so the "no `q.op`/`qt`" claim is a real asymmetry
/// rather than a corpus that simply has no select requests at all.
#[test]
fn traces_do_carry_select_traffic_so_the_absence_is_real() {
    let mut seen = false;
    for file in trace_files() {
        let capture = load(&file);
        let request = serde_json::to_string(&capture["request"]).unwrap_or_default();
        if request.contains("q=") {
            seen = true;
            break;
        }
    }
    assert!(
        seen,
        "no trace carries any `q=` select traffic. The `q.op`/`qt` absence claim is now blind — \
         pick a new positive control or revisit the guard."
    );
}

// --- PRD channel: the descope is recorded --------------------------------

/// The whole §5 `solr_document` subsection, from its heading up to (but not
/// including) the parity-roadmap heading that follows it.
fn solr_document_section() -> &'static str {
    let start = PRD
        .find("### `solr_document` datasource — out of Wayfinder's world")
        .expect("PRD must still contain the `solr_document` descope subsection");
    let rest = &PRD[start..];
    let end = rest
        .find("### Solr 9.x parity roadmap")
        .expect("the parity-roadmap heading must still follow the `solr_document` subsection");
    &rest[..end]
}

#[test]
fn prd_records_the_descope_and_its_decision_issue() {
    let section = solr_document_section();
    assert!(
        section.contains("#356"),
        "PRD §5's `solr_document` subsection should reference issue #356 so a future reader can \
         find the decision and its evidence (finding 190)."
    );
    assert!(
        section.contains("#301"),
        "PRD §5's `solr_document` subsection should reference issue #301 (one core per site), \
         which is what puts the `solr_document` datasource out of Wayfinder's world."
    );
    assert!(
        section.contains("SELECT_PARAMS"),
        "PRD §5's `solr_document` subsection should state that `q.op`/`qt` stay absent from \
         `SELECT_PARAMS`."
    );
}

// --- strict_params: q.op and qt still 400 --------------------------------

/// Builds an app with `strict_params = true`, indexed with the shared 5-doc
/// corpus. Separate from the common helpers because strictness is a server-wide
/// config, not toggleable per request — the same shape
/// `tests/grouping.rs`'s `strict_grouping_app` uses.
async fn strict_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, SCHEMA_TOML).expect("write schema.toml");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, "strict_params = true\n").expect("write wayfinder.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app =
        wayfinder::app_with_config(&schema_path, &data_dir, &config_path).expect("app must build");
    let (status, body) = post_docs(&app, &corpus()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "indexing the shared corpus must succeed, got {body}"
    );
    (app, dir)
}

/// `q.op` is unimplemented and must 400 under `strict_params`, not be silently
/// accepted. This is the executable half of the descope: a later change that
/// adds `q.op` to `SELECT_PARAMS` without implementing the operator would turn
/// a loud 400 into a silently wrong (or inert) answer, and this test catches it.
#[tokio::test]
async fn q_op_is_rejected_under_strict_params() {
    let (app, _dir) = strict_app().await;
    let (status, body) = get(&app, "select?q=*:*&q.op=AND&fl=id&wt=json").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "q.op is unimplemented (out of Wayfinder's world per #356) and must 400 under \
         strict_params, not be silently accepted, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("q.op"),
        "the rejection should name q.op, got {msg:?}"
    );
}

/// Same for `qt`.
#[tokio::test]
async fn qt_is_rejected_under_strict_params() {
    let (app, _dir) = strict_app().await;
    let (status, body) = get(&app, "select?q=*:*&qt=standard&fl=id&wt=json").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "qt is unimplemented (out of Wayfinder's world per #356) and must 400 under \
         strict_params, not be silently accepted, got {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("qt"),
        "the rejection should name qt, got {msg:?}"
    );
}
