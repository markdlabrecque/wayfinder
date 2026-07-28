//! Wayfinder: a Solr-wire-compatible search server on top of Tantivy.
//!
//! Grown from the tracer bullet (PRD §7) — one thin vertical slice through
//! every layer, kept and iterated on rather than a spike: TOML schema ->
//! Tantivy schema, `/update` (JSON add + commit), `/select` (`q`, `fq`,
//! `fl`, `rows`, `start`, and the `facet.*` family — see `crate::facet`),
//! and `/admin/ping`.
//!
//! `sort` was out of the tracer-bullet scope and has since landed (issue #2).
//! Deliberately out of scope here (PRD §7): highlighting, edismax, stats,
//! MLT. Multi-core: out of scope too — `app()` serves exactly one
//! core, matching PRD open question 1's "single-core-per-process" lean.

mod collector;
mod config;
mod core_index;
mod error;
mod facet;
mod params;
pub mod schema;

pub use config::ServerConfig;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as AxPath, RawQuery, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde_json::{Value, json};
use tantivy::query::{EmptyQuery, Occur, Query, QueryClone};

use collector::{SortClause, SortKey};
use core_index::CoreIndex;
use error::{Envelope, WfError};
use params::Params;

struct AppState {
    core_name: String,
    index: CoreIndex,
    config: ServerConfig,
}

/// Request params Wayfinder implements today. Only consulted when
/// `strict_params` is on — by default unknown params are ignored, as Solr does
/// (findings fact 8).
///
/// **Implementing a new param? Add it here.** Otherwise `strict_params = true`
/// will 400 on a param Wayfinder actually supports. `sort` is fully implemented
/// as of #2 — validated by #11, ordered by #2. The `facet.*` family landed with
/// #3; still absent from it, and so still unlisted: `facet.method`,
/// `facet.prefix`, `facet.pivot`, interval and heatmap faceting,
/// `facet.range.other` / `.include` / `.hardend`, and `f.<field>.facet.*`
/// per-field overrides. Also still missing, waiting on their issues:
/// `commitWithin` / `overwrite` / `softCommit` (#9).
const SELECT_PARAMS: &[&str] = &[
    "q",
    "df",
    "fq",
    "fl",
    "rows",
    "start",
    "facet",
    "facet.field",
    "facet.query",
    "facet.limit",
    "facet.mincount",
    "facet.sort",
    "facet.missing",
    "facet.range",
    "facet.range.start",
    "facet.range.end",
    "facet.range.gap",
    "json.nl",
    "sort",
    "wt",
];
const UPDATE_PARAMS: &[&str] = &["commit", "wt"];
const PING_PARAMS: &[&str] = &["wt"];

/// Builds the Wayfinder HTTP app for a single core with all server-config
/// defaults (PRD §6). Use `app_with_config` to supply a config file.
pub fn app(schema_path: &Path, data_dir: &Path) -> anyhow::Result<Router> {
    build(schema_path, data_dir, ServerConfig::default())
}

/// As `app`, with the server config read from `config_path`. A missing file
/// means all defaults; unknown keys in a present file are an error.
pub fn app_with_config(
    schema_path: &Path,
    data_dir: &Path,
    config_path: &Path,
) -> anyhow::Result<Router> {
    let config = ServerConfig::load(config_path)?;
    build(schema_path, data_dir, config)
}

fn build(schema_path: &Path, data_dir: &Path, config: ServerConfig) -> anyhow::Result<Router> {
    let index = CoreIndex::open(schema_path, data_dir, &config)?;
    let core_name = index.wf_schema.core.name.clone();
    let state = Arc::new(AppState {
        core_name,
        index,
        config,
    });

    // `any`, not `get`/`post`: Solr's request handlers are method-agnostic —
    // `err_select_delete.json` shows DELETE /select served as a normal query,
    // so a 405 from the router would be a divergence. `/update` does reject
    // some methods (`err_update_put.json`), which it does itself, with Solr's
    // envelope for it.
    Ok(Router::new()
        .route("/solr/{core}/update", any(update))
        .route("/solr/{core}/select", any(select))
        .route("/solr/{core}/admin/ping", any(ping))
        .with_state(state))
}

/// Verifies the request's `{core}` path segment matches the core this app
/// serves. Not part of the tracer-bullet scope (single core per process,
/// PRD open question 1) beyond this sanity check.
///
/// Documented divergence: `err_missing_core.json` shows Solr answering an
/// unknown core with a 404 *HTML* page, not a JSON envelope. Wayfinder matches
/// the status code and returns its normal JSON error, which is what clients
/// actually parse.
///
/// `envelope` differs per endpoint: `/select` echoes params, `/update` does not.
fn check_core(
    state: &AppState,
    core: &str,
    params: &Params,
    envelope: Envelope,
) -> Result<(), WfError> {
    if core != state.core_name {
        return Err(WfError::new(
            StatusCode::NOT_FOUND,
            "wayfinder::UnknownCore",
            format!("unknown core `{core}`"),
        )
        .with_params(params)
        .envelope(envelope));
    }
    Ok(())
}

/// Rejects methods Solr rejects on `/update`, with the bare no-`responseHeader`
/// envelope Solr uses for it (`err_update_put.json`).
fn check_update_method(method: &Method) -> Result<(), WfError> {
    if method != Method::POST {
        return Err(WfError::bad_request(
            "wayfinder::UnsupportedMethod",
            format!("Unsupported method: {method} for request /update"),
        )
        .envelope(Envelope::Bare));
    }
    Ok(())
}

/// Parses and validates the `sort` parameter, returning the clauses to order by
/// (empty means "no sort", i.e. Solr's default `score desc`).
///
/// Three hard 400s, all captured:
///
/// - Sorting on an undefined field.
/// - Sorting on a field that is not `fast`: a hard 400 in Solr (finding 11,
///   `err_bad_sort.json`), never a silent fallback.
/// - A clause whose direction token is missing or is not `asc`/`desc`
///   (`err_sort_no_direction.json`, `err_sort_bad_direction.json`).
///
/// The *order* of those checks is itself captured behaviour (finding 18), and it
/// has two independent halves, each with its own fixture — they are separate
/// claims and only one fixture each can establish them:
///
/// - **Across clauses:** left to right, stopping at the first bad clause, so one
///   bad clause rejects the whole spec rather than sorting on the valid prefix
///   (`err_sort_bad_clause_among_good.json`, and
///   `err_sort_field_before_direction.json` where an earlier clause's field error
///   beats a later clause's direction error).
/// - **Within a clause:** the direction is checked **before** the field is
///   resolved. Only `err_sort_direction_before_field.json` shows this —
///   `sort=body sideways` is bad in both ways at once, and Solr answers the
///   direction error. Every other captured spec is identical under either
///   within-clause order, so nothing else can be cited for it.
///
/// `score` is special-cased out of field resolution entirely. Note what
/// establishes what: `err_sort_score_bad_direction.json` shows only that `score`
/// is **not exempt from the direction check** — under direction-first, a bad
/// direction errors whether or not `score` is special-cased, so that fixture
/// says nothing about resolution. The special-casing itself is established by
/// `select_sort_score_{all,asc,desc}` returning 200 and ranking by score, which
/// an unresolvable field could not do.
fn check_sort(state: &AppState, params: &Params) -> Result<Vec<SortClause>, WfError> {
    let Some(sort) = params.get("sort") else {
        return Ok(Vec::new());
    };

    let mut clauses = Vec::new();
    let mut offset = 0usize;
    for raw in sort.split(',') {
        let clause_start = offset;
        offset += raw.len() + 1; // +1 for the comma that `split` consumed
        let clause = raw.trim();
        if clause.is_empty() {
            continue;
        }
        let mut tokens = clause.split_whitespace();
        let field_name = tokens.next().unwrap_or(clause);

        // Direction first, field second. `err_sort_direction_before_field.json`
        // (`sort=body sideways` -> the direction error, not the field error) is
        // the only captured spec that separates the two within-clause orders.
        let descending = match tokens.next() {
            Some("asc") => false,
            Some("desc") => true,
            // `pos` mirrors Solr's parser position — the offset just past this
            // clause's field name: `pos=2` for `'id sideways'`
            // (`err_sort_bad_direction.json`), `pos=5` for `'score sideways'`
            // (`err_sort_score_bad_direction.json`), `pos=4` for
            // `'body sideways'` (`err_sort_direction_before_field.json`). Three
            // different field-name lengths, so a constant offset is ruled out.
            //
            // INFERRED, not captured: that the offset is absolute within the
            // whole spec rather than relative to the clause. Every captured
            // fixture has `clause_start == 0`, so the two are indistinguishable.
            // Reaching the difference needs a spec whose earlier clauses are
            // fully valid (`sort=id asc,id sideways` — Wayfinder emits `pos=9`);
            // uncaptured. Contained risk: `error.msg` is outside the
            // compatibility contract (finding 10).
            _ => {
                let pos = clause_start + (raw.len() - raw.trim_start().len()) + field_name.len();
                return Err(WfError::bad_request(
                    "wayfinder::BadSort",
                    format!(
                        "Can't determine a Sort Order (asc or desc) in sort spec '{sort}', pos={pos}"
                    ),
                )
                .with_params(params));
            }
        };

        let key = if field_name == "score" {
            SortKey::Score
        } else {
            let field = state
                .index
                .wf_schema
                .fields
                .iter()
                .find(|f| f.name == field_name);
            match field {
                None => {
                    return Err(WfError::bad_request(
                        "wayfinder::BadSort",
                        format!("can not sort on undefined field: {field_name}"),
                    )
                    .with_params(params));
                }
                Some(f) if !f.fast => {
                    return Err(WfError::bad_request(
                        "wayfinder::BadSort",
                        format!(
                            "can not sort on a field w/o fast values (docValues): {field_name}"
                        ),
                    )
                    .with_params(params));
                }
                Some(f) => SortKey::Field(f.name.clone()),
            }
        };
        clauses.push(SortClause::new(key, descending));
    }
    Ok(clauses)
}

/// Under `strict_params`, rejects the first request param Wayfinder does not
/// implement — a development aid for finding gaps, off by default because Solr
/// serves such requests normally and rejecting them would break real clients.
fn check_params(state: &AppState, allowed: &[&str], params: &Params) -> Result<(), WfError> {
    if !state.config.strict_params {
        return Ok(());
    }
    match params.keys().find(|key| !allowed.contains(key)) {
        None => Ok(()),
        Some(unknown) => Err(WfError::bad_request(
            "wayfinder::UnknownParam",
            format!("unknown request parameter `{unknown}` (strict_params is on)"),
        )
        .with_params(params)),
    }
}

async fn ping(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, PING_PARAMS, &params)?;
    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
            "params": params.echo(),
        },
        "status": "OK",
    });
    Ok(axum::Json(body).into_response())
}

async fn update(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    method: Method,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Result<Response, WfError> {
    check_update_method(&method)?;
    let params = Params::parse(query.as_deref().unwrap_or(""));
    // `/update` errors carry a responseHeader but no params echo — Solr does
    // not echo params on this endpoint (`err_update_bad_json.json`).
    let update_err = |class: &'static str, msg: String| {
        WfError::bad_request(class, msg).envelope(Envelope::NoParams)
    };
    check_core(&state, &core, &params, Envelope::NoParams)?;
    check_params(&state, UPDATE_PARAMS, &params).map_err(|e| e.envelope(Envelope::NoParams))?;

    let docs: Value = serde_json::from_slice(&body).map_err(|e| {
        update_err(
            "wayfinder::BadUpdateBody",
            format!("invalid JSON body: {e}"),
        )
    })?;
    let docs = docs.as_array().cloned().ok_or_else(|| {
        update_err(
            "wayfinder::BadUpdateBody",
            "update body must be a JSON array of documents".to_string(),
        )
    })?;

    state
        .index
        .add_documents(&docs)
        .map_err(|e| update_err("wayfinder::IndexError", e.to_string()))?;

    if params.get("commit") == Some("true") {
        state.index.commit().map_err(|e| {
            WfError::internal("wayfinder::CommitError", e.to_string()).envelope(Envelope::NoParams)
        })?;
    }

    Ok(axum::Json(json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        }
    }))
    .into_response())
}

async fn select(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, SELECT_PARAMS, &params)?;
    let sort = check_sort(&state, &params)?;

    let default_field = params
        .get("df")
        .unwrap_or(&state.index.wf_schema.core.default_field)
        .to_string();

    // No `q` matches nothing — it does *not* default to `*:*`. Solr answers 200
    // with an empty result set (`err_missing_q.json`), which resolves
    // tracer-bullet review follow-up 2 against the fixture.
    let parsed = match params.get("q") {
        None => None,
        Some(q) => {
            let query = state.index.parse_query(q, &default_field).map_err(|e| {
                WfError::bad_request("wayfinder::SyntaxError", e.to_string()).with_params(&params)
            })?;

            let mut filter_queries = Vec::new();
            for fq in params.get_all("fq") {
                filter_queries.push(state.index.parse_query(fq, &default_field).map_err(|e| {
                    WfError::bad_request("wayfinder::SyntaxError", e.to_string())
                        .with_params(&params)
                })?);
            }
            Some((query, filter_queries))
        }
    };

    let hits = match &parsed {
        None => Vec::new(),
        Some((query, filter_queries)) => state
            .index
            .search(query.as_ref(), filter_queries, &sort)
            .map_err(|e| {
                WfError::internal("wayfinder::SearchError", e.to_string()).with_params(&params)
            })?,
    };

    let num_found = hits.len();
    let start: usize = params
        .get("start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // `rows_limit` is a Wayfinder cap with no Solr equivalent, so an
    // over-limit request is clamped rather than rejected — a clamp keeps a
    // client that asks for too much working, a 400 breaks it.
    let rows: usize = params
        .get("rows")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .min(state.config.query.rows_limit);

    let fl: Option<Vec<String>> = params
        .get("fl")
        .map(|fl| fl.split(',').map(|s| s.trim().to_string()).collect());

    let page = hits
        .iter()
        .skip(start)
        .take(rows)
        .copied()
        .collect::<Vec<_>>();

    let mut docs = Vec::with_capacity(page.len());
    for (_, addr) in page {
        docs.push(state.index.render_doc(addr, fl.as_deref()).map_err(|e| {
            WfError::internal("wayfinder::DocError", e.to_string()).with_params(&params)
        })?);
    }

    let mut body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
            "params": params.echo(),
        },
        "response": {
            "numFound": num_found,
            "start": start,
            "numFoundExact": true,
            "docs": docs,
        }
    });

    // `facet=true` gates the whole block; `facet.field` alone does not turn
    // faceting on and the key stays absent (findings fact 4).
    if params.get("facet") == Some("true") {
        // Facet counts are aggregated over a *real* query (`q` AND every `fq`),
        // not over `hits`: Solr enumerates the field's whole term dictionary,
        // which the hit list cannot see. `search` filters post-hoc with
        // `retain`, so the Boolean query is rebuilt here rather than reused.
        let base: facet::BaseClauses = match &parsed {
            // No `q` matches nothing, so neither does any facet — but the term
            // dictionary is still enumerated, at 0, exactly as `facet_zero`
            // shows for a `q` that matches nothing.
            None => vec![(Occur::Must, Box::new(EmptyQuery) as Box<dyn Query>)],
            Some((query, filter_queries)) => std::iter::once((Occur::Must, query.box_clone()))
                .chain(
                    filter_queries
                        .iter()
                        .map(|fq| (Occur::Must, fq.box_clone())),
                )
                .collect(),
        };
        let (facet_counts, warnings) =
            facet::facet_counts(&state.index, &state.config, &params, &default_field, &base)
                .map_err(|e| {
                    WfError::bad_request("wayfinder::FacetError", e.to_string())
                        .with_params(&params)
                })?;
        body["facet_counts"] = facet_counts;
        // `responseHeader.warnings` is absent unless there is something to warn
        // about (every fixture that isn't a Points-based `facet.field` at
        // mincount 0 lacks the key) — never an empty array.
        if !warnings.is_empty() {
            body["responseHeader"]["warnings"] = json!(warnings);
        }
    }

    Ok(axum::Json(body).into_response())
}
