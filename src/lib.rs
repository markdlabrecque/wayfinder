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
mod core_index;
mod params;
mod schema;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as AxPath, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use core_index::CoreIndex;
use params::Params;

struct AppState {
    core_name: String,
    index: CoreIndex,
}

/// Builds the Wayfinder HTTP app for a single core, loading its schema from
/// `schema_path` and storing/opening its Tantivy index under `data_dir`.
pub fn app(schema_path: &Path, data_dir: &Path) -> anyhow::Result<Router> {
    let index = CoreIndex::open(schema_path, data_dir)?;
    let core_name = index.wf_schema.core.name.clone();
    let state = Arc::new(AppState { core_name, index });

    Ok(Router::new()
        .route("/solr/{core}/update", post(update))
        .route("/solr/{core}/select", get(select))
        .route("/solr/{core}/admin/ping", get(ping))
        .with_state(state))
}

fn error_response(status: StatusCode, msg: impl Into<String>, params: &Params) -> Response {
    let code = status.as_u16() as i64;
    let body = json!({
        "responseHeader": {
            "status": code,
            "QTime": 0,
            "params": params.echo(),
        },
        "error": {
            "msg": msg.into(),
            "code": code,
        }
    });
    (status, axum::Json(body)).into_response()
}

/// Verifies the request's `{core}` path segment matches the core this app
/// serves. Not part of the tracer-bullet scope (single core per process,
/// PRD open question 1) beyond this sanity check.
fn check_core(state: &AppState, core: &str, params: &Params) -> Option<Response> {
    if core != state.core_name {
        return Some(error_response(
            StatusCode::NOT_FOUND,
            format!("unknown core `{core}`"),
            params,
        ));
    }
    None
}

async fn ping(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    if let Some(resp) = check_core(&state, &core, &params) {
        return resp;
    }
    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
            "params": params.echo(),
        },
        "status": "OK",
    });
    axum::Json(body).into_response()
}

async fn update(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    if let Some(resp) = check_core(&state, &core, &params) {
        return resp;
    }

    let docs: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("invalid JSON body: {e}"),
                &params,
            );
        }
    };
    let docs = match docs.as_array() {
        Some(arr) => arr.clone(),
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "update body must be a JSON array of documents",
                &params,
            );
        }
    };

    if let Err(e) = state.index.add_documents(&docs) {
        return error_response(StatusCode::BAD_REQUEST, e.to_string(), &params);
    }

    if params.get("commit") == Some("true")
        && let Err(e) = state.index.commit()
    {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), &params);
    }

    axum::Json(json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        }
    }))
    .into_response()
}

async fn select(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    if let Some(resp) = check_core(&state, &core, &params) {
        return resp;
    }

    let q = params.get("q").unwrap_or("*:*");
    let default_field = params
        .get("df")
        .unwrap_or(&state.index.wf_schema.core.default_field)
        .to_string();

    let query = match state.index.parse_query(q, &default_field) {
        Ok(q) => q,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e.to_string(), &params),
    };

    let mut filter_queries = Vec::new();
    for fq in params.get_all("fq") {
        match state.index.parse_query(fq, &default_field) {
            Ok(q) => filter_queries.push(q),
            Err(e) => return error_response(StatusCode::BAD_REQUEST, e.to_string(), &params),
        }
    }

    let hits = match state.index.search(query.as_ref(), &filter_queries) {
        Ok(hits) => hits,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), &params),
    };

    let num_found = hits.len();
    let start: usize = params
        .get("start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let rows: usize = params
        .get("rows")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

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
        match state.index.render_doc(addr, fl.as_deref()) {
            Ok(doc) => docs.push(doc),
            Err(e) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), &params);
            }
        }
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
            let counted = match state.index.facet_counts(field_name, &hits) {
                Ok(c) => c,
                Err(e) => {
                    return error_response(StatusCode::BAD_REQUEST, e.to_string(), &params);
                }
            };
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

    axum::Json(body).into_response()
}
