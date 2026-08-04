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
pub mod extract;
mod facet;
mod function_query;
mod grouping;
mod highlight;
mod local_params;
mod params;
mod query;
pub mod schema;
pub mod snapshot;
mod stats;

pub use config::ServerConfig;
pub use coverage::report as coverage_report;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path as AxPath, RawQuery, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use tantivy::Score;
use tantivy::query::{EmptyQuery, Occur, Query, QueryClone};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use collector::{SortClause, SortKey};
use config::AuthConfig;
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
    /// The budgets `/update/extract` runs every extraction under, resolved
    /// once from `[extraction]` in the server config.
    extract_limits: extract::ExtractLimits,
    /// The dedicated extraction thread pool. Built once here, not per
    /// request: its whole purpose is a *bounded, shared* pool, and a
    /// per-request one would give every concurrent request its own
    /// `max_concurrency` slots.
    /// `Arc` rather than owned by value so the pool can be handed out
    /// through [`AppServer::extraction`] without a second construction path:
    /// the handle a caller reserves a slot on must be the *same* runtime the
    /// route admits requests against, or the concurrency budget is two
    /// budgets.
    extraction: Arc<extract::ExtractionRuntime>,
}

/// Opaque handle to the state shared by an [`AppServer`]'s router.
///
/// Keeping this as an `Arc<AppState>` avoids a second index ownership path:
/// the binary can flush the exact core its router served after Axum has
/// drained all in-flight requests.
#[derive(Clone)]
pub struct ShutdownHandle(Arc<AppState>);

impl ShutdownHandle {
    /// Hard-commits all writes accepted before graceful shutdown completed.
    ///
    /// This is intentionally unconditional: delete-only updates do not raise
    /// `pending_docs`, but can still be waiting on a `commitWithin` deadline.
    pub fn flush(&self) -> anyhow::Result<()> {
        self.0.index.commit()
    }
}

/// Router construction plus a handle for graceful process shutdown.
///
/// [`app`] remains the normal in-process test entry point. The binary uses
/// this type so it retains the same core state that the returned router owns.
pub struct AppServer {
    router: Router,
    shutdown: ShutdownHandle,
}

impl AppServer {
    /// Returns a cloneable handle that can flush this router's core after it
    /// has stopped accepting and drained requests.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        self.shutdown.clone()
    }

    /// Returns the extraction thread pool this router's `/update/extract`
    /// route admits against.
    ///
    /// **Test support only.** This accessor exists so a single test can
    /// hold the only extraction permit and make saturation deterministic
    /// instead of racy (it has exactly one in-tree caller today). Production
    /// code should go through
    /// [`extract::ExtractionRuntime::spawn_extraction`] rather than holding
    /// a permit directly.
    ///
    /// Additive: it hands back the *same* [`extract::ExtractionRuntime`] the
    /// route uses, so a slot reserved through
    /// [`extract::ExtractionRuntime::try_acquire_permit`] on it is a slot
    /// the route can no longer hand out.
    ///
    /// The returned [`Arc`] carries two hazards the signature does not show:
    ///
    /// 1. It can outlive the `AppState` it came from. Each clone keeps the
    ///    runtime alive, deferring [`extract::ExtractionRuntime`]'s `Drop`
    ///    and so keeping its `max_concurrency` dedicated OS threads alive for
    ///    as long as any clone is held.
    /// 2. An [`extract::ExtractionPermit`] obtained through it and then
    ///    leaked (e.g. with `mem::forget`) permanently burns a concurrency
    ///    slot — the same burnt-slot failure `ExtractionRuntime` documents
    ///    for a wedged parser, but reachable by accident from outside the
    ///    module.
    ///
    /// Both are harmless in-tree; the only caller today is a test that drops
    /// the permit on scope exit.
    pub fn extraction(&self) -> Arc<extract::ExtractionRuntime> {
        Arc::clone(&self.shutdown.0.extraction)
    }

    /// Consumes this construction result into the HTTP router.
    pub fn into_router(self) -> Router {
        self.router
    }
}

#[derive(Clone)]
struct Authentication {
    auth: Option<AuthConfig>,
    core_name: String,
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
    // Accepted-and-warned (issue #232): edismax's boost-function param.
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
    // `function=max(_version_)` is a real Solr stats-component form (the
    // `stats_version_max` fixture captures Solr accepting and echoing it), so
    // it is admitted for strict_params parity. The captured client does not
    // send it: finding 132 (#293) shows search_api_solr reads `_version_`
    // through a `json.facet` aggregation, not the stats component. `function`
    // stays admitted as a valid capability of the statable `_version_` field.
    "function",
    // Result grouping (issue #290, finding 130): `setGrouping()` sends
    // exactly these six `group.*` params plus `group` (from Solarium's
    // component). `group.truncate` and `group.facet` are accepted for
    // strict_params parity even though their facet-interaction semantics
    // (computing facets over collapsed groups) are not yet fixture-backed —
    // their defaults (false) leave Wayfinder's existing facet behaviour
    // correct, so accepting them changes nothing until a request sets them
    // true. `group.format` and `group.main` are deliberately absent: they are
    // never sent (finding 130) and must 400 under strict_params rather than be
    // silently accepted as an unimplemented param.
    "group",
    "group.field",
    "group.ngroups",
    "group.limit",
    "group.offset",
    "group.sort",
    "group.truncate",
    "group.facet",
    "sort",
    "hl",
    "hl.fl",
    "hl.snippets",
    "hl.fragsize",
    "hl.simple.pre",
    "hl.simple.post",
    "hl.method",
    // Issues #139/#181: the captured client sends both as false, Solr's
    // defaults. Both false and true paths are implemented and fixture-backed:
    // requireFieldMatch controls cross-field term extraction, while
    // mergeContiguous controls original-highlighter fragment coalescing
    // (findings 113-114).
    "hl.mergeContiguous",
    "hl.requireFieldMatch",
    // Issue #222: Search API sends these spellcheck component params.
    // `spellcheck.dictionary` is intentionally repeatable, as in trace 00021;
    // the suggestion path uses its first value, matching Solr's capture.
    "spellcheck",
    "spellcheck.q",
    "spellcheck.dictionary",
    "spellcheck.collate",
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
/// ponytail: exactly four entries, and that is the ceiling, not an oversight.
/// `.limit`/`.mincount`/`.sort` joined `.missing` with issue #296, which
/// implemented them in `FacetSettings::resolve` (`src/facet.rs`) in this same
/// change. Every other `f.<field>.facet.*` Solr accepts (`.prefix`,
/// `.method`, `.range.*`) is unimplemented here and must keep 400ing under
/// `strict_params` — pinned by
/// `strict_params_still_rejects_an_unrelated_f_dot_param`
/// (`tests/facet_field_missing_override.rs`). Allowlisting a per-field param
/// whose value is then ignored converts a loud 400 into a silently wrong
/// answer: a client asking for `f.category.facet.prefix=x` would get the whole
/// bucket list and no indication the filter was dropped. Upgrade path:
/// implement the override where the global is read in `src/facet.rs`
/// (`FacetSettings::resolve` is the worked example — the addressed forms win
/// over the global, findings 147/151), *then* add the base param name here in
/// the same change. Adding a name here alone is the bug.
const PER_FIELD_PARAMS: &[&str] = &[
    "facet.missing",
    "facet.limit",
    "facet.mincount",
    "facet.sort",
];
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
/// `<core>/update/extract` (issues #258 and #259). Two modes share this
/// one allowlist:
///
/// - **extractOnly=true** (#258): the document is extracted and returned,
///   never indexed. Only the extract-shape params apply.
/// - **extractOnly absent/false** (#259, Solr Cell indexing semantics): the
///   extracted content is indexed through the same commit path `/update`
///   uses, so the commit family (`commit`/`commitWithin`/`softCommit`/
///   `overwrite`) is admitted too, alongside Solr's ExtractingRequestHandler
///   literal/mapping family.
///
/// The literal/mapping family is admitted as **prefix families** (the
/// trailing-dot entries below): any `literal.<field>` sets a document field
/// value, any `fmap.<from>=<to>` renames an extracted/captured field. The
/// remaining shape params (`uprefix`, `lowernames`, `captureAttr`) are exact
/// keys. `capture`/`xpath`/`defaultField`/`boost.*`/`ignoreTikaException`
/// stay absent and 400 under `strict_params`: accepting a param that silently
/// does nothing would be a worse divergence than rejecting it.
///
/// `extractOnly` is no longer required (issue #259 makes the indexing path
/// reachable); `extractFormat`/`resource.name` are only meaningful to the
/// extractOnly response and are accepted-and-ignored on the indexing path.
const EXTRACT_PARAMS: &[&str] = &[
    "extractOnly",
    "extractFormat",
    "resource.name",
    "wt",
    "omitHeader",
    "json.nl",
    // Indexing path (#259): commit semantics shared with `/update`.
    "commit",
    "commitWithin",
    "softCommit",
    "overwrite",
    // Indexing path (#259): Solr Cell field-mapping shape params.
    "uprefix",
    "lowernames",
    "captureAttr",
    // Prefix families — any `literal.<field>` / `fmap.<from>` is accepted
    // (the trailing dot is what `check_params`'s prefix match keys on).
    "literal.",
    "fmap.",
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

/// Per-route body-limit policy. The fourth column of `search_api_routes!`.
///
/// `inherit_body_limit` is the normal case: the route says nothing, so the
/// global `DefaultBodyLimit::max(resources.max_body_size)` layer applies.
///
/// `extraction_body_limit` overrides that layer for one route.
/// `/update/extract` needs a *different* finite ceiling, not no ceiling: its
/// content budget is `extraction.max_body_bytes`, counted byte by byte by the
/// handler for every part it consumes, so an oversized upload gets the
/// captured 413 envelope rather than axum's bare `LengthLimitError`. But the
/// handler cannot count what it never sees — `multer` consumes multipart part
/// *headers* before a `Field` exists — so the route still needs a transport
/// backstop. `ExtractLimits::route_body_ceiling` is that backstop:
/// `max_body_bytes` plus framing head-room, so the counted content limit is
/// what a realistic oversized upload hits first, and the transport cap only
/// catches bodies that are pathological in a way counting cannot reach.
///
/// A route-level `DefaultBodyLimit::max(...)` layer is applied *inside* the
/// global one and replaces it for that route, which is exactly the intent
/// here.
type RouteMethods = axum::routing::MethodRouter<Arc<AppState>>;

fn inherit_body_limit(route: RouteMethods, _extract_ceiling: usize) -> RouteMethods {
    route
}

fn extraction_body_limit(route: RouteMethods, extract_ceiling: usize) -> RouteMethods {
    route.layer(DefaultBodyLimit::max(extract_ceiling))
}

macro_rules! search_api_routes {
    ($apply:ident $(, $extra:expr)?) => {
        $apply! {
            $([$extra])?
            ("/wayfinder/{core}/update", update, update_method, inherit_body_limit),
            ("/wayfinder/{core}/update/extract", update_extract, update_method, extraction_body_limit),
            ("/wayfinder/{core}/select", select, any_method, inherit_body_limit),
            ("/wayfinder/{core}/mlt", mlt, any_method, inherit_body_limit),
            ("/wayfinder/{core}/terms", terms, any_method, inherit_body_limit),
            ("/wayfinder/{core}/admin/ping", ping, any_method, inherit_body_limit),
            ("/wayfinder/admin/info/system", admin_info_system, any_method, inherit_body_limit),
            ("/wayfinder/{core}/admin/system", core_admin_system, any_method, inherit_body_limit),
            ("/wayfinder/{core}/schema/fieldtypes", schema_fieldtypes, any_method, inherit_body_limit),
            ("/wayfinder/{core}/admin/luke", admin_luke, any_method, inherit_body_limit),
            ("/wayfinder/{core}/admin/mbeans", admin_mbeans, any_method, inherit_body_limit),
        }
    };
}

macro_rules! route_specs {
    ($(($path:literal, $handler:ident, $accepts_method:ident, $body_limit:ident)),+ $(,)?) => {
        &[$(RouteSpec { path: $path, accepts_method: $accepts_method }),+]
    };
}

macro_rules! wire_routes {
    ([$ceiling:expr] $(($path:literal, $handler:ident, $accepts_method:ident, $body_limit:ident)),+ $(,)?) => {
        Router::new()$(.route($path, $body_limit(any($handler), $ceiling)))+
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
    "mlt.maxntp",
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
];

/// `/terms` params (Solr's TermsComponent). `terms` gates the component,
/// `terms.fl` (repeatable) names the fields; `terms.prefix`/`terms.limit` are
/// the autocomplete params `search_api_solr`'s `setAutocompleteTermQuery()`
/// sends (issue #308, findings 141/142); `omitHeader`/`wt`/`json.nl` are the
/// envelope params it always sends here (`solr-ref/search-api/trace/00028.json`).
///
/// ponytail: deliberately absent, so `strict_params = true` still 400s them —
/// `terms.sort`, `terms.lower`/`upper`, `terms.mincount`/`maxcount`,
/// `terms.regex`, `terms.raw`, `terms.ttf`. `terms.sort` is honoured only as
/// its default (`count`); the rest are unimplemented. Add them when a capture
/// needs them — listing a param here that the handler ignores would be worse
/// than 400ing it, since it would silently answer the wrong question.
const TERMS_PARAMS: &[&str] = &[
    "terms",
    "terms.fl",
    "terms.prefix",
    "terms.limit",
    "omitHeader",
    "wt",
    "json.nl",
];

/// Solr's `terms.limit` default (finding 142). Applied per field when
/// `terms.limit` is absent; a negative value means unlimited, so the default
/// is the `Some(TERMS_DEFAULT_LIMIT)` case of [`parse_terms_limit`].
const TERMS_DEFAULT_LIMIT: usize = 10;

/// Builds the Wayfinder HTTP app for a single core with all server-config
/// defaults (PRD §6). Use `app_with_config` to supply a config file.
pub fn app(schema_path: &Path, data_dir: &Path) -> anyhow::Result<Router> {
    app_server(schema_path, data_dir).map(AppServer::into_router)
}

/// As `app`, with the server config read from `config_path`. A missing file
/// means all defaults; unknown keys in a present file are an error.
pub fn app_with_config(
    schema_path: &Path,
    data_dir: &Path,
    config_path: &Path,
) -> anyhow::Result<Router> {
    app_server_with_config(schema_path, data_dir, config_path).map(AppServer::into_router)
}

/// Builds an app together with the opaque handle needed to flush its core at
/// process shutdown. This is the binary-facing counterpart to [`app`].
pub fn app_server(schema_path: &Path, data_dir: &Path) -> anyhow::Result<AppServer> {
    build(schema_path, data_dir, ServerConfig::default())
}

/// As [`app_server`], with server config loaded from `config_path`.
pub fn app_server_with_config(
    schema_path: &Path,
    data_dir: &Path,
    config_path: &Path,
) -> anyhow::Result<AppServer> {
    build(schema_path, data_dir, ServerConfig::load(config_path)?)
}

fn build(schema_path: &Path, data_dir: &Path, config: ServerConfig) -> anyhow::Result<AppServer> {
    let index = CoreIndex::open(schema_path, data_dir, &config)?;
    let core_name = index.wf_schema.core.name.clone();
    // Issue #64: raise (and make configurable via `resources.max_body_size`)
    // the request-body cap that axum's `Bytes`/`Json` extractors otherwise
    // enforce at a bare, hardcoded 2MB via `DefaultBodyLimit`.
    let max_body_size = config.resources.max_body_size;
    let authentication = Authentication {
        auth: config.auth.clone(),
        core_name: core_name.clone(),
    };
    let extract_limits = config.extraction.limits();
    let extract_body_ceiling = extract_limits.route_body_ceiling();
    let extraction = Arc::new(extract::ExtractionRuntime::new(&extract_limits));
    let state = Arc::new(AppState {
        core_name,
        index,
        config,
        started_at: Instant::now(),
        extract_limits,
        extraction,
    });

    // `any`, not `get`/`post`: Solr's request handlers are method-agnostic —
    // `err_select_delete.json` shows DELETE /select served as a normal query,
    // so a 405 from the router would be a divergence. `/update` does reject
    // some methods (`err_update_put.json`), which it does itself, with Solr's
    // envelope for it.
    let router = search_api_routes!(wire_routes, extract_body_ceiling)
        // Admin UI (issue #94, PRD §5 v2.5). Outside `/wayfinder/*` on purpose:
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
    let router = router.route("/wayfinder/{core}/__test_panic__", any(test_panic));

    // Defence in depth (#39): a handler panic (e.g. an unforeseen
    // `.unwrap()`/`.expect()` deep in a dependency, reachable from
    // attacker-controlled input) must surface as a normal HTTP 500 in
    // Solr's error envelope rather than unwinding the connection. This is a
    // last-resort net, not a substitute for fixing the panic at its source.
    let shutdown = ShutdownHandle(Arc::clone(&state));
    let router = router
        .with_state(state)
        .layer(middleware::from_fn_with_state(authentication, authenticate))
        .layer(CatchPanicLayer::custom(handle_panic))
        .layer(DefaultBodyLimit::max(max_body_size))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        uri = %request.uri(),
                    )
                })
                .on_request(|_request: &Request<_>, span: &tracing::Span| {
                    let _entered = span.enter();
                    tracing::info!("request started");
                })
                .on_response(
                    |response: &axum::http::Response<_>, latency, span: &tracing::Span| {
                        let _entered = span.enter();
                        tracing::info!(
                            status = response.status().as_u16(),
                            latency = ?latency,
                            "request completed"
                        );
                    },
                ),
        );
    Ok(AppServer { router, shutdown })
}

/// Enforces optional HTTP Basic authentication before any application route.
/// The two health checks remain public so orchestration can distinguish an
/// unavailable process from unavailable credentials.
async fn authenticate(
    State(authentication): State<Authentication>,
    request: Request,
    next: Next,
) -> Response {
    if authentication.auth.is_none()
        || public_auth_path(request.uri().path(), &authentication.core_name)
    {
        return next.run(request).await;
    }

    let valid = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(basic_credentials)
        .is_some_and(|credentials| {
            authentication
                .auth
                .as_ref()
                .is_some_and(|auth| auth.matches(&credentials))
        });

    if valid {
        next.run(request).await
    } else {
        let mut response = WfError::new(
            StatusCode::UNAUTHORIZED,
            "wayfinder::AuthenticationError",
            "authentication required",
        )
        .envelope(Envelope::NoParams)
        .into_response();
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"wayfinder\""),
        );
        response
    }
}

fn public_auth_path(path: &str, core_name: &str) -> bool {
    path == "/ui/ping" || path == format!("/wayfinder/{core_name}/admin/ping")
}

fn basic_credentials(header: &str) -> Option<Vec<u8>> {
    let mut parts = header.split_ascii_whitespace();
    let scheme = parts.next()?;
    let payload = parts.next()?;
    if !scheme.eq_ignore_ascii_case("basic") || parts.next().is_some() {
        return None;
    }
    STANDARD.decode(payload).ok()
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
/// query string it calls [`select`] — the very function `/wayfinder/{core}/select`
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
        // The form checkbox only, not a validation point: this handler always
        // renders HTML, and an invalid `facet` value 400s inside the `select`
        // call below, whose real envelope is what the page shows.
        facet: params
            .get("facet")
            .and_then(params::parse_bool)
            .unwrap_or(false),
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
/// very function `/wayfinder/{core}/admin/ping` routes to, with this process's
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
    match params.get("sort") {
        None => Ok(Vec::new()),
        Some(spec) => parse_sort_spec(&state.index.wf_schema, params, spec),
    }
}

/// Parses a Solr sort spec string into clauses. Shared by `check_sort` (the
/// `sort` param) and grouping's `group.sort` (issue #290) so both speak the
/// same field-direction grammar — comma does not delimit the field token,
/// direction is checked before the field resolves, and a dynamic-only match
/// sorts on its catch-all fast column (findings 18/34/35, issue #66).
pub(crate) fn parse_sort_spec(
    schema: &crate::schema::WayfinderSchema,
    params: &Params,
    spec: &str,
) -> Result<Vec<SortClause>, WfError> {
    // Rewritten clause grammar (finding 34/35, issue #32). Scanned with an
    // absolute cursor into `spec` rather than `split(',')`: a comma does NOT
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
        let ws = spec[pos..]
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(spec.len() - pos);
        pos += ws;
        if pos >= spec.len() {
            break;
        }

        // FIELD: the next whitespace-delimited token, starting at `pos`. A
        // comma does not delimit it.
        let field_len = spec[pos..]
            .find(char::is_whitespace)
            .unwrap_or(spec.len() - pos);
        let field_end = pos + field_len;
        let field_name = &spec[pos..field_end];

        // DIRECTION: from just past the field token to the next comma or end
        // of spec, trimmed, checked as one chunk against `asc`/`desc`.
        let dir_start = field_end;
        let comma_rel = spec[dir_start..].find(',');
        let dir_end = dir_start + comma_rel.unwrap_or(spec.len() - dir_start);
        let direction_raw = &spec[dir_start..dir_end];

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
                        "Can't determine a Sort Order (asc or desc) in sort spec '{spec}', pos={dir_start}"
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
            match schema.resolved_fast(field_name) {
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
                    let column = schema
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
            SortKey::Field(_) => schema.resolved_value_kind(field_name),
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
            // Solr's own wording, from the Jetty page issue #179 captured for
            // `omitHeader=1` (`omit_header_invalid_one.html`: `msg=invalid
            // boolean value: 1`) — the same message every other invalid
            // boolean gets, since it is the same `StrUtils.parseBool` failure
            // (issue #187, finding 115).
            WfError::bad_request("wayfinder::InvalidParam", params::invalid_bool_msg(value))
                .with_params(params)
                .suppress_response_header()
        })?;
    }
    let accepted = |key: &str| {
        allowed.contains(&key)
            // A trailing-dot entry in `allowed` is a *prefix family*: any
            // key starting with it (with at least one character after the
            // dot) is accepted. Route-scoped by construction — only a route
            // whose allowlist carries such an entry gets prefix matching —
            // so `literal.id`/`fmap.content` are accepted on `/update/extract`
            // (issue #259) but `literal.id` still 400s on `/select`.
            || allowed.iter().any(|prefix| {
                prefix.ends_with('.') && key.starts_with(prefix) && key.len() > prefix.len()
            })
            || params::split_per_field_key(key, PER_FIELD_PARAMS)
                .is_some_and(|(_, base)| allowed.contains(&base))
    };
    if !state.config.strict_params {
        for unknown in params.keys().filter(|key| !accepted(key)) {
            tracing::debug!(parameter = %unknown, "ignoring unknown request parameter");
        }
        return Ok(());
    }
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

/// `/wayfinder/admin/info/system` — server-level version handshake (issue #59).
/// Not core-scoped: no `{core}` path segment, hence no `check_core` call.
///
/// `lucene.wayfinder-spec-version` is the ONE field `search_api_solr`'s
/// `SolrConnector::getSolrVersion()` (finding 78) actually reads, and it is
/// read here from `config.admin.reported_server_version` — see
/// `config::Admin` for the version-choice reasoning (PRD open question 2).
async fn admin_info_system(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or(""));
    check_params(&state, ADMIN_INFO_PARAMS, &params)?;
    let (jvm, system, security) = admin_info_jvm_system_security();
    let version = &state.config.admin.reported_server_version;
    let mut lucene = Map::new();
    lucene.insert("wayfinder-spec-version".to_string(), json!(version));
    lucene.insert(
        "wayfinder-impl-version".to_string(),
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
        "wayfinder_home": "/var/wayfinder/data",
        "core_root": "/var/wayfinder/data",
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
const CORE_ADMIN_SCHEMA: &str = "drupal-4.4.0-wayfinder-9.x-0";

/// `/wayfinder/{core}/admin/system` — core-scoped fallback for the same
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
    let version = &state.config.admin.reported_server_version;
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
            "wayfinder-spec-version": version,
            "wayfinder-impl-version": format!("{version} wayfinder"),
            "lucene-spec-version": "9.12.3",
            "lucene-impl-version": "9.12.3 wayfinder",
        },
        "jvm": jvm,
        "security": security,
        "system": system,
    });
    Ok(axum::Json(body).into_response())
}

/// `/wayfinder/{core}/admin/mbeans` -- the JMX-bean dump `search_api_solr`'s
/// "Solr server status" report reads (issue #158, reversing #57's descope for
/// this endpoint).
///
/// Ground truth is `solr-ref/search-api/trace/00025.json`: 48 KB of real
/// `solr:9` output, of which `SolrConnectorPluginBase::getStatsSummary()`
/// (`coverage/search_api_solr_4.4.0_source/src/SolrConnector/SolrConnectorPluginBase.php`,
/// ~L775-820) reads exactly six leaves on its Solr >= 7.0 branch -- the branch
/// that applies, since `config.admin.reported_server_version` reports 9.x:
///
/// - `wayfinder-mbeans.UPDATE.updateHandler.stats["UPDATE.updateHandler.docsPending"]`
/// - `...["UPDATE.updateHandler.softAutoCommitMaxTime"]`
/// - `...["UPDATE.updateHandler.deletesById"]`
/// - `...["UPDATE.updateHandler.deletesByQuery"]`
/// - `wayfinder-mbeans.CORE.core.stats["CORE.coreName"]`
/// - `wayfinder-mbeans.CORE.core.stats["INDEX.size"]`
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

    // The glued-param trace still matters here, it is just no longer a
    // deviation (issue #187). The captured request path is verbatim
    // `admin/mbeans?stats=true?omitHeader=false&json.nl=map&json.nl=flat&wt=json`
    // -- `search_api_solr` concatenates a handler string that already carries a
    // query onto Solarium's own params -- so `stats` arrives with the raw value
    // `true?omitHeader=false`, and the captured RESPONSE shows Solr honoured
    // it anyway (`UPDATE.updateHandler.stats` is present with real values).
    // Solr's own `StrUtils.parseBool` is a prefix test, so the shared parser
    // reads that glued value as `true` for the same reason real Solr did, and
    // this site is now conformant rather than a special case.
    // ponytail: Wayfinder does not recover the `omitHeader=false` that got
    // glued on, and does not honour it. Ceiling -- real Solr splits the glued
    // query back out into separate params; here it stays part of `stats`'s
    // value, which changes nothing for `stats` itself but silently drops the
    // `omitHeader` the client meant to send.
    let want_stats = params.bool_or("stats", false)?;

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
        json!("dev.wayfinder.update.DirectUpdateHandler2"),
    );
    update_handler.insert(
        "description".to_string(),
        json!("Update handler that efficiently directly updates the on-disk main lucene index"),
    );
    let mut core_bean = Map::new();
    core_bean.insert("class".to_string(), json!(state.core_name));
    core_bean.insert("description".to_string(), json!("WayfinderCore"));
    // The `stats` sub-object appears only under `stats=true` -- without it Solr
    // returns the bean list alone, and the coverage probe for
    // `admin.mbeans.wayfinder-mbeans` GETs the endpoint with no `stats` at all.
    if want_stats {
        update_handler.insert("stats".to_string(), Value::Object(update_stats));
        core_bean.insert("stats".to_string(), core_stats);
    }

    let body = json!({
        "responseHeader": {
            "status": 0,
            "QTime": 0,
        },
        "wayfinder-mbeans": {
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
fn field_class_for_builtin(name: &str) -> &'static str {
    match name {
        "string" | "keyword" => "wayfinder.StrField",
        "int" => "wayfinder.IntPointField",
        "long" => "wayfinder.LongPointField",
        "float" => "wayfinder.FloatPointField",
        "double" => "wayfinder.DoublePointField",
        "date" => "wayfinder.DatePointField",
        // `text_general`, `text_en` and every `text_<code>` preset: analyzed
        // text, which is Solr's `TextField`.
        _ => "wayfinder.TextField",
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

/// `/wayfinder/{core}/schema/fieldtypes` — the field types this core can actually
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
        .map(|ft| field_type_entry(&ft.name, "wayfinder.TextField"))
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
            .map(|name| field_type_entry(name, field_class_for_builtin(name))),
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

/// `/wayfinder/{core}/admin/luke` — index statistics and the field list (issue
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
    check_params(&state, UPDATE_PARAMS, &params).map_err(|e| e.envelope(Envelope::NoParams))?;

    // Every boolean this handler reads, validated at entry so an invalid value
    // 400s here rather than being silently read as `false` later.
    // `omitHeader` is NOT among them: `check_params` above already validated
    // it, for every allowlist containing the name (issue #214).
    let bool_param = |key: &str, default: bool| {
        params
            .bool_or(key, default)
            .map_err(|e| e.envelope(Envelope::NoParams))
    };
    // Bound separately and *then* OR-ed: writing this as
    // `bool_param("commit", false)? || bool_param("softCommit", false)?`
    // short-circuits, so `commit=true&softCommit=nope` would never parse
    // `softCommit` at all and would 200 on an invalid boolean -- the exact
    // silent acceptance issue #187 exists to remove. Every boolean this
    // handler accepts is validated, whatever the others say.
    let commit = bool_param("commit", false)?;
    let soft_commit = bool_param("softCommit", false)?;
    let commit_requested = commit || soft_commit;
    // `overwrite=false` skips the default replace-by-uniqueKey step
    // (finding 48b); Solr's default is `overwrite=true`.
    let overwrite = bool_param("overwrite", true)?;

    // GET carries no body (finding 47): it is not a method error, but a
    // *content-stream* one — 400 "missing content stream" unless the only
    // thing being asked is a commit, which really commits and answers 200.
    if method == Method::GET {
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
    if commit_requested {
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

/// Adapts one axum multipart field to `extract::ChunkSource`, so the upload
/// is counted and spilled to a temp file by the same `stream_to_tempfile`
/// the phase-0 budget tests exercise — rather than a second byte-counting
/// loop written here, which is exactly how the two would drift apart.
struct FieldChunks<'a> {
    field: axum::extract::multipart::Field<'a>,
    /// Set when the underlying multipart error is the route's
    /// `DefaultBodyLimit` firing. `ChunkSource` yields `io::Error`, which
    /// would otherwise flatten a transport-level 413 into a 500 by the time
    /// `ExtractError::Io` has stringified it — this flag carries the one bit
    /// of that error that changes the response.
    body_limit_hit: Arc<AtomicBool>,
}

impl<'a> FieldChunks<'a> {
    fn new(field: axum::extract::multipart::Field<'a>, body_limit_hit: Arc<AtomicBool>) -> Self {
        FieldChunks {
            field,
            body_limit_hit,
        }
    }
}

impl extract::ChunkSource for FieldChunks<'_> {
    async fn next_chunk(&mut self) -> Option<std::io::Result<axum::body::Bytes>> {
        match self.field.chunk().await {
            Ok(Some(chunk)) => Some(Ok(chunk)),
            Ok(None) => None,
            Err(e) => {
                if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    self.body_limit_hit.store(true, Ordering::Relaxed);
                }
                Some(Err(std::io::Error::other(e)))
            }
        }
    }
}

/// The default `Content-Type` for a part that declares none — Tika's own
/// fallback, and what every captured no-`Content-Type` extract echoes back
/// as `stream_content_type` (`extract_plain_text_xml.json`).
const OCTET_STREAM: &str = "application/octet-stream";

/// The Search-API configset's `ExtractingRequestHandler` defaults (issue
/// #259), hardcoded because they are the only evidenced config — the
/// captured index/select pair was taken against exactly these. "Wire format
/// only, never Solr's config format" (CLAUDE.md) means matching that
/// configset's wire behaviour, not exposing its `solrconfig.xml`. Request
/// params override and extend: a request `fmap.<from>` wins over the default
/// on the same `<from>` and adds new mappings; `lowernames`/`uprefix`/
/// `captureAttr` are overridden outright when sent.
const EXTRACT_DEFAULT_FMAP: &[(&str, &str)] = &[("a", "links"), ("div", "ignored_")];
const EXTRACT_DEFAULT_UPREFIX: &str = "ignored_";

/// Builds the indexed document's field map from an extraction and the
/// request's Solr-Cell params (#259), in Solr's order: `lowernames` → `fmap`
/// rename → `uprefix`-drop → `literal.*` overlay, then keep only fields that
/// resolve against the schema (declared or a dynamic rule).
///
/// Returns `field_name -> Vec<Value>` in insertion order; the caller wraps it
/// as a JSON object and indexes it through the normal `/update` path. A field
/// that does not resolve against the schema is dropped when `uprefix` is set
/// — reproducing the observable effect of the Search-API configset's catch-all
/// `<dynamicField name="*" type="ignored">` (stored/indexed false), which is
/// what makes `uprefix=ignored_` drop unmapped fields from selects. Without
/// `uprefix`, the field passes through so `add_documents` errors on a
/// genuinely unknown field exactly as strict Solr
/// (`-Dupdate.autoCreateFields=false`) does.
///
/// ponytail: Wayfinder drops uprefix'd fields outright rather than indexing
/// them into a catch-all ignored-type field. The observable result is
/// identical (the field never appears in a select); reproducing the
/// ignored-type field would need a schema change for no wire benefit. Trigger:
/// a captured index whose select returns a value Solr stored under the
/// catch-all but Wayfinder dropped.
///
/// The indexed `body`/`links` values come from Wayfinder's own extractors and
/// so diverge from the captured select fixture (`extract_html_select.json`):
/// Wayfinder does not replicate Tika's content-field whitespace, and PRD
/// divergence 10 forbids fabricating `shape="rect"`, so `links` carries only
/// the real attribute values. That divergence is recorded in the PRD and
/// asserted by the route tests; this function is where it originates.
fn extract_cell_fields(
    doc: &extract::ExtractedDocument,
    params: &Params,
    schema: &schema::WayfinderSchema,
) -> Result<Vec<(String, Vec<Value>)>, WfError> {
    let lowernames = params.bool_or("lowernames", true)?;
    let capture_attr = params.bool_or("captureAttr", true)?;
    let uprefix = params.get("uprefix").unwrap_or(EXTRACT_DEFAULT_UPREFIX);
    let uprefix_set = !uprefix.is_empty();

    // `fmap`: defaults merged with request params (request wins on conflict).
    let mut fmap: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for (from, to) in EXTRACT_DEFAULT_FMAP {
        fmap.insert(from, to);
    }
    for (from, to) in params.pairs_with_prefix("fmap.") {
        fmap.insert(from, to);
    }
    let rename = |name: &str| -> String {
        fmap.get(name)
            .map(|s| (*s).to_string())
            .unwrap_or_else(|| name.to_string())
    };
    let resolves = |name: &str| schema.is_static(name) || schema.match_dynamic(name).is_some();

    // Source fields: extracted text/metadata, plus captured element
    // attributes when `captureAttr` is on.
    let mut source: Vec<(String, Vec<String>)> = doc.extract_source_fields();
    if capture_attr {
        for (element, value) in &doc.captured_attrs {
            if let Some(entry) = source.iter_mut().find(|(name, _)| name == element) {
                entry.1.push(value.clone());
            } else {
                source.push((element.clone(), vec![value.clone()]));
            }
        }
    }

    let mut fields: Vec<(String, Vec<Value>)> = Vec::new();
    let push = |raw_name: String, values: Vec<String>, fields: &mut Vec<(String, Vec<Value>)>| {
        let name = if lowernames {
            raw_name.to_ascii_lowercase()
        } else {
            raw_name
        };
        let name = rename(&name);
        if !resolves(&name) {
            // With `uprefix` set, unknown fields are dropped — the observable
            // effect of the Search-API configset's catch-all ignored-type
            // dynamic field, which `uprefix=ignored_` relies on. Without it,
            // the field passes through to `add_documents`, which errors on a
            // genuinely unknown field as strict Solr does.
            if uprefix_set {
                return;
            }
        }
        let vals: Vec<Value> = values.into_iter().map(Value::String).collect();
        if let Some(entry) = fields.iter_mut().find(|(n, _)| *n == name) {
            entry.1.extend(vals);
        } else {
            fields.push((name, vals));
        }
    };
    for (name, values) in source {
        push(name, values, &mut fields);
    }
    // `literal.*` overlay: explicit field values, added after extraction
    // (Solr merges them into the document; a literal naming a field the
    // extractor also produced multivalues). `lowernames` applies; `fmap` does
    // not — `fmap` is for extracted/captured fields, and a literal is already
    // the caller's chosen destination name.
    for (field, value) in params.pairs_with_prefix("literal.") {
        push(field.to_string(), vec![value.to_string()], &mut fields);
    }
    Ok(fields)
}

/// The indexing half of `/update/extract` (issue #259): takes the extraction
/// and the request's Solr-Cell params, builds the document through
/// [`extract_cell_fields`], and indexes it through the same commit path `/update`
/// uses — `add_documents` then the `commit`/`softCommit`/`commitWithin`
/// semantics, answering the bare `responseHeader` envelope
/// (`extract_html_index.json`).
///
/// Every boolean is validated at entry (issue #187), so `commit=maybe`
/// 400s here rather than being silently read as `false` — exactly as `/update`
/// does. The bound-then-OR pattern is copied from there for the same reason:
/// short-circuiting would let an invalid `softCommit` hide behind a true
/// `commit`.
async fn extract_cell_index(
    state: &AppState,
    params: &Params,
    doc: &extract::ExtractedDocument,
) -> Result<Response, WfError> {
    let no_params = |class: &'static str, msg: String| {
        WfError::bad_request(class, msg)
            .with_params(params)
            .envelope(Envelope::NoParams)
    };
    let commit = params
        .bool_or("commit", false)
        .map_err(|e| e.envelope(Envelope::NoParams))?;
    let soft_commit = params
        .bool_or("softCommit", false)
        .map_err(|e| e.envelope(Envelope::NoParams))?;
    let overwrite = params
        .bool_or("overwrite", true)
        .map_err(|e| e.envelope(Envelope::NoParams))?;
    let commit_requested = commit || soft_commit;

    let fields = extract_cell_fields(doc, params, &state.index.wf_schema)
        .map_err(|e| e.envelope(Envelope::NoParams))?;
    let mut obj = Map::new();
    for (name, values) in fields {
        // Single value -> scalar, multi -> array, matching the JSON shape
        // `/update` bodies use and `add_documents` expects.
        let value = if values.len() == 1 {
            values.into_iter().next().expect("len == 1")
        } else {
            Value::Array(values)
        };
        obj.insert(name, value);
    }
    state
        .index
        .add_documents(&[Value::Object(obj)], overwrite)
        .map_err(|e| no_params("wayfinder::IndexError", e.to_string()))?;
    if commit_requested {
        state.index.commit().map_err(|e| {
            WfError::internal("wayfinder::CommitError", e.to_string())
                .with_params(params)
                .envelope(Envelope::NoParams)
        })?;
    }
    if let Some(ms) = params
        .get("commitWithin")
        .and_then(|s| s.parse::<u64>().ok())
    {
        state.index.schedule_commit(ms);
    }
    Ok(update_success(params))
}

/// `POST /wayfinder/{core}/update/extract` (issues #258 and #259).
///
/// Two modes share this one handler, selected by the resolved `extractOnly`
/// boolean:
///
/// - **`extractOnly=true`** (#258): extract the document and return Tika's
///   `{responseHeader, file, file_metadata}` envelope, never indexing it.
/// - **`extractOnly` absent/false** (#259, Solr Cell indexing): apply the
///   extracted content to the index through the same commit path `/update`
///   uses, answering the bare `responseHeader` envelope
///   (`extract_html_index.json`). See [`extract_cell_index`].
///
/// #258 shipped requiring `extractOnly=true` (PRD divergence 10): indexing was
/// out of v1 scope, so a 200 that silently indexed nothing was the worse
/// failure. #259 retires that half of the divergence — the indexing path now
/// exists — while keeping the other halves (no `X-Parsed-By`, no fabricated
/// `shape="rect"`, 415 for unsupported formats).
///
/// Shape of the work, in the order the budgets need it:
///
/// 1. Params and core, exactly as `/update` validates them (`Envelope::NoParams` —
///    this is an `/update` path and Solr never echoes params on one).
/// 2. Multipart intake: the **first part with a non-empty filename** is the
///    document. Streamed to a temp file through `stream_to_tempfile`, which
///    fails with `BodyTooLarge` at the first chunk that crosses
///    `extraction.max_body_bytes` — before the whole body is buffered, and
///    without trusting `Content-Length`.
/// 3. The parse, and only the parse, runs under an `ExtractionRuntime` permit.
///    Holding a concurrency slot across the body read would let a slow client
///    occupy an extraction slot without extracting anything, which is a
///    trivially cheap way to hold the pool down.
/// 4. Rendering, back on the request task.
///
/// The `Budget` is constructed *inside* the closure because it is `!Sync` by
/// design (its counters are `Cell`s) — it must never be held across an
/// `.await`, and building it on the pool thread makes that unrepresentable
/// rather than merely discouraged.
async fn update_extract(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    method: Method,
    RawQuery(query): RawQuery,
    multipart: Result<axum::extract::Multipart, axum::extract::multipart::MultipartRejection>,
) -> Result<Response, WfError> {
    check_update_method(&method)?;
    let params = Params::parse(query.as_deref().unwrap_or("")).allow_omit_header();
    let extract_err = |class: &'static str, msg: String| {
        WfError::bad_request(class, msg)
            .with_params(&params)
            .envelope(Envelope::NoParams)
    };
    check_core(&state, &core, &params, Envelope::NoParams)?;
    check_params(&state, EXTRACT_PARAMS, &params).map_err(|e| e.envelope(Envelope::NoParams))?;

    // `extractOnly` selects the two modes this handler serves: #258's
    // extract-only response, and #259's Solr-Cell indexing. The resolved
    // boolean, not the param's presence: `extractOnly=false` asks for
    // indexing just as plainly as omitting it does.
    let extract_only = params
        .bool_or("extractOnly", false)
        .map_err(|e| e.envelope(Envelope::NoParams))?;
    // `extractFormat` is only meaningful to the extractOnly response; on the
    // indexing path it is accepted-and-ignored (it remains in the allowlist).
    let as_text = if extract_only {
        match params.get("extractFormat") {
            None | Some("xml") => false,
            Some("text") => true,
            Some(other) => {
                return Err(extract_err(
                    "wayfinder::InvalidParam",
                    format!("invalid extractFormat value: {other}"),
                ));
            }
        }
    } else {
        false
    };

    let mut multipart = multipart.map_err(|e| {
        extract_err(
            "wayfinder::BadContentStream",
            format!("expected a multipart/form-data upload: {e}"),
        )
    })?;

    let mut temp = tempfile::NamedTempFile::new().map_err(|e| {
        WfError::internal(
            "wayfinder::ExtractionIo",
            format!("creating upload temp file: {e}"),
        )
        .with_params(&params)
        .envelope(Envelope::NoParams)
    })?;

    // Every byte the handler consumes is charged against one request-wide
    // total, file part or not. Skipping a non-file field without counting it
    // is what made this route unbounded: `next_field()` drains the skipped
    // field to completion, so an arbitrarily long (or endless chunked) stream
    // of non-file parts was read in full and then answered
    // `MissingContentStream`. The route-level `DefaultBodyLimit`
    // (`route_body_ceiling`) is the backstop for the part *headers* this
    // counter cannot see; this counter is what produces the captured 413.
    let max_body_bytes = state.extract_limits.max_body_bytes;
    let body_ceiling = state.extract_limits.route_body_ceiling() as u64;
    let body_limit_hit = Arc::new(AtomicBool::new(false));
    let mut consumed: u64 = 0;
    // A transport-cap 413 outranks whatever `ChunkSource` managed to report:
    // see `FieldChunks::body_limit_hit`.
    let body_error = |e: extract::ExtractError| {
        let e = if body_limit_hit.load(Ordering::Relaxed) {
            extract::ExtractError::BodyTooLarge {
                limit: body_ceiling,
            }
        } else {
            e
        };
        WfError::from(e).with_params(&params)
    };

    let mut found: Option<(String, String, String, u64)> = None;
    loop {
        let field = multipart.next_field().await.map_err(|e| {
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                return WfError::from(extract::ExtractError::BodyTooLarge {
                    limit: body_ceiling,
                })
                .with_params(&params);
            }
            extract_err(
                "wayfinder::BadContentStream",
                format!("malformed multipart body: {e}"),
            )
        })?;
        let Some(field) = field else { break };
        // An empty `filename=""` is a form field that happens to carry the
        // parameter, not an uploaded document — treated as "no file part"
        // rather than as a document named "".
        let file_name = field.file_name().unwrap_or_default().to_string();
        if file_name.is_empty() {
            let mut chunks = FieldChunks::new(field, Arc::clone(&body_limit_hit));
            extract::drain_counted(&mut chunks, max_body_bytes, &mut consumed)
                .await
                .map_err(body_error)?;
            continue;
        }
        let part_name = field.name().unwrap_or_default().to_string();
        // The raw header, not `Field::content_type()`: the latter goes through
        // the `mime` crate, which lowercases parameter values, and the
        // captured response echoes the client's `Content-Type` **verbatim**
        // (`extract_declared_charset_text.json` keeps `charset=ISO-8859-1`
        // uppercase in both `Content-Type` and `stream_content_type`).
        let declared_type = field
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| OCTET_STREAM.to_string());
        let mut chunks = FieldChunks::new(field, Arc::clone(&body_limit_hit));
        let written = extract::stream_to_tempfile_counted(
            &mut chunks,
            &mut temp,
            max_body_bytes,
            &mut consumed,
        )
        .await
        .map_err(body_error)?;
        found = Some((part_name, file_name, declared_type, written));
        break;
    }
    let Some((part_name, file_name, declared_type, stream_size)) = found else {
        return Err(extract_err(
            "wayfinder::MissingContentStream",
            "multipart body carries no file part to extract".to_string(),
        ));
    };

    // `resource.name` overrides the part's filename for detection and for the
    // echoed `resourceName` (that is what the param is for); the filename is
    // still reported separately as `stream_source_info`.
    let resource_name = params
        .get("resource.name")
        .unwrap_or(&file_name)
        .to_string();

    // ponytail: the document is streamed to a temp file and then read back
    // whole, because `Extractor::extract` takes `&[u8]`. Bounded by
    // `extraction.max_body_bytes` (32 MiB by default), so it is a real
    // ceiling rather than an unbounded one — but it is still a full copy in
    // RAM, and the temp file is currently only buying the streaming *count*.
    // The ceiling is per request, not per server: nothing bounds how many
    // requests are in this read at once, so resident bytes here are
    // `max_body_bytes` x (HTTP concurrency), not `max_body_bytes` x
    // `max_concurrency` — the extraction permit is acquired *after* this
    // point, deliberately (see `ExtractLimits::max_body_bytes`), so it does
    // not cap this. At the defaults that is 32 MiB per concurrent upload.
    // Trigger: the first extractor that can work incrementally (the phase-2a
    // ZIP walker, per `ZipBudget`'s documented call sequence) wants a reader,
    // at which point `ExtractInput` grows a stream variant and this read
    // goes away — *or* sooner, if in-flight-upload bytes are ever bounded
    // globally (item 1 of the route-side design on `max_body_bytes`), which
    // is the same knob.
    let bytes = std::fs::read(temp.path()).map_err(|e| {
        WfError::internal(
            "wayfinder::ExtractionIo",
            format!("reading upload temp file: {e}"),
        )
        .with_params(&params)
        .envelope(Envelope::NoParams)
    })?;

    let limits = state.extract_limits;
    let job_type = declared_type.clone();
    let job_resource = resource_name.clone();
    let doc = state
        .extraction
        .spawn_extraction(limits.deadline, move || {
            let budget = extract::Budget::new(limits);
            extract::extract_document(Some(&job_type), &job_resource, &bytes, &budget)
        })
        .await
        .and_then(|inner| inner)
        .map_err(|e| WfError::from(e).with_params(&params))?;

    if !extract_only {
        // Indexing path (issue #259): apply Solr-Cell field mapping to the
        // extraction and index through the normal `/update` commit path.
        return extract_cell_index(&state, &params, &doc).await;
    }

    let render = extract::ExtractRender {
        part_name: &part_name,
        resource_name: &resource_name,
        stream_source_info: &file_name,
        declared_type: &declared_type,
        stream_size,
        doc: &doc,
    };
    let file = if as_text {
        render.text()
    } else {
        render.xhtml()
    };
    // Solr renders `file_metadata` as a flat NamedList: `[key, [values], ...]`,
    // which is what every captured extract shows and what `json.nl=flat`
    // (the default) means for this writer. Issue #274 made the handler honour
    // `json.nl` rather than allowlisting it and ignoring it: `file_metadata`
    // is a plain (not `SimpleOrderedMap`) NamedList, so it reshapes per the
    // param exactly as a facet bucket list does (finding 128).
    let entries: Vec<(String, Value)> = render
        .file_metadata()
        .into_iter()
        .map(|(key, values)| {
            (
                key,
                Value::Array(values.into_iter().map(Value::String).collect()),
            )
        })
        .collect();
    let metadata = facet::render_named_list(&entries, facet::JsonNl::from_params(&params));

    let mut body = Map::new();
    if !params.omit_header() {
        body.insert(
            "responseHeader".to_string(),
            json!({"status": 0, "QTime": 0}),
        );
    }
    body.insert("file".to_string(), Value::String(file));
    body.insert("file_metadata".to_string(), metadata);
    Ok(axum::Json(Value::Object(body)).into_response())
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

/// Returns raw Unicode-alphanumeric runs with both Rust byte ranges (for
/// slicing/collation) and Java UTF-16 code-unit offsets (for Solr's wire
/// `startOffset`/`endOffset`; `spellcheck_unicode_offsets.json`).
fn spellcheck_tokens(text: &str) -> Vec<(usize, usize, usize, usize, &str)> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut utf16_offset = 0;
    for (byte_offset, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            start.get_or_insert((byte_offset, utf16_offset));
        } else if let Some((byte_start, utf16_start)) = start.take() {
            tokens.push((
                byte_start,
                byte_offset,
                utf16_start,
                utf16_offset,
                &text[byte_start..byte_offset],
            ));
        }
        utf16_offset += ch.len_utf16();
    }
    if let Some((byte_start, utf16_start)) = start {
        tokens.push((
            byte_start,
            text.len(),
            utf16_start,
            utf16_offset,
            &text[byte_start..],
        ));
    }
    tokens
}

/// Builds the narrow issue-#223 spellcheck component from one real Tantivy
/// field dictionary. `dictionary` names `spellcheck_<dictionary>`, and the
/// first repeated request value wins through `Params::get`.
fn spellcheck(index: &CoreIndex, params: &Params) -> anyhow::Result<Value> {
    let map = params.get("json.nl") == Some("map");
    let empty = || {
        let named_list = || {
            if map {
                Value::Object(Map::new())
            } else {
                Value::Array(Vec::new())
            }
        };
        json!({ "suggestions": named_list(), "collations": named_list() })
    };

    let Some(dictionary) = params.get("spellcheck.dictionary") else {
        return Ok(empty());
    };
    let field_name = format!("spellcheck_{dictionary}");
    if !index.resolves_field_name(&field_name) || params.get("spellcheck.q").is_none() {
        return Ok(empty());
    }
    let terms = index.field_terms(&field_name)?;
    let text = params
        .get("spellcheck.q")
        .expect("spellcheck.q presence was checked above");
    let mut corrections = Vec::new();

    // ponytail: this is deliberately not Solr's configurable spellcheck
    // analyzer or ranking pipeline. It only scans raw Unicode-alphanumeric
    // query runs, matches exact dictionary terms verbatim, then chooses one
    // Damerau candidate at edit distance <= 2 (term-ascending tie break).
    // Extend it only with a captured analyzer/ranking contract.
    for (byte_start, byte_end, offset_start, offset_end, token) in spellcheck_tokens(text) {
        if terms.contains_key(token) {
            continue;
        }
        let candidate = terms
            .keys()
            .filter_map(|term| {
                let distance = query::levenshtein(token, term);
                (distance <= 2).then_some((distance, term))
            })
            .min_by(|(distance_a, term_a), (distance_b, term_b)| {
                distance_a.cmp(distance_b).then_with(|| term_a.cmp(term_b))
            })
            .map(|(_, term)| term.clone());
        if let Some(candidate) = candidate {
            corrections.push((
                byte_start,
                byte_end,
                offset_start,
                offset_end,
                token,
                candidate,
            ));
        }
    }

    let suggestions = if map {
        let mut suggestions = Map::new();
        for (_, _, start, end, token, candidate) in &corrections {
            suggestions.insert(
                (*token).to_string(),
                json!({
                    "numFound": 1,
                    "startOffset": start,
                    "endOffset": end,
                    "suggestion": [candidate],
                }),
            );
        }
        Value::Object(suggestions)
    } else {
        let mut suggestions = Vec::with_capacity(corrections.len() * 2);
        for (_, _, start, end, token, candidate) in &corrections {
            suggestions.push(json!(token));
            suggestions.push(json!({
                "numFound": 1,
                "startOffset": start,
                "endOffset": end,
                "suggestion": [candidate],
            }));
        }
        Value::Array(suggestions)
    };

    let collations = if params.bool_or("spellcheck.collate", false)? && !corrections.is_empty() {
        let mut corrected = String::with_capacity(text.len());
        let mut cursor = 0;
        for (byte_start, byte_end, _, _, _, candidate) in &corrections {
            corrected.push_str(&text[cursor..*byte_start]);
            corrected.push_str(candidate);
            cursor = *byte_end;
        }
        corrected.push_str(&text[cursor..]);
        if map {
            json!({ "collation": corrected })
        } else {
            json!(["collation", corrected])
        }
    } else if map {
        Value::Object(Map::new())
    } else {
        Value::Array(Vec::new())
    };

    Ok(json!({ "suggestions": suggestions, "collations": collations }))
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

    // Read *before* the base query runs, matching Solr's own timing: an
    // invalid value here answers with the error-only envelope, no `response`
    // block (`bool_facet_invalid.json`) -- unlike `facet.missing`, which
    // `facet::facet_counts` reads after the query and whose error therefore
    // carries one. `omitHeader` is not read here: `check_params` above already
    // validated it (issue #214).
    let facet_requested = params.bool_or("facet", false)?;
    let stats_requested = params.bool_or("stats", false)?;
    let hl_requested = params.bool_or("hl", false)?;
    // Use the same strict Solr boolean parser as `hl`: only true enables the
    // component; false and an absent param leave its key out of the envelope.
    let spellcheck_requested = params.bool_or("spellcheck", false)?;

    // `bf` and `boost` no longer need the #232 accept-and-warn treatment:
    // function queries are implemented (issue #289), so a function-form
    // `boost` and any `bf` are applied rather than ignored. `select_warnings`
    // still collects facet warnings below.
    let mut select_warnings = Vec::new();

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
            // Solr (`defType` only ever governs `q`). A `q` beginning with a
            // function-query local-params block (`{!func}` / `{!boost b=}`)
            // takes precedence over `defType` — those are query parsers in
            // their own right (issue #289).
            let query = if let Some(func_q) = state
                .index
                .parse_function_query_q(q, &default_field)
                .map_err(|e| query_parse_error(anyhow::Error::from(e), &params))?
            {
                func_q
            } else if params.get("defType") == Some("edismax") {
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
                // `boost` and `bf` are function-query params (issue #289): a
                // plain number like `boost=2` is Solr's simplest constant
                // function, a function form like `boost=product(rating,2)` or
                // any `bf` is evaluated per document. Both are passed raw to
                // `parse_edismax_query`, which applies them.
                let boost = params.get("boost").map(str::to_string);
                let bf: Vec<String> = params
                    .get_all("bf")
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                state
                    .index
                    .parse_edismax_query(
                        q,
                        &default_field,
                        qf,
                        pf,
                        mm,
                        tie,
                        &bq,
                        boost.as_deref(),
                        &bf,
                    )
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

    // Result grouping (issue #290, PRD §5 v3): `group=true` swaps the
    // `response` doclist for a `grouped` envelope keyed by `group.field`. The
    // collector buckets every match by the group field's fast value (a
    // single-valued, non-text field — validated inside `grouping::grouping`,
    // which 400s on undefined / non-fast / multiValued the way Solr does,
    // finding 130). Branching here, before the ungrouped top-N search, means a
    // grouped request never materialises the hits it would then discard.
    //
    // `fl`/`wants_score` are derived the same way the ungrouped path derives
    // them below; duplicated locally so this early branch is self-contained
    // and leaves that path byte-identical.
    let fl_group: Option<Vec<String>> = params
        .get("fl")
        .map(|fl| fl.split(',').map(|s| s.trim().to_string()).collect());
    let wants_score_group = fl_group
        .as_deref()
        .is_some_and(|fl| fl.iter().any(|f| f == "score"));
    if let Some(grouped) = grouping::grouping(
        &state.index,
        &params,
        parsed.as_ref().map(|(q, fqs)| (q.as_ref(), fqs.as_slice())),
        &sort,
        rows,
        start,
        fl_group.as_deref(),
        wants_score_group,
    )? {
        // A grouped response keeps `responseHeader` (gated by `omitHeader`)
        // and replaces `response` with `grouped` — no `facet_counts`/
        // `stats`/`highlighting` block, matching the fixture shape (none of
        // the `group_*` fixtures combine grouping with another component).
        let mut response_header = Map::new();
        response_header.insert("status".to_string(), json!(0));
        response_header.insert("QTime".to_string(), json!(0));
        response_header.insert("params".to_string(), json!(params.echo()));
        let body = if params.omit_header() {
            json!({ "grouped": grouped })
        } else {
            json!({
                "responseHeader": response_header,
                "grouped": grouped,
            })
        };
        return Ok(axum::Json(body).into_response());
    }

    // Fused faceting (issue #246): the `facet.field` terms aggregation runs
    // over exactly the doc set the hit list iterates (`q` AND every `fq`), so
    // planning it here lets the main search compute both in one pass instead
    // of walking the same postings twice — ~5 ms of the 6.6 ms faceting cost
    // at 2M docs was that second walk.
    //
    // A planning error is *discarded*, not reported: the request then takes
    // today's unfused path, which re-derives the identical error at its
    // original point in the request lifecycle, so its message, its
    // `PreQueryFacetError` treatment and whether the envelope carries a
    // `response` block all stay bit-identical. Double validation costs nothing
    // on a request that is already failing.
    let mut facet_field_plan = if facet_requested {
        facet::plan_facet_fields(&state.index, &params)
            .ok()
            .filter(|plan| !plan.fields.is_empty())
    } else {
        None
    };
    // #295: an excluded facet.field (`{!ex=...}`) counts against a reduced
    // filter set, which the fused aggregation (over the full q+fq set) cannot
    // produce. Drop the plan so the dispatch below takes the unfused path,
    // which builds a per-facet base. Multi-select facet requests are rare and
    // not the hot path, so forgoing fusing for them is a deliberate
    // simplification (see `FacetFieldsPlan::exclusion_active`).
    if facet_field_plan
        .as_ref()
        .is_some_and(|plan| plan.exclusion_active)
    {
        facet_field_plan = None;
    }

    // Bounded search (issue #242): only the first `start + rows` hits are
    // materialised; `num_found` and `max_score` still cover every match.
    let mut facet_field_aggs = None;
    let outcome = match &parsed {
        None => crate::collector::TopOutcome {
            num_found: 0,
            max_score: None,
            top: Vec::new(),
        },
        Some((query, filter_queries)) => {
            let unfused = || {
                state
                    .index
                    .search_top(
                        query.as_ref(),
                        filter_queries,
                        &sort,
                        start.saturating_add(rows),
                    )
                    .map_err(|e| {
                        WfError::internal("wayfinder::SearchError", e.to_string())
                            .with_params(&params)
                    })
            };
            // Attempted, not borrowed-through, so the plan can be dropped
            // below without fighting the borrow checker.
            let attempt = facet_field_plan.as_ref().map(|plan| {
                state.index.search_top_with_aggs(
                    query.as_ref(),
                    filter_queries,
                    &sort,
                    start.saturating_add(rows),
                    plan.aggregations.clone(),
                )
            });
            match attempt {
                Some(Ok((outcome, aggs))) => {
                    facet_field_aggs = Some(aggs);
                    outcome
                }
                // An aggregation-class refusal (bucket limit, memory limit,
                // a malformed aggregation) is exactly the error the unfused
                // path raises out of `facet_counts` as a 400
                // `wayfinder::FacetError` with the `response` block attached
                // -- not the 500 `wayfinder::SearchError` it would become
                // here. Un-fuse and let that path answer it, so the wire
                // output stays bit-identical whichever way the request went.
                Some(Err(e)) if crate::core_index::is_aggregation_error(&e) => {
                    facet_field_plan = None;
                    unfused()?
                }
                Some(Err(e)) => {
                    return Err(WfError::internal("wayfinder::SearchError", e.to_string())
                        .with_params(&params));
                }
                None => unfused()?,
            }
        }
    };

    let num_found = outcome.num_found;

    let fl: Option<Vec<String>> = params
        .get("fl")
        .map(|fl| fl.split(',').map(|s| s.trim().to_string()).collect());

    let page = outcome
        .top
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
        // (unpaginated) match set, not just the current page — an
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
        if let Some(max_score) = outcome.max_score {
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
    let facet_result = if facet_requested {
        // Issue #246: when the plan phase succeeded above, `facet.field`'s
        // buckets are already computed and only need shaping; otherwise this
        // is the unchanged, unfused path.
        let counts = match (&facet_field_plan, &facet_field_aggs) {
            (Some(plan), Some(aggs)) => facet::facet_counts_fused(
                &state.index,
                &state.config,
                &params,
                &default_field,
                &base,
                (plan, aggs),
            ),
            _ => facet::facet_counts(&state.index, &state.config, &params, &default_field, &base),
        };
        Some(counts.map_err(|e| {
            // Issue #35: `facet.range` is detected before the base
            // query ever runs (Solr's own `facet_err_range_single.json`
            // has no `response` block), while `facet.query`/
            // `facet.field` errors are detected after it (Solr's
            // `facet_unknown_field.json` / `facet_err_query_single.json`
            // do). `facet::facet_counts` marks the former with
            // `PreQueryFacetError` so only the latter gets `response`
            // attached here.
            let err =
                WfError::bad_request("wayfinder::FacetError", e.to_string()).with_params(&params);
            if e.downcast_ref::<facet::PreQueryFacetError>().is_some() {
                err
            } else {
                err.with_response(Value::Object(response.clone()))
            }
        })?)
    } else {
        None
    };
    // Select-level warnings precede facet warnings because they describe
    // request parameters ignored before facet processing begins.
    if let Some((_, facet_warnings)) = &facet_result {
        select_warnings.extend(facet_warnings.iter().cloned());
    }

    // `stats=true` gates the whole `stats` block the same way `facet=true`
    // gates `facet_counts` — `stats.field` alone does not turn it on (mirrors
    // `facet.field`'s own convention, and matches `stats_key_absent_without_stats_true`).
    let stats_result = if stats_requested {
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
    let highlighting_result = if hl_requested {
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
    if !select_warnings.is_empty() {
        response_header.insert("warnings".to_string(), json!(select_warnings));
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
    if spellcheck_requested {
        body["spellcheck"] = spellcheck(&state.index, &params).map_err(|e| {
            WfError::internal("wayfinder::SpellcheckError", e.to_string()).with_params(&params)
        })?;
    }

    Ok(axum::Json(body).into_response())
}

/// `GET /wayfinder/<core>/mlt` (issue #6, PRD §5). `q` resolves the source
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

    // Both of this handler's own booleans, validated at entry. `omitHeader` is
    // `check_params`'s job (issue #214), not this handler's.
    let mlt_boost = params.bool_or("mlt.boost", false)?;
    // `mlt.match.include=false` drops the `match` key from the envelope
    // entirely -- not an empty-and-present object (finding 100,
    // `mlt_match_include_false.json` is `{responseHeader, response}`).
    // Solr's default is `true`.
    let include_match = params.bool_or("mlt.match.include", true)?;

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

    // Solr parses this as a signed Java int: malformed or out-of-range values
    // are 400s, while zero and negatives mean no analyzer-emitted tokens. It
    // parses `q` first, so a malformed query wins when both values are bad.
    let max_num_tokens_parsed = match params.get("mlt.maxntp") {
        None => 5000,
        Some(value) => value
            .parse::<i32>()
            .map(|value| value.max(0) as usize)
            .map_err(|_| {
                WfError::bad_request(
                    "wayfinder::BadMltParam",
                    format!("invalid mlt.maxntp value `{value}`"),
                )
                .envelope(Envelope::NoParams)
            })?,
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

            // Solr's defaults: mintf=2, mindf=5, maxqt=25, maxntp=5000, no
            // word-length or max-doc-frequency gate, boost=false (equal-weighted terms).
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
                max_num_tokens_parsed,
                min_word_length: params.get("mlt.minwl").and_then(|s| s.parse().ok()),
                max_word_length: params.get("mlt.maxwl").and_then(|s| s.parse().ok()),
                // Tantivy's own boost weighting (relative term score, best
                // term normalised to 1.0) only when `mlt.boost=true`; equal
                // weight (no `BoostQuery` wrapper at all) otherwise.
                boost_factor: mlt_boost.then_some(1.0),
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

/// `GET /wayfinder/<core>/terms` — Solr's TermsComponent (issue #155, PRD's
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
/// - The per-field value is a Solr NamedList of `(term, count)` pairs, so it
///   honours `json.nl` through the same `render_named_list` facets use (finding
///   142 / `terms_prefix_json_nl_map`): `flat` -> `[term, count, ...]` (the
///   default), `map` -> `{term: count}`, `arrarr`/`arrmap` likewise. The outer
///   `terms` object stays keyed by field name under every shape.
/// - Ordering is Solr's `terms.sort=count` default: count descending, ties
///   broken by term ascending. The trace pins both halves — `dog`/`lazi`/
///   `quick` tied at 2 ahead of the singletons, and the singletons themselves
///   alphabetical.
/// - `terms.prefix` filters each field's term dictionary literally before the
///   sort — no analyzer over the prefix, case-sensitive `str::starts_with` on
///   the indexed term; absent or empty means no filter (finding 141).
/// - `terms.limit` defaults to 10 (`TERMS_DEFAULT_LIMIT`), applied per field
///   after the sort; a negative value is the "unlimited" sentinel, and `0`
///   means zero. A non-integer is a 400 with an empty `terms:{}` sibling
///   (finding 142).
/// - `omitHeader=true` (which the module always sends here) drops
///   `responseHeader` entirely.
///
/// An *undefined* `terms.fl` is not an error: finding 141 /
/// `terms_prefix_unknown_field` answers 200 with the field's key present and
/// an empty list (which matters for #308's purpose — stock
/// `search_api_autocomplete` names fields an index may not have). A `terms.fl`
/// naming a *defined but non-text* field is still a 400 — see
/// `check_terms_field`.
///
/// `terms_body.json` and its `solr-ref/manifest.tsv` row cover this endpoint
/// in the differential harness. The one captured analyzer difference is
/// explicit and narrowly guarded under issue #205: Solr's `text_en` emits
/// `dai` where Tantivy emits `day` (finding 103).
async fn terms(
    State(state): State<Arc<AppState>>,
    AxPath(core): AxPath<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, WfError> {
    let params = Params::parse(query.as_deref().unwrap_or("")).allow_omit_header();
    check_core(&state, &core, &params, Envelope::WithParams)?;
    check_params(&state, TERMS_PARAMS, &params)?;

    let mut terms_block = Map::new();
    let terms_requested = params.bool_or("terms", false)?;
    if terms_requested {
        // `terms.limit` is global to the component. A non-integer is the one
        // error case in the set (finding 142 / `terms_limit_invalid`): Solr
        // has already emitted the component's (still-empty) container when the
        // parse fails, so the 400 carries an empty `terms:{}` alongside
        // `error` via `ErrorExtra::terms`.
        let limit =
            parse_terms_limit(&params).map_err(|e| e.with_params(&params).with_terms(json!({})))?;
        // `/terms` is a Solr NamedList, so it honours `json.nl` through the
        // same `render_named_list` facets use (finding 142 /
        // `terms_prefix_json_nl_map`); the outer `terms` object stays keyed
        // by field name under every shape.
        let nl = facet::JsonNl::from_params(&params);
        // `terms.prefix` filters the indexed dictionary literally (no analyzer
        // over the prefix), case-sensitive, applied per field before the sort
        // (finding 141). Absent or empty means no filter.
        let prefix = params.get("terms.prefix").filter(|p| !p.is_empty());
        for field_name in params.get_all("terms.fl") {
            check_terms_field(&state.index, field_name).map_err(|e| e.with_params(&params))?;
            let mut entries: Vec<(String, u64)> = if state.index.resolves_field_name(field_name) {
                state
                    .index
                    .field_terms(field_name)
                    .map_err(|e| {
                        WfError::internal("wayfinder::TermsError", e.to_string())
                            .with_params(&params)
                    })?
                    .into_iter()
                    .filter(|(term, _)| prefix.is_none_or(|p| term.starts_with(p)))
                    .collect()
            } else {
                // An undefined `terms.fl` is not an error: finding 141 /
                // `terms_prefix_unknown_field` answers 200 with the field's
                // key present and an empty list.
                Vec::new()
            };
            // `field_terms` yields terms ascending, so a *stable* sort on
            // count descending leaves equal counts in term-ascending order —
            // Solr's `terms.sort=count` tie-break, asserted against the
            // trace's alphabetical run of singletons. The limit is applied
            // AFTER the sort (finding 142).
            entries.sort_by(|(_, a), (_, b)| b.cmp(a));
            if let Some(limit) = limit {
                entries.truncate(limit);
            }
            let named: Vec<(String, Value)> = entries
                .into_iter()
                .map(|(term, count)| (term, json!(count)))
                .collect();
            terms_block.insert(field_name.to_string(), facet::render_named_list(&named, nl));
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

/// Parses `terms.limit` into a per-field truncation bound (finding 142).
///
/// The `Option<usize>` return is the bound handed to `Vec::truncate`: `None`
/// means "unlimited". An absent param yields the default
/// `Some(TERMS_DEFAULT_LIMIT)`; a **negative** value is Solr's "unlimited"
/// sentinel rather than a clamp-to-zero; `0` is `Some(0)` — zero means zero,
/// not "default". A non-integer is the one error case, a 400 whose body the
/// caller adorns with an empty `terms:{}` sibling via `WfError::with_terms`.
///
/// `-1` is the only negative value captured (`terms_limit_negative`); per the
/// spec any negative is treated as unlimited, and this comment is the honest
/// statement of the extent of that evidence.
fn parse_terms_limit(params: &Params) -> Result<Option<usize>, WfError> {
    match params.get("terms.limit") {
        None => Ok(Some(TERMS_DEFAULT_LIMIT)),
        Some(raw) => match raw.parse::<i64>() {
            // Any negative is the unlimited sentinel (only -1 is captured).
            Ok(n) if n < 0 => Ok(None),
            Ok(n) => Ok(Some(n as usize)),
            Err(_) => Err(WfError::bad_request(
                "wayfinder::TermsInvalidLimit",
                format!("terms.limit is not an integer: \"{raw}\""),
            )),
        },
    }
}

/// Refuses a *defined but non-text* `terms.fl` whose term dictionary does
/// not hold UTF-8 text. (An *undefined* field is no longer refused — see
/// below.)
///
/// **Existence is resolved, not just looked up.** `CoreIndex::
/// resolves_field_name` (the public face of `field_target`, the same
/// static-before-dynamic resolution `/select` uses) accepts a declared
/// `[[fields]]` entry *and* a name only a `[[dynamic_fields]]` pattern matches.
/// An earlier version consulted `WayfinderSchema::field` alone, which 400d
/// `terms.fl=tm_X3b_en_title` — `presets/search-api.toml`'s own `tm_X3b_en_*`
/// rule, and the exact request `solr-ref/search-api/trace/00028.json` captures
/// — as an undefined field while `q=tm_X3b_en_title:lazy` resolved fine on the
/// same core. A name matching neither is no longer an error: finding 141
/// settles that an undefined `terms.fl` answers 200 with an empty list, which
/// the handler renders itself.
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
    // A *defined but non-text* field is refused: its term dictionary holds
    // Tantivy's fixed-width numeric/date encoding, not UTF-8, so decoding it
    // lossily produced replacement-character keys and silently *summed*
    // unrelated frequencies
    // (`terms_non_text_field_is_rejected_rather_than_lossily_decoded`).
    //
    // An *undefined* field is no longer an error (finding 141 /
    // `terms_prefix_unknown_field`): the handler renders it as an empty list,
    // so only the defined-non-text case fails here.
    if index.resolves_field_name(field_name)
        && index.wf_schema.resolved_value_kind(field_name) != Some(schema::ValueKind::Text)
    {
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
