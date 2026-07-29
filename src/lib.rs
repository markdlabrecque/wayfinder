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
use serde_json::{Map, Value, json};
use tantivy::Score;
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
/// per-field overrides.
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
/// `commitWithin` / `overwrite` / `softCommit` landed with #9.
const UPDATE_PARAMS: &[&str] = &["commit", "commitWithin", "overwrite", "softCommit", "wt"];
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
/// envelope Solr uses for it (`err_update_put.json`). GET is not a method
/// error (finding 47) — Solr serves it, either 400ing on the empty body
/// (`missing content stream`) or committing if only asked to, both handled in
/// `update` itself, not here.
fn check_update_method(method: &Method) -> Result<(), WfError> {
    if method != Method::POST && method != Method::GET {
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

    // Rewritten clause grammar (finding 34/35, issue #32). Scanned with an
    // absolute cursor into `sort` rather than `split(',')`: a comma does NOT
    // delimit the field token (`,id` is one token, which is exactly how the
    // leading/doubled-comma fixtures end up as *field* errors — the glued
    // token simply fails field resolution), and everything after the field
    // token up to the next comma (or end of spec) is the direction, checked
    // as a single trimmed chunk rather than split further — which is what
    // makes `sort=id asc garbage` a direction error instead of a silently
    // dropped extra token.
    let mut clauses = Vec::new();
    let mut pos = 0usize;
    loop {
        // Skip whitespace between clauses. Also the mechanism for "no more
        // clauses": an empty or all-whitespace spec, or a trailing comma
        // followed only by whitespace/end, lands here with nothing left.
        let ws = sort[pos..]
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(sort.len() - pos);
        pos += ws;
        if pos >= sort.len() {
            break;
        }

        // FIELD: the next whitespace-delimited token, starting at `pos`. A
        // comma does not delimit it.
        let field_len = sort[pos..]
            .find(char::is_whitespace)
            .unwrap_or(sort.len() - pos);
        let field_end = pos + field_len;
        let field_name = &sort[pos..field_end];

        // DIRECTION: from just past the field token to the next comma or end
        // of spec, trimmed, checked as one chunk against `asc`/`desc`.
        let dir_start = field_end;
        let comma_rel = sort[dir_start..].find(',');
        let dir_end = dir_start + comma_rel.unwrap_or(sort.len() - dir_start);
        let direction_raw = &sort[dir_start..dir_end];

        // Direction first, field second (finding 18/34).
        //
        // `pos` mirrors Solr's parser position — the *absolute* offset within
        // the whole spec just past this clause's field token, leading
        // whitespace included (finding 35): `pos=2` for `'id sideways'`
        // (`err_sort_bad_direction.json`), `pos=9` for the second clause of
        // `'id asc,id sideways'` (`err_sort_second_clause_bad_direction.json`),
        // `pos=15` — past the *whole* second field token — for
        // `'id asc,category'` (`err_sort_second_clause_no_direction.json`),
        // and `pos=4` with leading spaces preserved for `'  id sideways'`
        // (`err_sort_leading_whitespace.json`).
        let descending = match direction_raw.trim() {
            "asc" => false,
            "desc" => true,
            _ => {
                return Err(WfError::bad_request(
                    "wayfinder::BadSort",
                    format!(
                        "Can't determine a Sort Order (asc or desc) in sort spec '{sort}', pos={dir_start}"
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

        // The schema's declared value kind travels with the clause so the
        // collector can materialise a segment-wide-absent column's missing
        // value as the right *type* (finding 36/37) — `score` has none, it is
        // never missing. `value_kind` already resolves any custom
        // `[[field_types]]`, which only ever produce `Text`, so there is no
        // numeric/date custom-type case this can miss.
        let value_kind = match &key {
            SortKey::Score => None,
            SortKey::Field(name) => state.index.wf_schema.value_kind(name),
        };
        clauses.push(SortClause::new(key, descending, value_kind));

        // Consume at most one comma after a valid clause. A trailing comma
        // (no more clauses after it) is fine; anything else — a second
        // consecutive comma, more text with no comma — starts the next loop
        // iteration as a new clause, whose field token then starts with a
        // comma and fails field resolution (the leading/doubled-comma cases).
        match comma_rel {
            Some(rel) => pos = dir_start + rel + 1,
            None => break,
        }
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

/// The parsed shape of an `/update` POST body: either form Solr accepts.
/// `add_docs` holds one JSON doc object per add (from either the bare-array
/// form or a `{"add":{"doc":{...}}}` command); `delete_ids`/`delete_queries`
/// and `commit` come only from the command-object form, since the bare-array
/// form is adds-only (existing, pre-#9 behaviour, unchanged).
#[derive(Default)]
struct UpdateCommands {
    add_docs: Vec<Value>,
    delete_ids: Vec<String>,
    delete_queries: Vec<String>,
    commit: bool,
}

/// Parses a `/update` POST body into add/delete/commit commands (finding 46).
/// Two shapes: the pre-#9 bare JSON array of docs (all adds, unchanged), and
/// Solr's command-object form — `{"add":{"doc":{...}}, "delete":{"id":...} |
/// [...] | {"query":"..."}, "commit":{}}`. Every key present in a
/// command-object body executes; the mixed-command fixture
/// (`update_mixed_commands.json`) has an add and a delete on independent ids,
/// so any execution order passes — `update` below just does adds, then
/// deletes-by-id, then deletes-by-query, then commit.
///
/// ponytail: `serde_json`'s `Value::Object` collapses a duplicate JSON key to
/// the last occurrence (legal in Solr's own hand-rolled parser, but
/// unobserved — no fixture repeats a command key), so that shape is out of
/// scope per the task spec.
fn parse_update_commands(body: &[u8]) -> Result<UpdateCommands, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let mut commands = UpdateCommands::default();
    match value {
        Value::Array(docs) => commands.add_docs = docs,
        Value::Object(map) => {
            for (key, val) in map {
                match key.as_str() {
                    "add" => {
                        let doc = val
                            .get("doc")
                            .cloned()
                            .ok_or_else(|| "\"add\" command is missing \"doc\"".to_string())?;
                        commands.add_docs.push(doc);
                    }
                    "delete" => match val {
                        Value::Array(ids) => {
                            for id in ids {
                                let id = id.as_str().ok_or_else(|| {
                                    "\"delete\" id list entries must be strings".to_string()
                                })?;
                                commands.delete_ids.push(id.to_string());
                            }
                        }
                        Value::Object(ref dm) if dm.contains_key("id") => {
                            let id = dm["id"]
                                .as_str()
                                .ok_or_else(|| "\"delete.id\" must be a string".to_string())?;
                            commands.delete_ids.push(id.to_string());
                        }
                        Value::Object(ref dm) if dm.contains_key("query") => {
                            let q = dm["query"]
                                .as_str()
                                .ok_or_else(|| "\"delete.query\" must be a string".to_string())?;
                            commands.delete_queries.push(q.to_string());
                        }
                        other => {
                            return Err(format!("unsupported \"delete\" command shape: {other}"));
                        }
                    },
                    "commit" => commands.commit = true,
                    other => return Err(format!("unsupported update command `{other}`")),
                }
            }
        }
        other => {
            return Err(format!(
                "update body must be a JSON array of documents or a command object, got {other}"
            ));
        }
    }
    Ok(commands)
}

/// The bare `{"responseHeader":{"status":0,"QTime":0}}` envelope every
/// `/update` success answers with, for every command shape (finding 46) —
/// never a `params` echo, never per-command keys.
fn update_success() -> Response {
    axum::Json(json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        }
    }))
    .into_response()
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

    // GET carries no body (finding 47): it is not a method error, but a
    // *content-stream* one — 400 "missing content stream" unless the only
    // thing being asked is a commit, which really commits and answers 200.
    if method == Method::GET {
        let commit_requested =
            params.get("commit") == Some("true") || params.get("softCommit") == Some("true");
        if !commit_requested {
            return Err(update_err(
                "wayfinder::MissingContentStream",
                "missing content stream".to_string(),
            ));
        }
        state.index.commit().map_err(|e| {
            WfError::internal("wayfinder::CommitError", e.to_string()).envelope(Envelope::NoParams)
        })?;
        return Ok(update_success());
    }

    // `overwrite=false` skips the default replace-by-uniqueKey step
    // (finding 48b); every other value (including absent) is Solr's default
    // `overwrite=true`.
    let overwrite = params.get("overwrite") != Some("false");

    let commands =
        parse_update_commands(&body).map_err(|msg| update_err("wayfinder::BadUpdateBody", msg))?;

    if !commands.add_docs.is_empty() {
        state
            .index
            .add_documents(&commands.add_docs, overwrite)
            .map_err(|e| update_err("wayfinder::IndexError", e.to_string()))?;
    }
    if !commands.delete_ids.is_empty() {
        state
            .index
            .delete_by_ids(&commands.delete_ids)
            .map_err(|e| update_err("wayfinder::IndexError", e.to_string()))?;
    }
    for query in &commands.delete_queries {
        state
            .index
            .delete_by_query(query, &state.index.wf_schema.core.default_field)
            .map_err(|e| update_err("wayfinder::IndexError", e.to_string()))?;
    }

    // `commit=true` (existing) and `softCommit=true` both mean "commit and
    // reload now" — per the task spec's softCommit note, Tantivy has no
    // in-memory-searchable segment for a real soft commit to leave
    // uncommitted-but-visible, so Wayfinder's softCommit is a hard commit
    // too (wire-visible behaviour matches Solr; durability is only ever
    // stronger, never weaker). A `commit` key in the body does the same.
    let commit_now = commands.commit
        || params.get("commit") == Some("true")
        || params.get("softCommit") == Some("true");
    if commit_now {
        state.index.commit().map_err(|e| {
            WfError::internal("wayfinder::CommitError", e.to_string()).envelope(Envelope::NoParams)
        })?;
    }
    // `commitWithin=<ms>` schedules a commit at most that many ms out — also
    // a HARD commit (+ reload) in Wayfinder, same divergence note as
    // `softCommit` above (task spec: "Same for commitWithin: it schedules a
    // HARD commit, where Solr's default is soft").
    if let Some(ms) = params
        .get("commitWithin")
        .and_then(|s| s.parse::<u64>().ok())
    {
        state.index.schedule_commit(ms);
    }

    Ok(update_success())
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

    // `fl=score` is what turns scoring output on at all (Solr), so this is
    // the single check that gates both the per-doc `score` key and
    // `response.maxScore` below.
    let wants_score = fl
        .as_deref()
        .is_some_and(|fl| fl.iter().any(|f| f == "score"));

    let mut docs = Vec::with_capacity(page.len());
    for (score, addr) in page {
        docs.push(
            state
                .index
                .render_doc(addr, fl.as_deref(), Some(score))
                .map_err(|e| {
                    WfError::internal("wayfinder::DocError", e.to_string()).with_params(&params)
                })?,
        );
    }

    // Key order in the fixtures is `numFound, start, maxScore, numFoundExact,
    // docs` — `maxScore` sits between `start` and `numFoundExact`, which a
    // `json!` object literal can't express conditionally, so this is built
    // the same way `response_header` is below. Built *before* `facet_result`
    // (issue #35): a `facet.query`/`facet.field` error is detected only after
    // the base query has already run, so Solr's own fixtures for those errors
    // (`facet_unknown_field.json`, `facet_err_query_single.json`) still carry
    // this `response` block alongside `error` — it has to exist already so it
    // can be attached to that error below.
    let mut response = Map::new();
    response.insert("numFound".to_string(), json!(num_found));
    response.insert("start".to_string(), json!(start));
    if wants_score {
        // ponytail: computed as the max score across the *whole*
        // (unpaginated) match list, not just the current page — an
        // unverified choice, not a fixtured fact. Every scored fixture
        // (`select_term_scored.json`, `select_quick_scored.json`) has
        // `start=0` with the full result set on one page, so page-max and
        // global-max are indistinguishable there; no fixture pages a scored
        // query to tell them apart.
        //
        // ponytail: no fixture covers `fl=score` against zero hits, so
        // whether Solr omits `maxScore` or reports `0.0` there is
        // unverified; this omits the key entirely on the (untested)
        // assumption that Solr does the same, mirroring how `docs: []`
        // still reports a real `numFound: 0` without inventing a score.
        if let Some(max_score) = hits
            .iter()
            .map(|(score, _)| *score)
            .fold(None, |acc: Option<Score>, s| {
                Some(acc.map_or(s, |a| a.max(s)))
            })
        {
            response.insert("maxScore".to_string(), json!(max_score));
        }
    }
    response.insert("numFoundExact".to_string(), json!(true));
    response.insert("docs".to_string(), json!(docs));

    // `facet=true` gates the whole block; `facet.field` alone does not turn
    // faceting on and the key stays absent (findings fact 4). Computed *before*
    // `responseHeader` is built, not after: Solr's own `responseHeader` key
    // order is `warnings, status, QTime, params` — `warnings` leads, it does
    // not trail (finding 29 / issue #24) — and `serde_json`'s `preserve_order`
    // feature (issue #25) means the order keys are inserted in is now the order
    // they are emitted in, so `warnings` has to be known before the object
    // literal is written.
    let facet_result = if params.get("facet") == Some("true") {
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
        Some(
            facet::facet_counts(&state.index, &state.config, &params, &default_field, &base)
                .map_err(|e| {
                    // Issue #35: `facet.range` is detected before the base
                    // query ever runs (Solr's own `facet_err_range_single.json`
                    // has no `response` block), while `facet.query`/
                    // `facet.field` errors are detected after it (Solr's
                    // `facet_unknown_field.json` / `facet_err_query_single.json`
                    // do). `facet::facet_counts` marks the former with
                    // `PreQueryFacetError` so only the latter gets `response`
                    // attached here.
                    let err = WfError::bad_request("wayfinder::FacetError", e.to_string())
                        .with_params(&params);
                    if e.downcast_ref::<facet::PreQueryFacetError>().is_some() {
                        err
                    } else {
                        err.with_response(Value::Object(response.clone()))
                    }
                })?,
        )
    } else {
        None
    };
    let warnings = facet_result
        .as_ref()
        .map(|(_, warnings)| warnings.as_slice())
        .unwrap_or_default();

    // `responseHeader.warnings` is absent unless there is something to warn
    // about (every fixture that isn't a Points-based `facet.field` at
    // mincount 0 lacks the key) — never an empty array — and, when present,
    // leads the object rather than trailing it.
    let mut response_header = Map::new();
    if !warnings.is_empty() {
        response_header.insert("warnings".to_string(), json!(warnings));
    }
    response_header.insert("status".to_string(), json!(0));
    response_header.insert("QTime".to_string(), json!(0));
    response_header.insert("params".to_string(), json!(params.echo()));

    let mut body = json!({
        "responseHeader": response_header,
        "response": response,
    });

    if let Some((facet_counts, _)) = facet_result {
        body["facet_counts"] = facet_counts;
    }

    Ok(axum::Json(body).into_response())
}
