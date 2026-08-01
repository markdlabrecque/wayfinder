//! Wayfinder: a Solr-wire-compatible search server on top of Tantivy.
//!
//! Grown from the tracer bullet (PRD §7) — one thin vertical slice through
//! every layer, kept and iterated on rather than a spike: TOML schema ->
//! Tantivy schema, `/update` (JSON add + commit), `/select` (`q`, `fq`,
//! `fl`, `rows`, `start`, and the `facet.*` family — see `crate::facet`),
//! and `/admin/ping`.
//!
//! `sort` was out of the tracer-bullet scope and has since landed (issue #2),
//! as has `stats` (issue #5) and highlighting (`hl`/`hl.fl` — see
//! `crate::highlight`, issue #4). Deliberately out of scope here (PRD §7):
//! edismax, MLT. Multi-core: out of scope too — `app()` serves exactly one
//! core, matching PRD open question 1's "single-core-per-process" lean.
//!
//! Alongside the Solr wire API, `GET /ui` serves the admin UI's core page
//! (issue #94, PRD §5 v2.5) — Wayfinder's own surface, not Solr's, rendered
//! from the same in-process core state — and `GET /ui/query` the query tester
//! over it (issue #127), which runs its queries through `select` itself
//! rather than a second query path, and `GET /ui/schema` a read-only view of
//! the core's schema (issue #128), served from the `WayfinderSchema` the core
//! is running on rather than a fresh read of the TOML, and `GET /ui/stats`
//! the core's doc/segment counts, on-disk size and process uptime (issue
//! #129), and `GET /ui/ping` this process's ping/health status (issue #130),
//! which — like the query tester — calls the real `ping` handler rather than
//! running a health check of its own. See `crate::admin_ui`.

mod admin_ui;
mod collector;
mod config;
mod core_index;
mod coverage;
pub mod edismax;
mod error;
mod facet;
mod highlight;
mod local_params;
mod params;
mod query;
pub mod schema;
mod stats;

pub use config::ServerConfig;
pub use coverage::report as coverage_report;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path as AxPath, RawQuery, State};
use axum::http::{Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use tantivy::Score;
use tantivy::query::{EmptyQuery, Occur, Query, QueryClone};
use tower_http::catch_panic::CatchPanicLayer;

use collector::{SortClause, SortKey};
use core_index::CoreIndex;
use error::{Envelope, WfError};
use params::Params;

struct AppState {
    core_name: String,
    index: CoreIndex,
    config: ServerConfig,
    /// When this app was built, i.e. when the process began serving — the
    /// only piece of admin-UI state that is not derivable from the index
    /// (issue #129). Captured once in `build()` and never updated;
    /// `Instant` because uptime is an elapsed-time question, and a monotonic
    /// clock cannot be walked backwards by an NTP step the way a
    /// `SystemTime` difference can.
    started_at: Instant,
}

/// Request params Wayfinder implements today. Only consulted when
/// `strict_params` is on — by default unknown params are ignored, as Solr does
/// (findings fact 8).
///
/// **Implementing a new param? Add it here.** Otherwise `strict_params = true`
/// will 400 on a param Wayfinder actually supports. `sort` is fully implemented
/// as of #2 — validated by #11, ordered by #2. The `facet.*` family landed with
/// #3; still absent from it, and so still unlisted: `facet.method`,
/// `facet.prefix`, `facet.pivot`, interval and heatmap faceting, and
/// `facet.range.other` / `.include` / `.hardend`.
///
/// Solr's per-field override form `f.<field>.<param>` is a *shape*, not a fixed
/// name, so it cannot live in this list — see `PER_FIELD_PARAMS` and
/// `check_params`.
const SELECT_PARAMS: &[&str] = &[
    "q",
    "df",
    "fq",
    // edismax (issue #7, PRD §5 v1 exception): `defType=edismax` switches the
    // query parser; `qf`/`pf`/`mm`/`tie`/`boost`/`bq` are its own params.
    "defType",
    "qf",
    "pf",
    "mm",
    "tie",
    "boost",
    "bq",
    // Accepted-and-ignored (issue #108): edismax's boost-function param.
    // Wayfinder does not apply it yet, but a request using it must not 400
    // under strict_params, since Solr accepts it.
    "bf",
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
    "stats",
    "stats.field",
    // Solr accepts and echoes search_api_solr's `function=max(_version_)`
    // watermark shape; stats.field remains the sole aggregation key.
    "function",
    "sort",
    "hl",
    "hl.fl",
    "hl.snippets",
    "hl.fragsize",
    "hl.simple.pre",
    "hl.simple.post",
    "hl.method",
    // Issue #139: every captured `search_api_solr` search sends both of
    // these, always `false`. `false` is Solr's own documented default for
    // both *and* already Wayfinder's unconditional behaviour, so this is an
    // allowlist-only entry -- there is deliberately no knob behind it,
    // because the `false` path has nothing to disable. `src/highlight.rs`
    // applies no field-match filtering (a doc matching through a
    // non-highlighted field still gets its finding-52 entry, which *is*
    // `hl.requireFieldMatch=false`'s behaviour) and does no fragment merging
    // (each `CoreIndex::highlight_field` fragment is emitted as its own
    // snippet, which *is* `hl.mergeContiguous=false`'s behaviour).
    // Implementing the `true` side of either would be real work, not a flag:
    // `hl.requireFieldMatch=true` needs per-field query-term extraction so
    // only terms targeting field F highlight F, and `hl.mergeContiguous=true`
    // needs adjacent-fragment coalescing in `highlight_field`'s
    // mask-and-resnippet loop. Neither has a captured fixture to derive an
    // expected shape from, so neither is implemented or asserted.
    "hl.mergeContiguous",
    "hl.requireFieldMatch",
    "wt",
    // Envelope params `search_api_solr` sends on essentially every request
    // (issue #143). `omitHeader=true` drops `responseHeader` — see
    // `Params::omit_header`. `TZ` must not 400 under `strict_params = true`,
    // since Solr accepts it; it is accepted and ignored.
    //
    // ponytail: `TZ` is inert *today*, not inherently. Wayfinder does have a
    // date field type (`ResolvedType::Date` / `add_date_field`, `src/schema.rs`)
    // and date-range faceting (`parse_date` / `parse_date_gap` /
    // `RangeEnd::Date`, `src/facet.rs`), which are exactly what a timezone
    // would bear on in Solr. It stays inert because of two narrower facts:
    // `facet.range.start`/`end` must be literal RFC3339 instants — `parse_date`
    // rejects `NOW` and every other date-math expression, and no date math
    // exists anywhere else — and `parse_date_gap` refuses the calendar gaps
    // `+1MONTH`/`+1YEAR` by name. What is left is fixed-length gaps walked over
    // absolute instants, and those give the same bucket boundaries in every
    // timezone.
    //
    // The ceiling ends the moment either fact does: if `NOW`/date-math parsing
    // or MONTH/YEAR gaps land, `TZ` starts changing which bucket a document
    // falls in, and silently ignoring it becomes a wrong answer rather than a
    // no-op. Whoever lands either must thread `TZ` through to date parsing and
    // gap walking here, not just add the feature.
    "omitHeader",
    "TZ",
];
/// Base params Wayfinder also honours in Solr's per-field override form
/// `f.<field>.<param>` (issue #140 — `search_api_solr` sends
/// `f.ss_type.facet.missing=true`, never the bare global). `check_params`
/// accepts `f.<field>.<p>` for every `p` here that the endpoint's own allowlist
/// already contains, which is what keeps the shape from leaking to endpoints
/// that do not implement the base param at all: `/update` has no
/// `facet.missing`, so `f.x.facet.missing` still 400s there.
///
/// ponytail: exactly one entry, and that is the ceiling, not an oversight.
/// Every other `f.<field>.facet.*` Solr accepts (`.limit`, `.mincount`,
/// `.sort`, `.prefix`) is unimplemented here and must keep 400ing under
/// `strict_params` — pinned by
/// `strict_params_still_rejects_an_unrelated_f_dot_param`
/// (`tests/facet_field_missing_override.rs`). Allowlisting a per-field param
/// whose value is then ignored converts a loud 400 into a silently wrong
/// answer: a client asking for `f.category.facet.limit=5` would get the global
/// limit and no indication it was dropped. Upgrade path: implement the
/// override where the global is read in `src/facet.rs` (the `facet.missing`
/// resolution in `facet_fields` is the worked example — `Params::per_field`
/// wins over the global unconditionally, finding 97), *then* add the base param
/// name here in the same change. Adding a name here alone is the bug.
const PER_FIELD_PARAMS: &[&str] = &["facet.missing"];
/// `commitWithin` / `overwrite` / `softCommit` landed with #9. `omitHeader`
/// landed with #143 — `search_api_solr` sends `omitHeader=false` on every
/// `/update` (`solr-ref/search-api/trace/00001.json`). `json.nl` landed with
/// #153. No `TZ`: the module never sends one here.
const UPDATE_PARAMS: &[&str] = &[
    "commit",
    "commitWithin",
    "overwrite",
    "softCommit",
    "omitHeader",
    "wt",
    "json.nl",
];
const PING_PARAMS: &[&str] = &["wt"];
/// `/admin/info/system` (server-level) and `<core>/admin/system`
/// (core-scoped fallback) — issue #59's version-handshake endpoints.
const ADMIN_INFO_PARAMS: &[&str] = &["wt", "json.nl"];
/// `<core>/schema/fieldtypes` (issue #156). The captured request
/// (`solr-ref/search-api/trace/00020.json`) sends exactly these two.
const SCHEMA_FIELDTYPES_PARAMS: &[&str] = &["wt", "json.nl"];
/// `<core>/admin/luke` (issue #157). The captured request
/// (`solr-ref/search-api/trace/00024.json`) sends only `wt`/`json.nl`; the
/// other three are the params real Solr's LukeRequestHandler takes that a
/// client might plausibly send. `numTerms`, `show` and `fl` have no behaviour
/// here (no term histograms, no `show=schema` variant, no per-field
/// selection), and are accepted-and-ignored on purpose: 400ing a param Solr
/// serves would be a worse divergence than answering the full response.
const ADMIN_LUKE_PARAMS: &[&str] = &["wt", "json.nl", "numTerms", "show", "fl"];
/// `<core>/admin/mbeans` (issue #158). `stats` is the only one that changes
/// the response; `wt`/`json.nl` are the usual writer params, and `cat`/`key`
/// are Solr's bean-selection filters, accepted-and-ignored (see
/// `admin_mbeans`) so a client that sends them does not 400 under
/// `strict_params`.
const MBEANS_PARAMS: &[&str] = &["stats", "wt", "json.nl", "cat", "key"];
/// `/mlt` params in scope for issue #6 (PRD §5's MoreLikeThis row). `q`
/// selects the source doc with the same query-parsing semantics as
/// `/select`'s `q` (hence `df` alongside it); `fl`/`rows`/`start` page the
/// similar-docs result set exactly as `/select` does. Out of scope, per the
/// task spec: `mlt=true` as a `/select` search component, and content-stream
/// MLT.
/// Route behavior shared by the real Axum router and coverage report. A route
/// cannot be reported as covered unless this table wires it and accepts the
/// captured method.
struct RouteSpec {
    path: &'static str,
    accepts_method: fn(&str) -> bool,
}

fn any_method(_: &str) -> bool {
    true
}

fn update_method(method: &str) -> bool {
    matches!(method, "POST" | "GET")
}

macro_rules! search_api_routes {
    ($apply:ident) => {
        $apply! {
            ("/solr/{core}/update", update, update_method),
            ("/solr/{core}/select", select, any_method),
            ("/solr/{core}/mlt", mlt, any_method),
            ("/solr/{core}/terms", terms, any_method),
            ("/solr/{core}/admin/ping", ping, any_method),
            ("/solr/admin/info/system", admin_info_system, any_method),
            ("/solr/{core}/admin/system", core_admin_system, any_method),
            ("/solr/{core}/schema/fieldtypes", schema_fieldtypes, any_method),
            ("/solr/{core}/admin/luke", admin_luke, any_method),
            ("/solr/{core}/admin/mbeans", admin_mbeans, any_method),
        }
    };
}

macro_rules! route_specs {
    ($(($path:literal, $handler:ident, $accepts_method:ident)),+ $(,)?) => {
        &[$(RouteSpec { path: $path, accepts_method: $accepts_method }),+]
    };
}

macro_rules! wire_routes {
    ($(($path:literal, $handler:ident, $accepts_method:ident)),+ $(,)?) => {
        Router::new()$(.route($path, any($handler)))+
    };
}

const ROUTES: &[RouteSpec] = search_api_routes!(route_specs);

const MLT_PARAMS: &[&str] = &[
    "q",
    "df",
    "fl",
    "rows",
    "start",
    "mlt.fl",
    "mlt.mintf",
    "mlt.mindf",
    "mlt.maxdf",
    "mlt.minwl",
    "mlt.maxwl",
    "mlt.maxqt",
    "mlt.boost",
    "mlt.interestingTerms",
    "wt",
    // Issue #143, same envelope pair `/select` takes — `search_api_solr`
    // sends `omitHeader=true&TZ=UTC` on `/mlt` too
    // (`solr-ref/search-api/trace/00022.json`).
    "omitHeader",
    "TZ",
    // Issue #141. Each of these four is *implemented* below, not merely
    // allowlisted: `fq` filters the similar-docs set only (finding 98),
    // `mlt.match.include=false` drops the `match` key outright (finding 100),
    // `mlt.match.offset` selects which `q` hit seeds the query (finding 99),
    // and `json.nl` shapes `interestingTerms`'s container (finding 101).
    "fq",
    "mlt.match.include",
    "mlt.match.offset",
    "json.nl",
    // ponytail: `mlt.maxntp` is deliberately absent, so `strict_params = true`
    // keeps 400ing it (issue #189). Tantivy 0.26's `MoreLikeThis` has no
    // `maxNumTokensParsed` equivalent, and real Solr's `mlt.maxntp` genuinely
    // narrows results at a low value (finding block for issue #141) —
    // allowlisting it would turn a loud 400 into a silent wrong answer.
];

/// `/terms` params in scope for issue #155 (Solr's TermsComponent). `terms`
/// gates the component, `terms.fl` (repeatable) names the fields;
/// `omitHeader`/`wt`/`json.nl` are the envelope params `search_api_solr`
/// always sends on this endpoint (`solr-ref/search-api/trace/00028.json`).
///
/// ponytail: deliberately absent, so `strict_params = true` still 400s them —
/// `terms.limit`, `terms.sort`, `terms.prefix`, `terms.lower`/`upper`,
/// `terms.mincount`/`maxcount`, `terms.regex`, `terms.raw`, `terms.ttf`. The
/// ceiling is Solr's defaults only (`limit=10`, `sort=count`), which is
/// exactly what the trace exercises and what the coverage contract asks for.
/// Add the rest when the suggester work (PRD v3) produces a capture that needs
/// them — listing a param here that the handler ignores would be worse than
/// 400ing it, since it would silently answer the wrong question.
const TERMS_PARAMS: &[&str] = &["terms", "terms.fl", "omitHeader", "wt", "json.nl"];

/// Solr's `terms.limit` default. Not configurable here — see `TERMS_PARAMS`.
const TERMS_DEFAULT_LIMIT: usize = 10;

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
    // Issue #64: raise (and make configurable via `resources.max_body_size`)
    // the request-body cap that axum's `Bytes`/`Json` extractors otherwise
    // enforce at a bare, hardcoded 2MB via `DefaultBodyLimit`.
    let max_body_size = config.resources.max_body_size;
    let state = Arc::new(AppState {
        core_name,
        index,
        config,
        started_at: Instant::now(),
    });

    // `any`, not `get`/`post`: Solr's request handlers are method-agnostic —
    // `err_select_delete.json` shows DELETE /select served as a normal query,
    // so a 405 from the router would be a divergence. `/update` does reject
    // some methods (`err_update_put.json`), which it does itself, with Solr's
    // envelope for it.
    let router = search_api_routes!(wire_routes)
        // Admin UI (issue #94, PRD §5 v2.5). Outside `/solr/*` on purpose:
        // this is Wayfinder's own surface, not part of the Solr wire API, so
        // it can never shadow a path a Solr client expects — deliberately
        // not part of `search_api_routes!`, which drives the coverage
        // denominator's route surface. `get`, not `any` — the
        // method-agnostic routing the macro uses exists to match Solr's
        // request handlers, and that reason does not apply here.
        .route("/ui", get(core_ui))
        // Query tester (issue #127) — same reasoning as `/ui` above, and
        // deliberately a thin wrapper: `query_ui` calls `select` itself.
        .route("/ui/query", get(query_ui))
        // Schema view (issue #128) — read-only, and served from the
        // `WayfinderSchema` already on `AppState.index`, so there is no
        // second schema-parsing path to keep in sync with `schema::load`.
        .route("/ui/schema", get(schema_ui))
        // Index stats (issue #129) — read-only, and derived per request from
        // the live core plus this app's start instant, so there is no stats
        // subsystem and no cached figure that could go stale.
        .route("/ui/stats", get(stats_ui))
        // Ping/health (issue #130) — same thin-wrapper reasoning as
        // `/ui/query`: `ping_ui` calls `ping` itself, so there is no second
        // health-check path that could report healthy while `/admin/ping`
        // does not.
        .route("/ui/ping", get(ping_ui));

    // Test-only, never in a default/release build (#39): a route that always
    // panics, so `tests/panic_recovery.rs` can exercise the real router's
    // panic-catching layer via a genuine, unconditional handler panic
    // instead of relying on a bug (like the `*:*` sub-clause panic this same
    // change fixes elsewhere) as its trigger. Gated behind the `test-support`
    // Cargo feature, which only this crate's own `[dev-dependencies]` entry
    // in `Cargo.toml` enables — a normal `cargo build`/`cargo build --release`
    // never compiles it in.
    #[cfg(feature = "test-support")]
    let router = router.route("/solr/{core}/__test_panic__", any(test_panic));

    // Defence in depth (#39): a handler panic (e.g. an unforeseen
    // `.unwrap()`/`.expect()` deep in a dependency, reachable from
    // attacker-controlled input) must surface as a normal HTTP 500 in
    // Solr's error envelope rather than unwinding the connection. This is a
    // last-resort net, not a substitute for fixing the panic at its source.
    Ok(router
        .with_state(state)
        .layer(CatchPanicLayer::custom(handle_panic))
        .layer(DefaultBodyLimit::max(max_body_size)))
}

/// Test-only handler behind the `test-support` feature (see `build()`): an
/// unconditional panic, for `tests/panic_recovery.rs` to exercise the
/// panic-catching layer against a real, deliberate panic.
#[cfg(feature = "test-support")]
async fn test_panic() -> Response {
    panic!("test-support: deliberate panic for panic-recovery test coverage")
}

/// Converts a caught router panic into a Solr-shaped 500 error response.
///
/// Uses `Envelope::Bare` (no `responseHeader`/`params` echo): a panic can
/// happen before request params are ever parsed, and this handler runs
/// outside any single request's handler body, so it has no `Params` value to
/// echo — unlike `WfError` sites inside `select`/`update`, which do.
fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let details = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    };
    WfError::internal("wayfinder::PanicError", details)
        .envelope(Envelope::Bare)
        .into_response()
}

/// `GET /ui` — the admin UI's core page (issue #94, PRD §5 v2.5).
///
/// Read-only and idempotent: it takes a searcher for the doc count and stats
/// the data dir for the size, and writes nothing. No params, no core segment
/// in the path — this process serves exactly one core (see the module doc),
/// so there is nothing to select between and nothing to add to
/// `SELECT_PARAMS`.
///
/// A template render failure is a bug in a compile-time-checked template, not
/// a client error, so it surfaces as a plain 500 rather than a Solr JSON
/// error envelope — the envelope is the wire API's contract, and this route
/// is deliberately outside it.
async fn core_ui(State(state): State<Arc<AppState>>) -> Response {
    let html = admin_ui::render_core_page(
        &state.core_name,
        state.index.doc_count(),
        state.index.disk_size_bytes(),
    );
    match html {
        Ok(body) => Html(body).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render the admin UI: {e}"),
        )
            .into_response(),
    }
}

/// `GET /ui/schema` — the admin UI's schema view (issue #128, PRD §5 v2.5).
///
/// Renders the core's declared fields (name, type, and the
/// `stored`/`fast`/`multi_valued`/`required` flags), its dynamic-field rules,
/// and its copy-field pairs, straight off the in-process `WayfinderSchema`
/// `CoreIndex::open` loaded at startup — the same struct
/// `schema::check_compatible` validates against. Re-reading the TOML per
/// request would introduce a second parsing path that could disagree with the
/// index actually being served.
///
/// Read-only: no params, no form, no mutation route. A render failure is a
/// bug in a compile-time-checked template, so it surfaces as a plain 500,
/// same as `/ui`.
async fn schema_ui(State(state): State<Arc<AppState>>) -> Response {
    let wf_schema = &state.index.wf_schema;
    let html = admin_ui::render_schema_page(
        &state.core_name,
        &wf_schema.fields,
        &wf_schema.dynamic_fields,
        &wf_schema.copy_fields,
    );
    match html {
        Ok(body) => Html(body).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render the schema view: {e}"),
        )
            .into_response(),
    }
}

/// `GET /ui/stats` — the admin UI's index stats page (issue #129, PRD §5
/// v2.5).
///
/// Doc count and segment count come off the live searcher, the on-disk size
/// off a walk of the core's data dir (all three the same `CoreIndex`
/// accessors `/ui` uses), and the uptime off `AppState.started_at` — so every
/// figure describes the index this process is actually serving, and none of
/// them is cached.
///
/// No resident-memory figure is reported: Wayfinder is mmap-based, the page
/// says so, and it does not invent a JVM-heap-shaped number (PRD §5 v2.5,
/// §6's absent-heap-knob honesty).
///
/// Read-only, and a render failure surfaces as a plain 500, same as `/ui`.
async fn stats_ui(State(state): State<Arc<AppState>>) -> Response {
    let html = admin_ui::render_stats_page(
        &state.core_name,
        state.index.doc_count(),
        state.index.segment_count(),
        state.index.disk_size_bytes(),
        state.started_at.elapsed(),
    );
    match html {
        Ok(body) => Html(body).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render the index stats: {e}"),
        )
            .into_response(),
    }
}

/// `GET /ui/query` — the admin UI's query tester (issue #127, PRD §5 v2.5).
///
/// A form over the core's own `/select` and nothing more: with a non-empty
/// query string it calls [`select`] — the very function `/solr/{core}/select`
/// routes to, with this process's single core name filled in — and renders
/// that call's real status and JSON body. There is no second parsing,
/// validation, or execution path, so nothing here can drift from the wire
/// API: a query that 400s against `/select` 400s here, with `/select`'s own
/// error envelope on the page rather than a UI-invented message.
///
/// Read-only: `select` never mutates the index, and this handler adds
/// nothing to it.
///
/// The status code is `select`'s, not the page render's, so the tester is
/// as scriptable/diagnosable as the endpoint it wraps; the body is always
/// HTML (including on an error), so the form is still there to correct and
/// resubmit.
async fn query_ui(State(state): State<Arc<AppState>>, RawQuery(raw): RawQuery) -> Response {
    let raw = raw.unwrap_or_default();
    let params = Params::parse(&raw);
    let form = admin_ui::QueryForm {
        q: params.get("q").unwrap_or(""),
        fq: params.get("fq").unwrap_or(""),
        fl: params.get("fl").unwrap_or(""),
        rows: params.get("rows").unwrap_or(""),
        start: params.get("start").unwrap_or(""),
        facet_field: params.get("facet.field").unwrap_or(""),
        facet: params.get("facet").is_some_and(|v| v == "true"),
    };

    let result = match submitted_query(&raw) {
        None => None,
        Some(query) => {
            // The one and only query path: `/select`'s own handler.
            let response = match select(
                State(Arc::clone(&state)),
                AxPath(state.core_name.clone()),
                RawQuery(Some(query)),
            )
            .await
            {
                Ok(response) => response,
                Err(e) => e.into_response(),
            };
            let status = response.status().as_u16();
            let body = match response.into_body().collect().await {
                Ok(collected) => String::from_utf8_lossy(&collected.to_bytes()).into_owned(),
                // `select` builds its body in memory, so this is unreachable
                // in practice; surfacing it as text beats unwrapping.
                Err(e) => format!("failed to read the /select response body: {e}"),
            };
            Some((status, body))
        }
    };

    let render = admin_ui::render_query_page(
        &state.core_name,
        &form,
        result
            .as_ref()
            .map(|(status, body)| (*status, body.as_str())),
    );
    match render {
        Ok(html) => {
            let status = result
                .map(|(status, _)| StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .unwrap_or(StatusCode::OK);
            (status, Html(html)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render the query tester: {e}"),
        )
            .into_response(),
    }
}

/// `GET /ui/ping` — the admin UI's ping/health page (issue #130, PRD §5
/// v2.5).
///
/// The health verdict is not computed here: this handler calls [`ping`] — the
/// very function `/solr/{core}/admin/ping` routes to, with this process's
/// single core name filled in — and renders that call's real `status` value
/// and HTTP status. So there is no second health-check code path to keep in
/// sync with the wire endpoint; a page that says `OK` says it because
/// `/admin/ping` just did.
///
/// Read-only: `ping` takes no searcher and writes nothing, and this handler
/// adds nothing to it. No params either — `ping`'s only param is `wt`, which
/// selects a response writer this page does not use.
async fn ping_ui(State(state): State<Arc<AppState>>) -> Response {
    // The one and only health path: `/admin/ping`'s own handler.
    let response = match ping(
        State(Arc::clone(&state)),
        AxPath(state.core_name.clone()),
        RawQuery(None),
    )
    .await
    {
        Ok(response) => response,
        Err(e) => e.into_response(),
    };
    let http_status = response.status().as_u16();
    let body = match response.into_body().collect().await {
        Ok(collected) => String::from_utf8_lossy(&collected.to_bytes()).into_owned(),
        // `ping` builds its body in memory, so this is unreachable in
        // practice; surfacing it as text beats unwrapping.
        Err(e) => format!("failed to read the /admin/ping response body: {e}"),
    };

    // Whatever `ping` reported, verbatim. A response without a string
    // `status` is not something `ping` produces today, but showing the raw
    // body beats the page inventing a verdict of its own.
    let parsed = serde_json::from_str::<Value>(&body).ok();
    let status = parsed
        .as_ref()
        .and_then(|value| value["status"].as_str())
        .unwrap_or(body.as_str());

    match admin_ui::render_ping_page(&state.core_name, status, http_status) {
        Ok(html) => (
            StatusCode::from_u16(http_status).unwrap_or(StatusCode::OK),
            Html(html),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render the ping page: {e}"),
        )
            .into_response(),
    }
}

/// The query string to hand to `/select`, or `None` if this is a first load
/// (or a submission of a wholly empty form) and no query should run.
///
/// Params written as `key=` (an explicit `=` with nothing after it) are
/// dropped: a GET form submits every one of its inputs, so an untouched `fq`
/// box would otherwise reach `/select` as `fq=`, an empty filter query that
/// 400s. Everything that survives is forwarded *verbatim*, still
/// percent-encoded, so `/select` sees those params exactly as it would have
/// received them directly — same values, same order, same echo.
///
/// Two known consequences, both accepted for a form UI rather than papered
/// over:
///
/// - An *intentionally* empty value (`q=`, `fl=`) cannot be expressed
///   through the tester. `/select` distinguishes an absent `q` from an empty
///   one; the tester can only reach the absent case.
/// - The rule is written in terms of the raw segment, not the decoded value,
///   so a valueless `&fq` (no `=` at all) survives even though `Params`
///   decodes it to the same `""`. That shape is unreachable from the form
///   (a browser always sends `key=value`) and is kept deliberately: a bare
///   token is how flag-style params are written by hand, and dropping it
///   would silently discard a param the operator typed.
fn submitted_query(raw: &str) -> Option<String> {
    let kept: Vec<&str> = raw
        .split('&')
        .filter(|segment| !segment.is_empty())
        .filter(|segment| match segment.split_once('=') {
            Some((_, value)) => !value.is_empty(),
            // A valueless flag (`&debug`) is a real param, not a blank box.
            None => true,
        })
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join("&"))
    }
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
    if !update_method(method.as_str()) {
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
            // Resolved with the same static-before-dynamic precedence
            // indexing already uses (issue #66): a declared `[[fields]]`
            // entry wins over a `[[dynamic_fields]]` pattern that would also
            // match it, and a dynamic-only match sorts on the catch-all JSON
            // column it is actually indexed into (mirrors
            // `CoreIndex::rewrite_dynamic_fields`'s resolution for the query
            // path), not the bare field name.
            match state.index.wf_schema.resolved_fast(field_name) {
                None => {
                    return Err(WfError::bad_request(
                        "wayfinder::BadSort",
                        format!("can not sort on undefined field: {field_name}"),
                    )
                    .with_params(params));
                }
                Some(false) => {
                    return Err(WfError::bad_request(
                        "wayfinder::BadSort",
                        format!(
                            "can not sort on a field w/o fast values (docValues): {field_name}"
                        ),
                    )
                    .with_params(params));
                }
                Some(true) => {
                    let column = state
                        .index
                        .wf_schema
                        .resolved_fast_column(field_name)
                        .expect("resolved_fast confirmed this name resolves");
                    SortKey::Field(column)
                }
            }
        };

        // The schema's declared value kind travels with the clause so the
        // collector can materialise a segment-wide-absent column's missing
        // value as the right *type* (finding 36/37) — `score` has none, it is
        // never missing. `resolved_value_kind` already resolves any custom
        // `[[field_types]]`, which only ever produce `Text`, so there is no
        // numeric/date custom-type case this can miss. Resolved from the
        // original `field_name`, not the (possibly rewritten) column in
        // `key`, since a dynamic column's own name carries no schema entry.
        let value_kind = match &key {
            SortKey::Score => None,
            SortKey::Field(_) => state.index.wf_schema.resolved_value_kind(field_name),
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
///
/// Two things are accepted: an exact name in `allowed`, and Solr's per-field
/// override shape `f.<field>.<param>` where `<param>` is in both
/// `PER_FIELD_PARAMS` and `allowed` (issue #140). The shape has to be matched
/// rather than listed, since `<field>` is any field name the schema resolves.
fn check_params(state: &AppState, allowed: &[&str], params: &Params) -> Result<(), WfError> {
    // Validate only endpoints that implement this envelope parameter. Keeping
    // it inside the allowlist boundary lets admin endpoints continue ignoring
    // `omitHeader` under their default non-strict parameter policy.
    if allowed.contains(&"omitHeader") {
        params.validate_omit_header().map_err(|value| {
            WfError::bad_request(
                "wayfinder::InvalidParam",
                format!(
                    "invalid omitHeader value `{value}`; expected true, yes, on, false, no, or off"
                ),
            )
            .with_params(params)
        })?;
    }
    if !state.config.strict_params {
        return Ok(());
    }
    let accepted = |key: &str| {
        allowed.contains(&key)
            || params::split_per_field_key(key, PER_FIELD_PARAMS)
                .is_some_and(|(_, base)| allowed.contains(&base))
    };
    match params.keys().find(|key| !accepted(key)) {
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

/// Static, plausible `jvm{}`/`system{}`/`security{}` placeholders shared by
/// both `/admin/info/system` and `<core>/admin/system` (issue #59). Key shape
/// matches `solr-ref/search-api/trace/00023.json`/`00026.json`; values are
/// deliberately static (not introspected from the real host) — no fixture
/// consumer reads them, per the task spec ("matched if cheap"), and pulling
/// real host stats would make responses non-deterministic and untestable.
fn admin_info_jvm_system_security() -> (Value, Value, Value) {
    let jvm = json!({
        "version": "17.0.19 17.0.19+10",
        "name": "Wayfinder",
        "spec": {
            "vendor": "Wayfinder",
            "name": "Java Platform API Specification",
            "version": "17",
        },
        "jre": {
            "vendor": "Wayfinder",
            "version": "17.0.19",
        },
        "vm": {
            "vendor": "Wayfinder",
            "name": "OpenJDK 64-Bit Server VM",
            "version": "17.0.19+10",
        },
        "processors": 1,
        "memory": {
            "free": "0 MB",
            "total": "0 MB",
            "max": "0 MB",
            "used": "0 MB (%0.0)",
            "raw": {
                "free": 0,
                "total": 0,
                "max": 0,
                "used": 0,
                "used%": 0.0,
            },
        },
        "jmx": {
            "classpath": "wayfinder",
            "commandLineArgs": [],
            "startTime": "1970-01-01T00:00:00.000Z",
            "upTimeMS": 0,
        },
    });
    let system = json!({
        "name": "Linux",
        "arch": "unknown",
        "availableProcessors": 1,
        "systemLoadAverage": 0.0,
        "version": "unknown",
        "committedVirtualMemorySize": 0,
        "cpuLoad": 0.0,
        "freeMemorySize": 0,
        "freePhysicalMemorySize": 0,
        "freeSwapSpaceSize": 0,
        "processCpuLoad": 0.0,
        "processCpuTime": 0,
        "systemCpuLoad": 0.0,
        "totalMemorySize": 0,
        "totalPhysicalMemorySize": 0,
        "totalSwapSpaceSize": 0,
        "maxFileDescriptorCount": 0,
        "openFileDescriptorCount": 0,
    });
    let security = json!({});
    (jvm, system, security)
}

/// `/solr/admin/info/system` — server-level version handshake (issue #59).
/// Not core-scoped: no `{core}` path segment, hence no `check_core` call.
///
/// `lucene.solr-spec-version` is the ONE field `search_api_solr`'s
/// `SolrConnector::getSolrVersion()` (finding 78) actually reads, and it is
/// read here from `config.admin.reported_solr_version` — see
/// `config::Admin` for the version-choice reasoning (PRD open question 2).
async fn admin_info_system(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_params(&state, ADMIN_INFO_PARAMS, &params)?;
    let (jvm, system, security) = admin_info_jvm_system_security();
    let version = &state.config.admin.reported_solr_version;
    let mut lucene = Map::new();
    lucene.insert("solr-spec-version".to_string(), json!(version));
    lucene.insert(
        "solr-impl-version".to_string(),
        json!(format!("{version} wayfinder")),
    );
    lucene.insert("lucene-spec-version".to_string(), json!("9.12.3"));
    lucene.insert("lucene-impl-version".to_string(), json!("9.12.3 wayfinder"));
    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        },
        "mode": "std",
        "solr_home": "/var/solr/data",
        "core_root": "/var/solr/data",
        "lucene": lucene,
        "jvm": jvm,
        "security": security,
        "system": system,
    });
    Ok(axum::Json(body).into_response())
}

/// `core.schema`'s value for the core-scoped route, verbatim from the
/// captured `solr-ref/responses/admin_system.json`. NOT a free placeholder:
/// `search_api_solr`'s `SolrConnectorPluginBase.php` reads this exact field
/// via `getSchemaVersionString()`/`getSchemaTargetedSolrBranch()`/
/// `isJumpStartConfigSet()`, all of which `explode('-', $schema)` and index
/// into the result — `$parts[1]` (module version, `"4.4.0"`), `$parts[3]`
/// (targeted Solr branch, `"9.x"`), and `$parts[4]` (`"0"`) must all be
/// present and non-empty, or those calls hit an undefined array index and
/// the version handshake breaks for real (finding 78,
/// docs/solr-ref-findings.md). A shorter placeholder like
/// `"wayfinder-{core}"` only has 2 dash-separated parts and fails this.
const CORE_ADMIN_SCHEMA: &str = "drupal-4.4.0-solr-9.x-0";

/// `/solr/{core}/admin/system` — core-scoped fallback for the same
/// version-handshake (finding 78: `search_api_solr` tries this path first,
/// falling back to `/admin/info/system`). Same envelope as
/// `admin_info_system` plus the `core{}` object (`solr-ref/search-api/trace/00026.json`).
async fn core_admin_system(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, ADMIN_INFO_PARAMS, &params)?;
    let (jvm, system, security) = admin_info_jvm_system_security();
    let version = &state.config.admin.reported_solr_version;
    let mut core_response = Map::new();
    core_response.insert("schema".to_string(), json!(CORE_ADMIN_SCHEMA));
    core_response.insert("host".to_string(), json!("wayfinder"));
    core_response.insert("now".to_string(), json!("1970-01-01T00:00:00.000Z"));
    core_response.insert("start".to_string(), json!("1970-01-01T00:00:00.000Z"));
    core_response.insert(
        "directory".to_string(),
        json!({
                "cwd": "/var/wayfinder",
                "instance": format!("/var/wayfinder/{core}"),
                "data": format!("/var/wayfinder/{core}/data"),
                // ponytail: no fixture consumer reads dirimpl's value, only
                // its presence — a plausible-looking string is sufficient
                // ceiling here, not a real Tantivy directory-factory class.
                "dirimpl": "wayfinder::CoreIndex",
                "index": format!("/var/wayfinder/{core}/data/index"),
        }),
    );
    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        },
        "core": core_response,
        "mode": "std",
        "lucene": {
            "solr-spec-version": version,
            "solr-impl-version": format!("{version} wayfinder"),
            "lucene-spec-version": "9.12.3",
            "lucene-impl-version": "9.12.3 wayfinder",
        },
        "jvm": jvm,
        "security": security,
        "system": system,
    });
    Ok(axum::Json(body).into_response())
}

/// `/solr/{core}/admin/mbeans` -- the JMX-bean dump `search_api_solr`'s
/// "Solr server status" report reads (issue #158, reversing #57's descope for
/// this endpoint).
///
/// Ground truth is `solr-ref/search-api/trace/00025.json`: 48 KB of real
/// `solr:9` output, of which `SolrConnectorPluginBase::getStatsSummary()`
/// (`coverage/search_api_solr_4.4.0_source/src/SolrConnector/SolrConnectorPluginBase.php`,
/// ~L775-820) reads exactly six leaves on its Solr >= 7.0 branch -- the branch
/// that applies, since `config.admin.reported_solr_version` reports 9.x:
///
/// - `solr-mbeans.UPDATE.updateHandler.stats["UPDATE.updateHandler.docsPending"]`
/// - `...["UPDATE.updateHandler.softAutoCommitMaxTime"]`
/// - `...["UPDATE.updateHandler.deletesById"]`
/// - `...["UPDATE.updateHandler.deletesByQuery"]`
/// - `solr-mbeans.CORE.core.stats["CORE.coreName"]`
/// - `solr-mbeans.CORE.core.stats["INDEX.size"]`
///
/// All six are real state here, not placeholders: `docsPending` is the very
/// counter `autocommit_max_docs` acts on (`CoreIndex::pending_docs`), the two
/// delete figures are process-lifetime counters bumped inside
/// `CoreIndex::delete_by_ids`/`delete_by_query`, `INDEX.size` is
/// `disk_size_bytes()` through `admin_ui::human_size` (whose spelling matches
/// Solr's byte-for-byte), and `CORE.coreName` is the configured core name.
/// The pre-7.0 `UPDATEHANDLER.*` key spellings are deliberately not
/// implemented, nor is `getIndexSize()`'s `/replication` fallback.
///
/// The stats leaves are deliberately NOT uniformly typed: the trace shows the
/// counters as bare integers and the time budgets as unit-suffixed strings
/// (`softAutoCommitMaxTime: "5000ms"`), and `softAutoCommitMaxTime` is absent
/// altogether when soft autocommit is off. That inconsistency is Solr's, and
/// matching it is the contract -- see the per-key notes in the body.
///
/// Everything ELSE in the captured 48 KB -- `CONTAINER`, `ADMIN`, `QUERY`,
/// `CACHE`, per-handler request timers, Java class names, JVM/filesystem
/// figures -- has no consumer, so this handler emits only the two categories
/// the six leaves live in, with static plausible `class`/`description`
/// strings, following the `admin_info_jvm_system_security()` precedent. There
/// is deliberately no `solr-ref/manifest.tsv` row: 48 KB of Java internals
/// cannot be matched honestly, and PRD section 5's v2.75 block is the record
/// of that (so the differential harness does not enforce byte fidelity here).
///
/// `cat`/`key` are accepted and ignored.
/// ponytail: real Solr uses them to filter the dump down to one category or
/// bean; Wayfinder always returns its whole (two-category) dump. Ceiling -- a
/// client that filters gets a superset, never a missing bean, and the one
/// real consumer sends neither.
async fn admin_mbeans(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, MBEANS_PARAMS, &params)?;

    // Deliberate deviation from this crate's usual `== Some("true")` test.
    // The captured request path is verbatim
    // `admin/mbeans?stats=true?omitHeader=false&json.nl=map&json.nl=flat&wt=json`
    // -- `search_api_solr` concatenates a handler string that already carries a
    // query onto Solarium's own params -- so `stats` arrives with the raw value
    // `true?omitHeader=false`, and the captured RESPONSE shows Solr honoured
    // it anyway (`UPDATE.updateHandler.stats` is present with real values).
    // A strict equality check would answer that live client with a bean list
    // and no stats, i.e. an empty status report.
    // ponytail: a truthy-*prefix* test, not Solr's real param parsing (which
    // splits the glued query out into separate params). Ceiling -- `stats=true`
    // and `stats=false` behave exactly as documented, and anything starting
    // `true` counts as on; Wayfinder does not recover the `omitHeader=false`
    // that got glued on, and does not honour it.
    let want_stats = params.get("stats").is_some_and(|v| v.starts_with("true"));

    // `json.nl` needs no handling beyond being allowed: `Params::get` already
    // returns the FIRST value for a repeated key, which is what the trace
    // shows Solr doing (`json.nl=map` before `json.nl=flat` came back as a
    // map), and `solr-mbeans` is an object here unconditionally -- Wayfinder
    // has no `flat` named-list rendering for it to differ from.

    // The mixed typing in this map is SOLR'S, verified leaf-by-leaf against
    // trace `00025.json`, not an oversight -- do not "tidy" it into
    // consistency, that would be the bug. In the captured bytes:
    //   "UPDATE.updateHandler.docsPending"       = 0         (integer)
    //   "UPDATE.updateHandler.deletesById"       = 0         (integer)
    //   "UPDATE.updateHandler.deletesByQuery"    = 0         (integer)
    //   "UPDATE.updateHandler.softAutoCommitMaxTime" = "5000ms"  (STRING)
    //   "UPDATE.updateHandler.autoCommitMaxTime"     = "15000ms" (STRING)
    // i.e. the three counters are bare integers while the two time budgets are
    // unit-suffixed strings. Matching Solr is the contract, so this map does
    // both. `autoCommitMaxTime` is captured-but-unserved: no `search_api_solr`
    // consumer reads it (see the six-leaf list above), so only
    // `softAutoCommitMaxTime` is emitted, and only when configured.
    let mut update_stats = Map::new();
    update_stats.insert(
        "UPDATE.updateHandler.docsPending".to_string(),
        json!(state.index.pending_docs()),
    );
    update_stats.insert(
        "UPDATE.updateHandler.deletesById".to_string(),
        json!(state.index.deletes_by_id()),
    );
    update_stats.insert(
        "UPDATE.updateHandler.deletesByQuery".to_string(),
        json!(state.index.deletes_by_query()),
    );
    // Wayfinder's `autocommit_max_time` is already in ms, which is the unit
    // this key reports, so it renders straight into Solr's `"<N>ms"` spelling.
    //
    // When soft autocommit is unset the key is OMITTED, because that is what
    // Solr does -- it never puts `-1` on the wire. The `-1` in
    // `SolrConnectorPluginBase.php:787-793` is the *consumer's* initialiser for
    // a key it did not find (`$max_time = -1`, then an `isset(...)` guard), so
    // emitting `-1` ourselves would be reflecting the client's default back at
    // it as though Solr had reported it.
    if let Some(ms) = state.config.commit.autocommit_max_time {
        update_stats.insert(
            "UPDATE.updateHandler.softAutoCommitMaxTime".to_string(),
            json!(format!("{ms}ms")),
        );
    }
    let size_bytes = state.index.disk_size_bytes();
    let core_stats = json!({
        "CORE.coreName": state.core_name,
        "INDEX.size": admin_ui::human_size(size_bytes),
        // Parity, not invention: `INDEX.sizeInBytes` is genuinely in the trace
        // (`21607`, sitting beside `INDEX.size: "21.1 KB"` -- the same figure
        // rounded), so emitting it matches captured Solr rather than adding a
        // Wayfinder-only key. No client reads it; it is the unrounded value,
        // exactly as `/ui/stats` shows it.
        "INDEX.sizeInBytes": size_bytes,
    });

    let mut update_handler = Map::new();
    update_handler.insert(
        "class".to_string(),
        json!("org.apache.solr.update.DirectUpdateHandler2"),
    );
    update_handler.insert(
        "description".to_string(),
        json!("Update handler that efficiently directly updates the on-disk main lucene index"),
    );
    let mut core_bean = Map::new();
    core_bean.insert("class".to_string(), json!(state.core_name));
    core_bean.insert("description".to_string(), json!("SolrCore"));
    // The `stats` sub-object appears only under `stats=true` -- without it Solr
    // returns the bean list alone, and the coverage probe for
    // `admin.mbeans.solr-mbeans` GETs the endpoint with no `stats` at all.
    if want_stats {
        update_handler.insert("stats".to_string(), Value::Object(update_stats));
        core_bean.insert("stats".to_string(), core_stats);
    }

    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        },
        "solr-mbeans": {
            "CORE": { "core": core_bean },
            "UPDATE": { "updateHandler": update_handler },
        },
    });
    Ok(axum::Json(body).into_response())
}

/// The `class` Wayfinder reports for a built-in type name. Solr class names,
/// because that is the vocabulary the wire format is written in — but they
/// describe Solr's taxonomy, not Wayfinder's storage: `int`/`long` are both
/// an i64 in Tantivy, and `float`/`double` are both an f64. The distinction
/// survives here only because the type *names* do.
fn solr_class_for_builtin(name: &str) -> &'static str {
    match name {
        "string" | "keyword" => "solr.StrField",
        "int" => "solr.IntPointField",
        "long" => "solr.LongPointField",
        "float" => "solr.FloatPointField",
        "double" => "solr.DoublePointField",
        "date" => "solr.DatePointField",
        // `text_general`, `text_en` and every `text_<code>` preset: analyzed
        // text, which is Solr's `TextField`.
        _ => "solr.TextField",
    }
}

/// One `fieldTypes[]` entry, in the shape
/// `solr-ref/search-api/trace/00020.json` shows.
///
/// `name` and `class` are the only keys real Solr puts on every entry, and
/// the only two this response ever varies. The three flags below are the
/// *type-level defaults* Wayfinder genuinely applies, which is what Solr's
/// own type-level `stored`/`multiValued`/`docValues` mean too: in Wayfinder
/// they are per-field `[[fields]]` flags that all default to false, while
/// indexing is unconditional (every declared field gets indexing options —
/// `schema::build`), so `indexed` is true for every type there is.
///
/// Deliberately absent: `indexAnalyzer`/`queryAnalyzer`/`analyzer`, which
/// real Solr fills with Lucene factory chains
/// (`solr.StandardTokenizerFactory`, `solr.SnowballPorterFilterFactory`, ...).
/// Wayfinder's analysis is Tantivy's, not Lucene's, so any chain emitted here
/// would be fiction — and no client reads it (see `schema_fieldtypes`). This
/// omission is the documented deliberate divergence for this endpoint
/// (PRD §5).
///
/// Deliberately *added*, the other direction: real Solr emits these four
/// sparsely — in `trace/00020.json` `indexed` appears on 4 of 41 entries and
/// `docValues` on 12 — because Solr only serialises an attribute that was
/// written into `managed-schema`, leaving the rest implied by the Lucene
/// `class` default. Wayfinder emits all four on every entry instead, on
/// purpose: reproducing the sparseness would mean encoding Lucene's per-class
/// default table (`solr.BinaryField` implies X, `solr.BoolField` implies Y...)
/// for classes Wayfinder has no implementation of, which is fiction of the
/// same kind as the analyzer chains, whereas these four values are Wayfinder's
/// real uniform type-level defaults. It is harmless to the one real consumer:
/// `isPartOfSchema` does an `in_array` over `name` and reads no attribute at
/// all, and every Solr client tolerates a present-but-default attribute since
/// Solr itself emits them whenever a schema author wrote them out explicitly.
/// Recorded as an addition (not just an omission) in PRD §5.
fn field_type_entry(name: &str, class: &str) -> Value {
    json!({
        "name": name,
        "class": class,
        "indexed": true,
        "stored": false,
        "multiValued": false,
        "docValues": false,
    })
}

/// `/solr/{core}/schema/fieldtypes` — the field types this core can actually
/// resolve (issue #156, resolving #142 as In).
///
/// The one real consumer is `search_api_solr`'s
/// `SearchApiSolrBackend::isPartOfSchema('fieldTypes', 'text_<lang>', ...)`,
/// which does an `in_array()` **name-membership** check and nothing else —
/// it never looks at analyzers. Its caller `getSchemaLanguageStatistics()`
/// turns each hit into a green "language supported" row in Drupal's admin UI.
/// So the names are load-bearing and everything else is not, which sets the
/// honesty rule for this handler: report exactly the types Wayfinder really
/// resolves — the live schema's `[[field_types]]` chains plus every built-in
/// `schema::resolve_type` accepts — and nothing invented to look more
/// Solr-like. Padding the list would flip today's misreport-downward (the
/// 404 makes every language read as unsupported) into a misreport-upward,
/// which is worse, because nobody investigates green.
///
/// Same precedent as `admin_info_jvm_system_security`: real values where a
/// real consumer exists (the `name`s, derived per request from
/// `AppState.index.wf_schema` — the same struct `/ui/schema` reads, not a
/// second schema-reading path), static plausible placeholders where none does
/// (`class` and the three default flags, see `field_type_entry`).
///
/// Scope boundary — do not widen: this is the only schema endpoint in the
/// coverage contract, and the only one with client evidence. `/schema`,
/// `/schema/fields`, `/schema/dynamicfields`, `/schema/copyfields` and the
/// rest of Solr's Schema API stay on the Solr 9.x parity roadmap (PRD §5).
/// This route existing is not an invitation to add its siblings.
async fn schema_fieldtypes(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, SCHEMA_FIELDTYPES_PARAMS, &params)?;

    let custom = &state.index.wf_schema.field_types;
    let mut field_types: Vec<Value> = custom
        .iter()
        // A custom chain is always analyzed text (`resolve_type` maps it to
        // `ResolvedType::Text`).
        .map(|ft| field_type_entry(&ft.name, "solr.TextField"))
        .collect();
    field_types.extend(
        schema::builtin_type_names()
            .iter()
            // Unreachable since issue #170: `schema::parse` rejects a
            // `[[field_types]]` name that collides with any built-in, so no
            // custom chain can shadow one and this filter never drops a name.
            // Kept as defence-in-depth -- if the reservation is ever narrowed,
            // this still keeps a duplicate out of the `in_array` name list
            // `isPartOfSchema` reads.
            .filter(|name| !custom.iter().any(|ft| &&ft.name == name))
            .map(|name| field_type_entry(name, solr_class_for_builtin(name))),
    );

    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        },
        "fieldTypes": field_types,
    });
    Ok(axum::Json(body).into_response())
}

/// The `index{}` keys of `/admin/luke` that describe *Lucene's* identity for a
/// segment set rather than the core's contents: static, plausible placeholders,
/// same precedent as `admin_info_jvm_system_security`. Nothing reads them —
/// the endpoint's only client consumer (`SearchApiSolrBackend::getLuke()`)
/// reads `index.numDocs` and nothing else — and each one would be fiction if
/// computed:
///
/// - `version` is Lucene's monotonic index-version counter, `current` its
///   "is the open reader the newest commit" flag (real Solr reports `false`
///   here even on a quiet core, per the trace); tantivy exposes neither.
/// - `directory` is a Java class-name dump of the `Directory` chain
///   (`NRTCachingDirectory(MMapDirectory@...)`) — Wayfinder has an MmapDirectory
///   from tantivy, not that stack, so any string here names classes that do
///   not exist in this process.
/// - `segmentsFile`/`segmentsFileSizeInBytes` name the Lucene `segments_N`
///   commit point; tantivy's equivalent is `meta.json` and its generation is
///   not a Lucene one. Solr itself reports `-1` for the size in the trace.
/// - `indexHeapUsageBytes` is Lucene's per-reader RAM accounting, which
///   tantivy does not report; real Solr also omits it in the captured trace.
/// - `userData` is the Lucene commit-userdata map, empty in the trace.
/// - `lastModified` is only emitted by real Solr when the commit carries a
///   `commitTimeMSec` in that userdata; the trace has none, so the honest
///   mirror of the captured shape is to leave both out together.
fn admin_luke_index_placeholders() -> Vec<(&'static str, Value)> {
    vec![
        ("version", json!(1)),
        ("current", json!(false)),
        ("directory", json!("org.apache.lucene.store.MMapDirectory")),
        ("segmentsFile", json!("segments_1")),
        ("segmentsFileSizeInBytes", json!(-1)),
        ("userData", json!({})),
    ]
}

/// One `fields{}` entry: the field's declared type plus the flags Wayfinder
/// genuinely applies to it, read off the live `[[fields]]` config.
///
/// Deliberately absent — and this is the reason the endpoint carries no
/// `manifest.tsv` row (PRD section 5, v2.75) — are the Lucene-internal keys the
/// trace shows: the `schema`/`index` flag strings (`ITS-----OF-----`, whose
/// letters are Lucene `FieldInfo` bits), `topTerms` and `histogram`. Wayfinder
/// has no Lucene index internals to read them from, nobody reads them (see
/// `admin_luke`), and a plausible-looking fake flag string would be worse than
/// an omitted key because it would read as authoritative. `docs` (per-field
/// document frequency) is out for the same reason as `numTerms`: it is a term-
/// dictionary walk this endpoint's one consumer does not ask for.
///
/// The boolean flags are the other direction — an honest *addition*, exactly as
/// in `field_type_entry` (issue #156): they carry the same information the
/// dropped `schema` string encodes, in the vocabulary Solr's own Schema API
/// uses (`stored`/`multiValued`/`docValues`/`required`), and are real per-field
/// values rather than a decoded fiction. `indexed` is unconditionally true
/// because `schema::build` gives every declared field indexing options; `fast`
/// is Wayfinder's spelling of docValues.
fn luke_field_entry(field: &schema::FieldConfig) -> Value {
    json!({
        "type": field.type_,
        "indexed": true,
        "stored": field.stored,
        "multiValued": field.multi_valued,
        "docValues": field.fast,
        "required": field.required,
    })
}

/// `/solr/{core}/admin/luke` — index statistics and the field list (issue
/// #157, reversing the #57 descope for this endpoint).
///
/// The one real consumer is `search_api_solr`'s
/// `SearchApiSolrBackend::getServerInfo()` path, which calls `getLuke()` and
/// reads `$data['index']['numDocs']` — that is the whole consumption, and it
/// becomes the "N items indexed" line on Drupal's server-status screen. So the
/// four count fields are real, read per request from the live core's searcher
/// (`CoreIndex::doc_count`/`deleted_doc_count`/`segment_count`, the same
/// searcher `/select` answers from, so this page cannot disagree with a query),
/// and the Lucene-identity fields are static placeholders
/// (`admin_luke_index_placeholders` names each one and why).
///
/// `maxDoc` is `numDocs + deletedDocs` because that is what Lucene's maxDoc is:
/// the addressable doc-id space of the current segments, tombstones included.
///
/// `fields{}` is derived from the live `[[fields]]` schema, not the index's
/// `FieldInfo`s. That means dynamic-field *instances* do not appear: Wayfinder
/// stores every dynamic value in the shared `_dynamic`/`_dynamic_text` tantivy
/// fields (`schema::DYNAMIC_FIELD`), so there is no per-instance field in the
/// index to enumerate, and inventing one entry per observed dynamic key would
/// be a term walk reporting a field that does not exist in the schema.
async fn admin_luke(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, ADMIN_LUKE_PARAMS, &params)?;

    let num_docs = state.index.doc_count();
    let deleted_docs = state.index.deleted_doc_count();
    let mut index = Map::new();
    index.insert("numDocs".to_string(), json!(num_docs));
    index.insert("maxDoc".to_string(), json!(num_docs + deleted_docs));
    index.insert("deletedDocs".to_string(), json!(deleted_docs));
    index.insert("hasDeletions".to_string(), json!(deleted_docs > 0));
    index.insert(
        "segmentCount".to_string(),
        json!(state.index.segment_count()),
    );
    for (key, value) in admin_luke_index_placeholders() {
        index.insert(key.to_string(), value);
    }

    let fields: Map<String, Value> = state
        .index
        .wf_schema
        .fields
        .iter()
        .map(|field| (field.name.clone(), luke_field_entry(field)))
        .collect();

    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        },
        "index": index,
        "fields": fields,
    });
    Ok(axum::Json(body).into_response())
}

/// One `/update` command, in the order the body listed it. A bare-array body
/// (the pre-#9 adds-only form) becomes a run of `Add`s; a command-object body
/// becomes one entry per key *occurrence*, duplicates included.
enum UpdateCommand {
    Add(Value),
    DeleteIds(Vec<String>),
    DeleteQuery(String),
    Commit,
}

/// The top level of an `/update` POST body, deserialized *without* going
/// through `serde_json::Value` — `Value::Object` is a map, so it collapses a
/// duplicate JSON key to the last occurrence, and Solr's command format
/// repeats `add` once per document (six times in `search-api/trace/00001.json`,
/// and `update_repeated_add_batch.json` confirms real Solr executes every
/// one). Driving the top level from `MapAccess` keeps each occurrence, in
/// order; each command's *value* is still an ordinary `Value`, which is all
/// the command bodies need.
///
/// ponytail: only the top level is duplicate-tolerant. A repeated key *inside*
/// a command value (`{"add":{"doc":{...},"doc":{...}}}`) still collapses to
/// the last occurrence — unobserved in any capture or trace, and Solr's own
/// `JsonLoader` reads a single `doc` per add.
enum UpdateBody {
    /// A bare JSON array of documents: every element is an add.
    Docs(Vec<Value>),
    /// A command object, as `(key, value)` pairs in body order with duplicate
    /// keys preserved.
    Commands(Vec<(String, Value)>),
    /// Anything else (a scalar, `null`), kept so the caller can name it in the
    /// "must be an array or a command object" error.
    Other(Value),
}

impl<'de> serde::Deserialize<'de> for UpdateBody {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct BodyVisitor;

        /// The scalar arms all mean the same thing to `parse_update_commands`
        /// ("not an array, not an object"); they exist only so the error can
        /// echo the offending body.
        macro_rules! scalar_arm {
            ($name:ident, $ty:ty) => {
                fn $name<E: serde::de::Error>(self, v: $ty) -> Result<UpdateBody, E> {
                    Ok(UpdateBody::Other(Value::from(v)))
                }
            };
        }

        impl<'de> serde::de::Visitor<'de> for BodyVisitor {
            type Value = UpdateBody;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON array of documents or an update command object")
            }

            scalar_arm!(visit_bool, bool);
            scalar_arm!(visit_i64, i64);
            scalar_arm!(visit_u64, u64);
            scalar_arm!(visit_f64, f64);
            scalar_arm!(visit_str, &str);

            fn visit_unit<E: serde::de::Error>(self) -> Result<UpdateBody, E> {
                Ok(UpdateBody::Other(Value::Null))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<UpdateBody, A::Error> {
                let mut docs = Vec::new();
                while let Some(doc) = seq.next_element::<Value>()? {
                    docs.push(doc);
                }
                Ok(UpdateBody::Docs(docs))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<UpdateBody, A::Error> {
                let mut pairs = Vec::new();
                while let Some((key, val)) = map.next_entry::<String, Value>()? {
                    pairs.push((key, val));
                }
                Ok(UpdateBody::Commands(pairs))
            }
        }

        de.deserialize_any(BodyVisitor)
    }
}

/// Parses a `/update` POST body into an ordered command list (finding 46).
/// Two shapes: the pre-#9 bare JSON array of docs (all adds, unchanged), and
/// Solr's command-object form — `{"add":{"doc":{...}}, "delete":{"id":...} |
/// [...] | {"query":"..."}, "commit":{}}` — where any key may repeat.
///
/// Order is the body's order and `update` below executes it as such, because
/// Solr does: `update_repeated_add_delete_before.json` deletes `r4` and then
/// re-adds it in one body, and the following select still finds `r4`
/// (finding 96). A body-order-independent execution (all adds, then all
/// deletes) loses that doc.
///
/// A malformed command aborts the whole body before anything executes, which
/// is also what Solr does: `update_repeated_add_missing_doc.json` is a 400 and
/// the valid add that *preceded* the bad one never lands
/// (`update_select_after_repeated_add_missing_doc.json`, `numFound` 0); same
/// for an unknown command key (`update_repeated_add_unknown_key.json`).
fn parse_update_commands(body: &[u8]) -> Result<Vec<UpdateCommand>, String> {
    let parsed: UpdateBody =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let pairs = match parsed {
        UpdateBody::Docs(docs) => {
            return Ok(docs.into_iter().map(UpdateCommand::Add).collect());
        }
        UpdateBody::Commands(pairs) => pairs,
        UpdateBody::Other(other) => {
            return Err(format!(
                "update body must be a JSON array of documents or a command object, got {other}"
            ));
        }
    };

    let mut commands = Vec::with_capacity(pairs.len());
    for (key, val) in pairs {
        match key.as_str() {
            "add" => {
                let doc = val
                    .get("doc")
                    .cloned()
                    .ok_or_else(|| "\"add\" command is missing \"doc\"".to_string())?;
                commands.push(UpdateCommand::Add(doc));
            }
            "delete" => match val {
                Value::Array(ids) => {
                    let mut delete_ids = Vec::with_capacity(ids.len());
                    for id in ids {
                        let id = id.as_str().ok_or_else(|| {
                            "\"delete\" id list entries must be strings".to_string()
                        })?;
                        delete_ids.push(id.to_string());
                    }
                    commands.push(UpdateCommand::DeleteIds(delete_ids));
                }
                Value::Object(ref dm) if dm.contains_key("id") => {
                    let id = dm["id"]
                        .as_str()
                        .ok_or_else(|| "\"delete.id\" must be a string".to_string())?;
                    commands.push(UpdateCommand::DeleteIds(vec![id.to_string()]));
                }
                Value::Object(ref dm) if dm.contains_key("query") => {
                    let q = dm["query"]
                        .as_str()
                        .ok_or_else(|| "\"delete.query\" must be a string".to_string())?;
                    commands.push(UpdateCommand::DeleteQuery(q.to_string()));
                }
                other => {
                    return Err(format!("unsupported \"delete\" command shape: {other}"));
                }
            },
            "commit" => commands.push(UpdateCommand::Commit),
            other => return Err(format!("unsupported update command `{other}`")),
        }
    }
    Ok(commands)
}

/// The bare `{"responseHeader":{"status":0,"QTime":0}}` envelope every
/// `/update` success answers with, for every command shape (finding 46) —
/// never a `params` echo, never per-command keys.
///
/// Under `omitHeader=true` that leaves `{}`: the bare envelope has no other
/// key to survive the header's removal.
///
/// ponytail: unfixtured for `/update` specifically. `search_api_solr` only
/// ever sends `omitHeader=false` here (`solr-ref/search-api/trace/00001.json`),
/// so no capture shows `/update` under `omitHeader=true`. This generalizes
/// from `/select`/`/mlt`/`/terms`, which all gate on the same param and are
/// fixture-pinned; the alternative reading ("`/update` never suppresses") is
/// possible but has nothing behind it. A capture of a real `solr:9`
/// `/update?commit=true&omitHeader=true` settles it.
fn update_success(params: &Params) -> Response {
    if params.omit_header() {
        return axum::Json(json!({})).into_response();
    }
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
    let params = Params::parse(query.as_deref().unwrap_or("")).allow_omit_header();
    // `/update` errors carry a responseHeader but no params echo — Solr does
    // not echo params on this endpoint (`err_update_bad_json.json`).
    let update_err = |class: &'static str, msg: String| {
        WfError::bad_request(class, msg)
            .with_params(&params)
            .envelope(Envelope::NoParams)
    };
    check_core(&state, &core, &params, Envelope::NoParams)?;
    check_params(&state, UPDATE_PARAMS, &params)
        .map_err(|e| e.with_params(&params).envelope(Envelope::NoParams))?;

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
            WfError::internal("wayfinder::CommitError", e.to_string())
                .with_params(&params)
                .envelope(Envelope::NoParams)
        })?;
        return Ok(update_success(&params));
    }

    // `overwrite=false` skips the default replace-by-uniqueKey step
    // (finding 48b); every other value (including absent) is Solr's default
    // `overwrite=true`.
    let overwrite = params.get("overwrite") != Some("false");

    let commands =
        parse_update_commands(&body).map_err(|msg| update_err("wayfinder::BadUpdateBody", msg))?;

    // Execute in body order (finding 96). Consecutive adds still go to
    // `add_documents` as one batch — the bare-array body is the whole-batch
    // case — but a delete or commit between them flushes first, so a
    // delete-then-re-add of the same id keeps the re-added doc.
    let mut pending_adds: Vec<Value> = Vec::new();
    macro_rules! flush_adds {
        () => {
            if !pending_adds.is_empty() {
                state
                    .index
                    .add_documents(&pending_adds, overwrite)
                    .map_err(|e| update_err("wayfinder::IndexError", e.to_string()))?;
                pending_adds.clear();
            }
        };
    }
    for command in commands {
        match command {
            UpdateCommand::Add(doc) => pending_adds.push(doc),
            UpdateCommand::DeleteIds(ids) => {
                flush_adds!();
                state
                    .index
                    .delete_by_ids(&ids)
                    .map_err(|e| update_err("wayfinder::IndexError", e.to_string()))?;
            }
            UpdateCommand::DeleteQuery(q) => {
                flush_adds!();
                state
                    .index
                    .delete_by_query(&q, &state.index.wf_schema.core.default_field)
                    .map_err(|e| update_err("wayfinder::IndexError", e.to_string()))?;
            }
            // A body `commit` commits what precedes it, there and then: any
            // add after it is a separate, still-uncommitted batch unless a
            // `commit`/`softCommit` param commits again below. Unfixtured
            // (no capture puts an add after a body `commit`), inferred from
            // the command-stream semantics finding 96 pins directly for
            // delete/add; deferring this commit to the end of the request
            // instead is a mutant the rest of the suite does NOT catch, so
            // `an_add_after_a_body_commit_key_stays_uncommitted` in
            // `tests/update_pipeline.rs` guards it and carries the ceiling.
            UpdateCommand::Commit => {
                flush_adds!();
                state.index.commit().map_err(|e| {
                    WfError::internal("wayfinder::CommitError", e.to_string())
                        .with_params(&params)
                        .envelope(Envelope::NoParams)
                })?;
            }
        }
    }
    flush_adds!();

    // `commit=true` (existing) and `softCommit=true` both mean "commit and
    // reload now" — per the task spec's softCommit note, Tantivy has no
    // in-memory-searchable segment for a real soft commit to leave
    // uncommitted-but-visible, so Wayfinder's softCommit is a hard commit
    // too (wire-visible behaviour matches Solr; durability is only ever
    // stronger, never weaker). A `commit` key in the body does the same, but
    // in body order, in the loop above.
    let commit_now =
        params.get("commit") == Some("true") || params.get("softCommit") == Some("true");
    if commit_now {
        state.index.commit().map_err(|e| {
            WfError::internal("wayfinder::CommitError", e.to_string())
                .with_params(&params)
                .envelope(Envelope::NoParams)
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

    Ok(update_success(&params))
}

/// Maps a `CoreIndex::parse_query` failure to the right `WfError` shape:
/// finding 59's one 500 (a regex that parses as a query but fails automaton
/// compilation — `query::QueryError::RegexCompile`, carried through
/// `parse_query`'s `anyhow::Error` via `From`) gets the trace-carrying,
/// no-`metadata` envelope `err_regex_bad_class.json` pins; any other
/// unexpected failure (`QueryError::Internal` — e.g. a term-dictionary I/O
/// error) is a plain 500 with the ordinary `metadata` shape, not dressed up
/// in the regex one; every other failure (unknown field, bad syntax, an
/// unclosed regex, a prefix query on a numeric field) is an ordinary 400
/// `wayfinder::SyntaxError`, as before this issue.
fn query_parse_error(e: anyhow::Error, params: &Params) -> WfError {
    match e.downcast_ref::<query::QueryError>() {
        Some(query::QueryError::RegexCompile(_)) => {
            WfError::internal("wayfinder::RegexCompileError", e.to_string())
                .with_trace(e.to_string())
                .with_params(params)
        }
        Some(query::QueryError::Internal(_)) => {
            WfError::internal("wayfinder::QueryError", e.to_string()).with_params(params)
        }
        _ => WfError::bad_request("wayfinder::SyntaxError", e.to_string()).with_params(params),
    }
}

async fn select(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or("")).allow_omit_header();
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
            // `defType=edismax` (issue #7, PRD §5 v1 exception) switches only
            // `q`'s own parser to the dismax-style qf/pf/mm/tie/boost/bq
            // composition (`CoreIndex::parse_edismax_query`) — `fq` below is
            // untouched, always the plain Solr query grammar, matching real
            // Solr (`defType` only ever governs `q`).
            let query = if params.get("defType") == Some("edismax") {
                let qf = params.get("qf").unwrap_or("");
                let pf = params.get("pf");
                let mm = params.get("mm");
                let tie: f32 = params
                    .get("tie")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                let bq: Vec<String> = params
                    .get_all("bq")
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                // `boost` is documented as a function query in real Solr (a
                // plain number like `boost=2` is just its simplest constant
                // function), but Wayfinder has no function-query evaluator
                // (PRD v1 scope explicitly excludes it, same as `bf` --
                // issue #108/finding 75). A non-numeric `boost` value (e.g.
                // `recip(rord(date),1,1000,1000)`) therefore fails `.parse()`
                // and falls back to `None` here -- accepted and silently
                // ignored, not rejected, matching the same unknown-value
                // leniency `bf` gets rather than a 400 (issue #110).
                let boost: Option<f32> = params.get("boost").and_then(|s| s.parse().ok());
                state
                    .index
                    .parse_edismax_query(q, &default_field, qf, pf, mm, tie, &bq, boost)
                    .map_err(|e| query_parse_error(anyhow::Error::from(e), &params))?
            } else {
                state
                    .index
                    .parse_query(q, &default_field)
                    .map_err(|e| query_parse_error(e, &params))?
            };

            let mut filter_queries = Vec::new();
            for fq in params.get_all("fq") {
                filter_queries.push(
                    state
                        .index
                        .parse_query(fq, &default_field)
                        .map_err(|e| query_parse_error(e, &params))?,
                );
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
    for (score, addr) in page.iter().copied() {
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

    // Facet and stats counts are both aggregated over a *real* query (`q` AND
    // every `fq`), not over `hits`: Solr enumerates the field's whole term
    // dictionary / metric aggregation over the matching set, which the hit
    // list cannot see (`search` filters post-hoc with `retain`, so the
    // Boolean query is rebuilt here rather than reused). Shared between both
    // features rather than built twice.
    let base: facet::BaseClauses = match &parsed {
        // No `q` matches nothing, so neither does any facet/stats block — but
        // the term dictionary is still enumerated, at 0, exactly as
        // `facet_zero` shows for a `q` that matches nothing, and `stats_zero`
        // shows for stats.
        None => vec![(Occur::Must, Box::new(EmptyQuery) as Box<dyn Query>)],
        Some((query, filter_queries)) => std::iter::once((Occur::Must, query.box_clone()))
            .chain(
                filter_queries
                    .iter()
                    .map(|fq| (Occur::Must, fq.box_clone())),
            )
            .collect(),
    };

    // `facet=true` gates the whole block; `facet.field` alone does not turn
    // faceting on and the key stays absent (findings fact 4). Computed *before*
    // `responseHeader` is built, not after: Solr's own `responseHeader` key
    // order is `warnings, status, QTime, params` — `warnings` leads, it does
    // not trail (finding 29 / issue #24) — and `serde_json`'s `preserve_order`
    // feature (issue #25) means the order keys are inserted in is now the order
    // they are emitted in, so `warnings` has to be known before the object
    // literal is written.
    let facet_result = if params.get("facet") == Some("true") {
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

    // `stats=true` gates the whole `stats` block the same way `facet=true`
    // gates `facet_counts` — `stats.field` alone does not turn it on (mirrors
    // `facet.field`'s own convention, and matches `stats_key_absent_without_stats_true`).
    let stats_result = if params.get("stats") == Some("true") {
        Some(stats::stats(&state.index, &params, &base).map_err(|e| {
            WfError::bad_request("wayfinder::StatsError", e.to_string())
                .with_params(&params)
                .with_response(Value::Object(response.clone()))
        })?)
    } else {
        None
    };

    // `hl=true` gates the whole `highlighting` block (finding 52); it is
    // keyed by unique-key value over the docs actually returned on this
    // page, matching `response.docs`'s own pagination.
    let highlighting_result = if params.get("hl") == Some("true") {
        let result = match &parsed {
            Some((query, _)) => highlight::highlighting(
                &state.index,
                &params,
                &default_field,
                query.as_ref(),
                &page,
                &state.index.wf_schema.core.unique_key,
            ),
            // No `q` matches nothing, so `page` is always empty here too —
            // an empty `highlighting` object, not an absent key.
            None => Ok(Value::Object(Map::new())),
        }
        .map_err(|e| {
            // An undefined/non-text `hl.fl` field is a request-input
            // problem (`highlight::InvalidHlField`), rendered as Solr's own
            // 400 -- mirroring `facet.field`'s own unknown-field handling
            // just above, including carrying the base query's already-built
            // `response` block alongside `error` (issue #35's precedent;
            // unfixtured for `hl.fl` specifically, since no captured
            // `hl_*` fixture exercises an invalid field, but the shape
            // should be consistent with `facet_unknown_field.json`'s). A
            // failure that isn't request-input (a genuine Tantivy/searcher
            // error) stays a 500.
            if e.downcast_ref::<highlight::InvalidHlField>().is_some() {
                WfError::bad_request("wayfinder::HighlightError", e.to_string())
                    .with_params(&params)
                    .with_response(Value::Object(response.clone()))
            } else {
                WfError::internal("wayfinder::HighlightError", e.to_string()).with_params(&params)
            }
        })?;
        Some(result)
    } else {
        None
    };

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

    // `omitHeader=true` drops the header and nothing else (issue #143); the
    // `response` block and every optional block below are unaffected. See
    // `Params::omit_header` for the ground truth and the error-path ceiling.
    let mut body = if params.omit_header() {
        json!({ "response": response })
    } else {
        json!({
            "responseHeader": response_header,
            "response": response,
        })
    };

    if let Some((facet_counts, _)) = facet_result {
        body["facet_counts"] = facet_counts;
    }
    if let Some(stats) = stats_result {
        body["stats"] = stats;
    }

    if let Some(highlighting) = highlighting_result {
        body["highlighting"] = highlighting;
    }

    Ok(axum::Json(body).into_response())
}

/// `GET /solr/<core>/mlt` (issue #6, PRD §5). `q` resolves the source
/// document the same way `/select`'s `q` does; `mlt.fl` names which stored
/// fields to mine for interesting terms (every declared field if absent);
/// `fl`/`rows`/`start` page the similar-docs result set exactly as
/// `/select` does. See `docs/solr-ref-findings.md` findings 60-67 for the
/// captured envelope shape this mirrors.
async fn mlt(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or("")).allow_omit_header();
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, MLT_PARAMS, &params)?;

    let default_field = params
        .get("df")
        .unwrap_or(&state.index.wf_schema.core.default_field)
        .to_string();

    let fl: Option<Vec<String>> = params
        .get("fl")
        .map(|fl| fl.split(',').map(|s| s.trim().to_string()).collect());
    let wants_score = fl
        .as_deref()
        .is_some_and(|fl| fl.iter().any(|f| f == "score"));

    let start: usize = params
        .get("start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let rows: usize = params
        .get("rows")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .min(state.config.query.rows_limit);

    // `fq` on `/mlt` filters the *similar-docs* set only — never the seed
    // resolution (finding 98): `mlt_fq_seed_not_filtered.json` sends an `fq`
    // that excludes the seed doc's own category and real Solr still resolves
    // `match` to it. Parsed here (not inside the `q` arm) so a malformed `fq`
    // is a 400 whether or not `q` resolved anything, the same as `/select`.
    // Repeated `fq` params AND together, also as on `/select`
    // (`mlt_fq_multiple_and.json`).
    let mut filter_queries = Vec::new();
    for fq in params.get_all("fq") {
        filter_queries.push(state.index.parse_query(fq, &default_field).map_err(|e| {
            WfError::bad_request("wayfinder::SyntaxError", e.to_string()).with_params(&params)
        })?);
    }

    // `q` resolves the source document exactly as `/select`'s `q` does — no
    // `q` matches nothing (findings fact per `/select`, extended here rather
    // than defaulting to `*:*`).
    let hits = match params.get("q") {
        None => Vec::new(),
        Some(q) => {
            let query = state.index.parse_query(q, &default_field).map_err(|e| {
                WfError::bad_request("wayfinder::SyntaxError", e.to_string()).with_params(&params)
            })?;
            state.index.search(query.as_ref(), &[], &[]).map_err(|e| {
                WfError::internal("wayfinder::SearchError", e.to_string()).with_params(&params)
            })?
        }
    };

    // Solr's `/mlt` resolves exactly one source document from `q` — `match`
    // reports the real `numFound` for `q` but only ever renders that one doc
    // (every captured fixture has `match.numFound: 1` with a one-element
    // `docs`; `match.numFound: 0` with an empty `docs` when `q` matched
    // nothing, finding 63).
    //
    // Which one is `mlt.match.offset`'s job (default 0, the top hit):
    // `mlt_match_offset.json` sends `mlt.match.offset=1` against a 5-hit `q`
    // and gets the *second* hit as the seed, with `match.start: 1` (finding
    // 99) — the seed genuinely changes, it is not a cosmetic echo.
    //
    // ponytail: an offset past the last hit resolves no seed at all here (so
    // `match.docs` is empty and `response` is `null`, finding 63's shape). No
    // fixture pins real Solr's out-of-range behaviour; the ceiling is the
    // in-range case the capture covers, and
    // `tests/mlt.rs::mlt_match_offset_past_the_last_hit_resolves_no_seed` pins
    // this choice so it cannot drift to `.or(hits.first())` or a clamp.
    let match_offset: usize = params
        .get("mlt.match.offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let source = hits.get(match_offset).copied();

    let max_score = |hits: &[(Score, tantivy::DocAddress)]| {
        hits.iter()
            .map(|(score, _)| *score)
            .fold(None, |acc: Option<Score>, s| {
                Some(acc.map_or(s, |a| a.max(s)))
            })
    };

    let mut match_block = Map::new();
    match_block.insert("numFound".to_string(), json!(hits.len()));
    match_block.insert("start".to_string(), json!(match_offset));
    if wants_score && let Some(score) = max_score(&hits) {
        match_block.insert("maxScore".to_string(), json!(score));
    }
    match_block.insert("numFoundExact".to_string(), json!(true));
    let match_docs = match source {
        Some((score, addr)) => vec![
            state
                .index
                .render_doc(addr, fl.as_deref(), Some(score))
                .map_err(|e| {
                    WfError::internal("wayfinder::DocError", e.to_string()).with_params(&params)
                })?,
        ],
        None => Vec::new(),
    };
    match_block.insert("docs".to_string(), json!(match_docs));

    // `response` is the literal JSON `null` when `q` matched no source
    // document at all (finding 63) — not the empty-object shape used below
    // for a source doc with no interesting terms.
    let response_value: Value = match source {
        None => Value::Null,
        Some((_, addr)) => {
            let mlt_fl: Option<Vec<String>> = params
                .get("mlt.fl")
                .map(|fl| fl.split(',').map(|s| s.trim().to_string()).collect());

            // Solr's defaults: mintf=2, mindf=5, maxqt=25, no word-length or
            // max-doc-frequency gate, boost=false (equal-weighted terms).
            let opts = core_index::MltOptions {
                min_term_frequency: Some(
                    params
                        .get("mlt.mintf")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(2),
                ),
                min_doc_frequency: Some(
                    params
                        .get("mlt.mindf")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(5),
                ),
                max_doc_frequency: params.get("mlt.maxdf").and_then(|s| s.parse().ok()),
                max_query_terms: Some(
                    params
                        .get("mlt.maxqt")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(25),
                ),
                min_word_length: params.get("mlt.minwl").and_then(|s| s.parse().ok()),
                max_word_length: params.get("mlt.maxwl").and_then(|s| s.parse().ok()),
                // Tantivy's own boost weighting (relative term score, best
                // term normalised to 1.0) only when `mlt.boost=true`; equal
                // weight (no `BoostQuery` wrapper at all) otherwise.
                boost_factor: (params.get("mlt.boost") == Some("true")).then_some(1.0),
            };
            let (mlt_query, _scored_terms) = state
                .index
                .mlt_query(addr, mlt_fl.as_deref(), opts)
                .map_err(|e| {
                WfError::internal("wayfinder::DocError", e.to_string()).with_params(&params)
            })?;

            let mut mlt_hits = state
                .index
                .search(&mlt_query, &filter_queries, &[])
                .map_err(|e| {
                    WfError::internal("wayfinder::SearchError", e.to_string()).with_params(&params)
                })?;
            // The seed document itself is not a "similar" result.
            mlt_hits.retain(|(_, a)| *a != addr);

            let num_found = mlt_hits.len();
            let page: Vec<_> = mlt_hits.iter().skip(start).take(rows).copied().collect();
            let mut docs = Vec::with_capacity(page.len());
            for (score, addr) in page {
                docs.push(
                    state
                        .index
                        .render_doc(addr, fl.as_deref(), Some(score))
                        .map_err(|e| {
                            WfError::internal("wayfinder::DocError", e.to_string())
                                .with_params(&params)
                        })?,
                );
            }

            let mut response = Map::new();
            response.insert("numFound".to_string(), json!(num_found));
            response.insert("start".to_string(), json!(start));
            if wants_score && let Some(score) = max_score(&mlt_hits) {
                response.insert("maxScore".to_string(), json!(score));
            }
            response.insert("numFoundExact".to_string(), json!(true));
            response.insert("docs".to_string(), json!(docs));
            Value::Object(response)
        }
    };

    // `mlt.match.include=false` drops the `match` key from the envelope
    // entirely — not an empty-and-present object (finding 100,
    // `mlt_match_include_false.json` is `{responseHeader, response}`). Only
    // the literal `false` turns it off; Solr's default is `true`.
    let include_match = params.get("mlt.match.include") != Some("false");

    // Issue #143: same suppression `/select` applies, on the same param.
    let mut body = if params.omit_header() {
        json!({})
    } else {
        json!({
            "responseHeader": {
                "status": 0,
                "QTime": 0,
            },
        })
    };
    if include_match {
        body["match"] = Value::Object(match_block);
    }
    body["response"] = response_value;

    // Real Solr's `mlt.interestingTerms` value set is `none | list | details`
    // (default `none`, which omits the key entirely) — `"false"` is not a
    // value Solr recognizes at all, so the gate is an exact match on the two
    // values that turn the key on, not "anything but false".
    //
    // ponytail: `CoreIndex::mlt_query` already returns the real scored terms
    // it built the query from (`_scored_terms` above) — the remaining gap is
    // not an API limitation, it is that no captured fixture pins the
    // non-empty wire shape (finding 62: the one fixture that sets
    // `mlt.interestingTerms=details` also has zero result docs, so its
    // `interestingTerms` is `[]` regardless of what this renders). This
    // still renders an empty array, matching every fixture that exercises
    // the key today; wiring `_scored_terms` into a real per-term shape needs
    // a fixture with a non-empty result set to pin the shape against first.
    //
    // `json.nl` reaches `/mlt` through exactly this key, and (while the term
    // set is always empty) only through its *container type*: real Solr
    // renders the empty set as `[ ]` under the default `flat` and as `{ }`
    // under `map` (finding 101, `mlt_json_nl_map_empty_terms.json` against
    // `mlt_interesting_terms_details.json`). Solr's other named-list writers
    // (`arrarr`/`arrmap`/`arrntv`) are array-shaped, so everything but `map`
    // renders `[]` — the two shapes a fixture pins are the only two that can
    // differ here today.
    if matches!(
        params.get("mlt.interestingTerms"),
        Some("list") | Some("details")
    ) {
        body["interestingTerms"] = if params.get("json.nl") == Some("map") {
            json!({})
        } else {
            json!([])
        };
    }

    Ok(axum::Json(body).into_response())
}

/// `GET /solr/<core>/terms` — Solr's TermsComponent (issue #155, PRD's
/// contract-endpoint backlog). Enumerates a field's **analyzed** inverted-index
/// term dictionary with per-term document frequency.
///
/// Ground truth is `solr-ref/search-api/trace/00028.json`, a real `solr:9`
/// response to `search_api_solr`'s own request
/// (`?omitHeader=true&wt=json&json.nl=flat&terms=true&terms.fl=tm_X3b_en_title`):
///
/// ```text
/// {"terms":{"tm_X3b_en_title":["dog",2,"lazi",2,"quick",2,"about",1,...]}}
/// ```
///
/// `tm_X3b_en_title` is not a declared field — it matches
/// `presets/search-api.toml`'s `tm_X3b_en_*` `[[dynamic_fields]]` rule — and
/// the first version of this handler 400d that request as an undefined field
/// while presenting the trace as ground truth. It no longer does:
/// `check_terms_field` resolves dynamic names through the same
/// `CoreIndex::field_target` `/select` uses, and
/// `tests/terms.rs::terms_resolves_the_shipped_drupal_preset_tm_x3b_en_title_field`
/// issues that exact request against `presets/search-api.toml` loaded as-is and
/// asserts the trace's ten terms and counts. Wayfinder-side, the traced request
/// works; what is still unverified against real Solr is the analyzer chain
/// (see the ponytail note at the end).
///
/// Shape and semantics, all read off that trace:
///
/// - `terms=true` gates the component. Without it — absent, or an explicit
///   `terms=false` — there is no `terms` block at all, which is how Solr's
///   search components work: a component whose gating boolean is false
///   contributes nothing to the response. The endpoint still 200s.
///
///   Not fixture-pinned, and honestly so: the trace only ever sends
///   `terms=true`, and the ticket defers the `/terms` capture, so no captured
///   response shows what `terms=false` returns. What *is* certain is that the
///   two readings cannot both be right, and an unconditional `{"terms":{}}`
///   contradicted this very doc comment. The gated reading is the one
///   consistent with every other optional block in this codebase
///   (`facet_counts` is absent unless `facet=true` — finding 4, which *is*
///   captured), so that is what is implemented. The deferred capture settles
///   it; if it disagrees, this is the line to change.
///
///   `terms=true` with no `terms.fl` is a *different* case, and deliberately
///   not swept up by the gate: the component runs and contributes an empty
///   list, so the block is present and empty (`{"terms":{}}`).
///   `tests/terms.rs::terms_true_without_fl_produces_an_empty_terms_object`
///   pins it. `src/coverage.rs`'s `terms.terms` probe deliberately does *not*
///   use that request (issue #162): a hollow `{"terms":{}}` is nothing a
///   client can read, so the probe sends `terms.fl=body` and requires a real
///   term/frequency pair.
/// - `terms.fl` is repeatable, one key under `terms` per field, each
///   independent.
/// - The value is the flat `[term, count, term, count, ...]` array. That is
///   what `json.nl=flat` produces and the only shape this endpoint's response
///   takes, so no general named-list machinery is needed (issue #153 is
///   deliberately not a prerequisite). `json.nl=flat` and an absent `json.nl`
///   are accepted; `map`/`arrarr`/`arrmap` are 400d rather than silently
///   answered flat — see `check_terms_json_nl`.
/// - Ordering is Solr's `terms.sort=count` default: count descending, ties
///   broken by term ascending. The trace pins both halves — `dog`/`lazi`/
///   `quick` tied at 2 ahead of the singletons, and the singletons themselves
///   alphabetical.
/// - `terms.limit` defaults to 10 (`TERMS_DEFAULT_LIMIT`), applied per field
///   after the sort.
/// - `omitHeader=true` (which the module always sends here) drops
///   `responseHeader` entirely.
///
/// A `terms.fl` naming an undefined field is a 400 in Solr's envelope with no
/// `response` key: unlike `facet.field`'s post-query error
/// (`facet_unknown_field.json`, which carries the base query's `response`),
/// `/terms` has no base query to have partially run, so this follows the
/// pre-query precedent (`facet_err_range_single.json`). A `terms.fl` naming a
/// *defined but non-text* field is a 400 the same way — see
/// `check_terms_field`.
///
/// ponytail: no `solr-ref/manifest.tsv` row, so the differential harness does
/// not cover this endpoint yet. Ceiling named because it is a real gap: the
/// capture needs the differential core, and it is likely to surface analyzer
/// differences between Wayfinder's `text_en` chain and Solr's — the captured
/// `solr-ref/search-api/configset` uses `StandardTokenizerFactory`, a
/// `LengthFilterFactory min="2"`, a `WordDelimiterGraphFilterFactory`, and an
/// `accents_en.txt` mapping char filter that Wayfinder has no counterpart for.
/// None of those is a verified finding yet, so none is recorded as one; the
/// capture is what would settle them, and any diff it shows is a finding to
/// escalate, not to normalise away.
async fn terms(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or("")).allow_omit_header();
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, TERMS_PARAMS, &params)?;
    check_terms_json_nl(&params).map_err(|e| e.with_params(&params))?;

    let mut terms_block = Map::new();
    let terms_requested = params.get("terms") == Some("true");
    if terms_requested {
        for field_name in params.get_all("terms.fl") {
            check_terms_field(&state.index, field_name).map_err(|e| e.with_params(&params))?;
            let totals = state.index.field_terms(field_name).map_err(|e| {
                WfError::internal("wayfinder::TermsError", e.to_string()).with_params(&params)
            })?;
            // `field_terms` yields terms ascending, so a *stable* sort on
            // count descending leaves equal counts in term-ascending order —
            // Solr's `terms.sort=count` tie-break, asserted against the
            // trace's alphabetical run of singletons.
            let mut entries: Vec<(String, u64)> = totals.into_iter().collect();
            entries.sort_by(|(_, a), (_, b)| b.cmp(a));
            entries.truncate(TERMS_DEFAULT_LIMIT);
            let mut flat = Vec::with_capacity(entries.len() * 2);
            for (term, count) in entries {
                flat.push(json!(term));
                flat.push(json!(count));
            }
            terms_block.insert(field_name.to_string(), Value::Array(flat));
        }
    }

    let mut body = Map::new();
    if !params.omit_header() {
        body.insert(
            "responseHeader".to_string(),
            json!({
                "status": 0,
                "QTime": 0,
                "params": params.echo(),
            }),
        );
    }
    if terms_requested {
        body.insert("terms".to_string(), Value::Object(terms_block));
    }
    Ok(axum::Json(Value::Object(body)).into_response())
}

/// Refuses a `json.nl` asking for a named-list shape `/terms` does not render.
///
/// `json.nl` is in `TERMS_PARAMS` because `search_api_solr` always sends it
/// (`solr-ref/search-api/trace/00028.json` sends `json.nl=flat`), and flat —
/// `[term, count, term, count, ...]` — is the only shape this handler produces.
/// Accepting `json.nl=map` and then answering flat anyway would be exactly the
/// silent-wrong-answer `TERMS_PARAMS`' own doc comment argues against, so the
/// three values this codebase already gives a documented, *fixture-pinned*
/// meaning to for facet counts (`map`, `arrarr`, `arrmap` — see
/// `src/facet.rs`'s `JsonNl`, backed by `facet_json_nl_map.json` and friends)
/// are a 400 here rather than a 200 in the wrong shape.
///
/// Any other value is treated as flat, matching `JsonNl::from_params`' own
/// fallback for an unrecognised value. Nothing here claims that is Solr's
/// behaviour: no captured response shows Solr's reaction to a bogus `json.nl`,
/// so this follows the one precedent in the tree instead of inventing one.
/// Rendering these shapes for real (issue #153's named-list machinery) is what
/// replaces this check.
fn check_terms_json_nl(params: &Params) -> Result<(), WfError> {
    match params.get("json.nl") {
        Some(shape @ ("map" | "arrarr" | "arrmap")) => Err(WfError::bad_request(
            "wayfinder::TermsUnsupportedJsonNl",
            format!(
                "json.nl={shape} is not supported on /terms: the terms block is only \
                 rendered in the flat [term, count, ...] shape (json.nl=flat)"
            ),
        )),
        _ => Ok(()),
    }
}

/// Refuses a `terms.fl` that `/terms` cannot enumerate, the same way
/// `stats::check_statable` refuses an unaggregatable `stats.field` — an
/// undefined field, or a defined one whose term dictionary does not hold UTF-8
/// text.
///
/// **Existence is resolved, not just looked up.** `CoreIndex::
/// resolves_field_name` (the public face of `field_target`, the same
/// static-before-dynamic resolution `/select` uses) accepts a declared
/// `[[fields]]` entry *and* a name only a `[[dynamic_fields]]` pattern matches.
/// An earlier version consulted `WayfinderSchema::field` alone, which 400d
/// `terms.fl=tm_X3b_en_title` — `presets/search-api.toml`'s own `tm_X3b_en_*`
/// rule, and the exact request `solr-ref/search-api/trace/00028.json` captures
/// — as an undefined field while `q=tm_X3b_en_title:lazy` resolved fine on the
/// same core. A name matching neither is still a 400.
///
/// The type test is `resolved_value_kind` == `ValueKind::Text`, resolved with
/// the same precedence: a declared field's own kind, else the matching dynamic
/// rule's. `Text` covers both `string`/`keyword` (unanalyzed, but still raw
/// UTF-8 in the dictionary, and Solr's own TermsComponent enumerates a
/// `StrField` happily) and every `text_*`/custom chain. It excludes
/// `int`/`long`/`float`/`double`/`date`, whether declared or reached through a
/// dynamic rule (`is_*` -> `int`), whose dictionary entries are Tantivy's
/// fixed-width order-preserving encoding rather than UTF-8. Those are not
/// renderable as Solr's terms at all: decoding them lossily produced
/// replacement-character keys, and worse, silently *summed* the document
/// frequencies of two distinct encoded terms that happened to decode to the
/// same replacement string. A 400 is the honest answer until `/terms` grows
/// real per-type term rendering (Solr's `terms.raw`, deliberately out of scope
/// for issue #155 and absent from `TERMS_PARAMS`).
///
/// Note what the rule is *not*: it is not "declared fields only", and it is not
/// "the catch-all JSON containers are excluded". A text dynamic name is
/// enumerable precisely because `CoreIndex::field_terms` addresses its own
/// path-prefixed slice of the `_dynamic_text` container rather than the
/// container wholesale. What stays excluded is a non-text *value* encoding,
/// wherever it lives.
///
/// ponytail: naming a catch-all container directly (`terms.fl=_dynamic_text`)
/// is refused as non-text — `resolved_value_kind` has no `[[fields]]` entry or
/// dynamic rule for it — even though its entries are in fact UTF-8. That is
/// deliberate: enumerating it would report every dynamic field's terms mixed
/// together under one key, with the raw `<path>\0s` bytes still attached.
/// Raising this ceiling means rendering the path back out as Solr field names,
/// which is a different response shape than `/terms` has, not a relaxed check.
///
/// No fixture pins the message: the ticket defers the `/terms` capture, so this
/// follows `check_statable`'s precedent of a clear unpinned 400 rather than
/// inventing a fixture-shaped wording.
fn check_terms_field(index: &CoreIndex, field_name: &str) -> Result<(), WfError> {
    if !index.resolves_field_name(field_name) {
        return Err(WfError::bad_request(
            "wayfinder::UndefinedField",
            format!("undefined field \"{field_name}\""),
        ));
    }
    if index.wf_schema.resolved_value_kind(field_name) != Some(schema::ValueKind::Text) {
        return Err(WfError::bad_request(
            "wayfinder::TermsUnsupportedField",
            format!(
                "can not enumerate terms on the non-text field \"{field_name}\": \
                 terms.fl needs a string or text field"
            ),
        ));
    }
    Ok(())
}
