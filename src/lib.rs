//! Wayfinder: a Solr-wire-compatible search server on top of Tantivy.
//!
//! This is the tracer bullet (PRD §7) — one thin vertical slice through
//! every layer, kept and iterated on rather than a spike: TOML schema ->
//! Tantivy schema, `/update` (JSON add + commit), `/select` (`q`, `fq`,
//! `fl`, `rows`, `start`, one `facet.field`), and `/admin/ping`.
//!
//! Deliberately out of scope here (PRD §7): highlighting, edismax, stats,
//! MLT, sort. Multi-core: out of scope too — `app()` serves exactly one
//! core, matching PRD open question 1's "single-core-per-process" lean.

mod collector;
mod config;
mod core_index;
mod error;
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
/// will 400 on a param Wayfinder actually supports. `sort` is listed because
/// #11 validates it (a non-`fast` field is a 400); ordering itself lands with
/// #2. Still missing, waiting on their issues: the rest of the `facet.*` family
/// (#3), `commitWithin` / `overwrite` / `softCommit` (#9).
const SELECT_PARAMS: &[&str] = &[
    "q",
    "df",
    "fq",
    "fl",
    "rows",
    "start",
    "facet",
    "facet.field",
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

/// Validates the `sort` parameter's *fields* without implementing ordering.
///
/// Sorting on a field that is not `fast` is a hard 400 in Solr (finding 11,
/// `err_bad_sort.json`), never a silent fallback — that error shape is in scope
/// here. Actually ordering the results is issue #2; until it lands a valid
/// `sort` is accepted and ignored.
fn check_sort(state: &AppState, params: &Params) -> Result<(), WfError> {
    let Some(sort) = params.get("sort") else {
        return Ok(());
    };
    for clause in sort.split(',') {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }
        let field_name = clause.split_whitespace().next().unwrap_or(clause);
        if field_name == "score" {
            continue;
        }
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
                    format!("can not sort on a field w/o fast values (docValues): {field_name}"),
                )
                .with_params(params));
            }
            Some(_) => {}
        }
    }
    Ok(())
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
    check_sort(&state, &params)?;

    let default_field = params
        .get("df")
        .unwrap_or(&state.index.wf_schema.core.default_field)
        .to_string();

    // No `q` matches nothing — it does *not* default to `*:*`. Solr answers 200
    // with an empty result set (`err_missing_q.json`), which resolves
    // tracer-bullet review follow-up 2 against the fixture.
    let hits = match params.get("q") {
        None => Vec::new(),
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

            state
                .index
                .search(query.as_ref(), &filter_queries)
                .map_err(|e| {
                    WfError::internal("wayfinder::SearchError", e.to_string()).with_params(&params)
                })?
        }
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

    if params.get("facet") == Some("true") {
        let facet_field = params.get("facet.field");
        let mut facet_fields = serde_json::Map::new();
        if let Some(field_name) = facet_field {
            let counted = state.index.facet_counts(field_name, &hits).map_err(|e| {
                WfError::bad_request("wayfinder::FacetError", e.to_string()).with_params(&params)
            })?;
            let mut flat = Vec::with_capacity(counted.len() * 2);
            for (term, count) in counted {
                flat.push(Value::String(term));
                flat.push(Value::from(count));
            }
            facet_fields.insert(field_name.to_string(), Value::Array(flat));
        }
        body["facet_counts"] = json!({
            "facet_queries": {},
            "facet_fields": facet_fields,
            "facet_ranges": {},
            "facet_intervals": {},
            "facet_heatmaps": {},
        });
    }

    Ok(axum::Json(body).into_response())
}
