//! Solr faceting (PRD §5): `facet.*` request params -> the `facet_counts`
//! response block.
//!
//! The Tantivy-facing primitives live on `CoreIndex` (`term_facet`, `count`);
//! everything here is Solr wire semantics — which sub-objects exist, term
//! ordering and truncation, the `json.nl=map` shape, and the `facet_ranges`
//! envelope. It is a module of its own rather than more of `lib.rs` so the
//! request-routing file stays small and concurrent branches keep merging
//! mechanically.
//!
//! Facts pinned by fixtures in `solr-ref/responses/`:
//!
//! - `facet=true` gates the whole block; without it the key is absent, not
//!   present-and-empty (findings fact 4).
//! - All five sub-objects are always present when faceting (fact 3).
//! - `facet.field` counts on a **string** field come from the field's whole term
//!   dictionary, not the hit set (`facet_zero.json`, `facet_subset.json`) — see
//!   `CoreIndex::term_facet`, whose `ponytail:` names the ceiling: Tantivy only
//!   enumerates the dictionary for string columns, so numeric and date
//!   `facet.field` report just the values the hit set contains.
//! - `facet.missing` appends a literal `null` key whose count is *hit-set*
//!   based (`facet_missing_no_hit.json`), after the zero-count terms.
//! - `json.nl=map` turns `facet_fields.<name>` and
//!   `facet_ranges.<name>.counts` into objects, and leaves
//!   `gap`/`start`/`end` alone (`facet_json_nl_map.json`,
//!   `facet_range_json_nl_map.json`).
//! - `facet_ranges` bucket keys are strings even for a numeric field, while
//!   `gap`/`start`/`end` echo as JSON numbers for a numeric field and as
//!   strings for a date field, the gap verbatim as the date-math expression
//!   (`facet_range_numeric.json`, `facet_range_date.json`).
//!
//! **Documented divergence** (finding 105, narrowed by issue #26): real Solr
//! answers a facet on an *existing but unfacetable* field — a non-docValues
//! field, or a stored-only field — with HTTP 200 and an empty array. Wayfinder
//! refuses with a 400, because Tantivy has no column to aggregate and a silently
//! empty count block is a wrong answer a client cannot detect.
//!
//! A field that does **not exist** is *not* part of that divergence: real Solr
//! 400s on it too (`facet_unknown_field.json`), so Wayfinder matches. The
//! original fixture said 200 because it was captured against a container whose
//! schema had been polluted by `capture.sh`'s own schemaless probe, which
//! auto-created `nosuchfield` — see issue #26.

use std::fmt;
use std::ops::Bound;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tantivy::query::{BooleanQuery, ExistsQuery, Occur, Query, RangeQuery};
use tantivy::time::format_description::well_known::Rfc3339;
use tantivy::time::{Duration, OffsetDateTime};
use tantivy::{DateTime, Term};

use crate::config::ServerConfig;
use crate::core_index::CoreIndex;
use crate::local_params;
use crate::params::Params;
use crate::schema::{ValueKind, WayfinderSchema};

/// Marks a `facet_counts` error as coming from `facet.range` specifically
/// (issue #35): Solr detects a broken `facet.range` *before* running the base
/// query, so its own fixtures for that case (`facet_err_range_single.json`
/// and friends) carry no `response` block, while a `facet.query`/
/// `facet.field` error is detected *after* the base query has already run and
/// does carry one. This wraps the original error rather than replacing it —
/// `Display` forwards to it verbatim — so `select` in `src/lib.rs` can tell
/// the two apart via `downcast_ref` without changing the message any
/// existing test or fixture comparison sees.
#[derive(Debug)]
pub struct PreQueryFacetError(anyhow::Error);

impl fmt::Display for PreQueryFacetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for PreQueryFacetError {}

/// Solr's `facet.limit` default.
const DEFAULT_FACET_LIMIT: i64 = 100;

/// Ceiling on the number of `facet.range` buckets one request may ask for, so a
/// tiny `facet.range.gap` over a huge span cannot spin the server.
const MAX_RANGE_BUCKETS: usize = 65_536;

/// The base query a facet is computed against: `q` and every `fq`, as `Must`
/// clauses ready to be extended per facet.
pub type BaseClauses = Vec<(Occur, Box<dyn Query>)>;

/// Solr's `json.nl` (named-list) rendering for a bucket list: the default is
/// the flat alternating array (`["apple",2,"banana",1]`); `map` turns it into
/// an object (`facet_json_nl_map.json`); `arrarr` nests each bucket as a
/// two-element array (`facet_json_nl_arrarr.json`) and `arrmap` as a
/// one-entry object per bucket (`facet_json_nl_arrmap.json`, finding 41c).
/// Applies identically to `facet_fields.<name>` and
/// `facet_ranges.<name>.counts`; `gap`/`start`/`end` are never affected.
///
/// The same enum renders the `/update/extract` `file_metadata` NamedList
/// (issue #274, finding 128): that NamedList reshapes per `json.nl` exactly
/// as a facet bucket list does, so the param is honoured there rather than
/// merely allowlisted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonNl {
    Flat,
    Map,
    ArrArr,
    ArrMap,
}

impl JsonNl {
    pub(crate) fn from_params(params: &Params) -> JsonNl {
        match params.get("json.nl") {
            Some("map") => JsonNl::Map,
            Some("arrarr") => JsonNl::ArrArr,
            Some("arrmap") => JsonNl::ArrMap,
            _ => JsonNl::Flat,
        }
    }
}

/// Builds the whole `facet_counts` block, plus any `responseHeader.warnings`
/// it earned (issue #24 — Solr's own mincount-raise warning for a `facet.field`
/// on a Points-based column). Every error is caused by the request (an
/// unfacetable field, an unparseable `facet.query`, a bad range spec), so the
/// caller renders them all as 400s.
pub fn facet_counts(
    index: &CoreIndex,
    config: &ServerConfig,
    params: &Params,
    default_field: &str,
    base: &BaseClauses,
) -> Result<(Value, Vec<String>)> {
    facet_counts_inner(index, config, params, default_field, base, None)
}

/// [`facet_counts`] with `facet.field` answered from the aggregation results
/// the main `/select` pass already produced (issue #246), instead of from a
/// second walk over the same doc set. Every other sub-object — `facet.query`,
/// `facet.range` — keeps its own pass and its own error precedence, so the
/// emitted block is byte-identical to [`facet_counts`]'s.
pub fn facet_counts_fused(
    index: &CoreIndex,
    config: &ServerConfig,
    params: &Params,
    default_field: &str,
    base: &BaseClauses,
    fused: (
        &FacetFieldsPlan,
        &tantivy::aggregation::agg_result::AggregationResults,
    ),
) -> Result<(Value, Vec<String>)> {
    facet_counts_inner(index, config, params, default_field, base, Some(fused))
}

fn facet_counts_inner(
    index: &CoreIndex,
    config: &ServerConfig,
    params: &Params,
    default_field: &str,
    base: &BaseClauses,
    fused: Option<(
        &FacetFieldsPlan,
        &tantivy::aggregation::agg_result::AggregationResults,
    )>,
) -> Result<(Value, Vec<String>)> {
    let nl = JsonNl::from_params(params);

    // Evaluation order is `facet.range` -> `facet.query` -> `facet.field`
    // (finding 38 / issue #30): when more than one facet param is broken at
    // once, Solr reports exactly one error and that precedence decides which.
    // The *emitted* key order of `facet_counts` below stays
    // queries/fields/ranges/intervals/heatmaps regardless — that is a
    // separate, order-sensitive contract (`tests/json_key_order.rs`) — so the
    // results are hoisted into bindings here, evaluated range-first, and only
    // placed into the `json!` object in the unchanged key order.
    //
    // `fused` carries issue #246's already-computed `facet.field` buckets when
    // `select` was able to plan them before the main search; `None` is the
    // unfused path, which runs `facet_fields`' own aggregation passes here.
    let facet_ranges = facet_ranges(index, params, base, nl)
        .map_err(|e| anyhow::Error::new(PreQueryFacetError(e)))?;
    // #295: the `{!tag=...}` each `fq` carries, so a facet's `{!ex=...}` can
    // drop the clauses it names. Aligned positionally with `base`'s trailing
    // fq clauses (index 0 is the main query).
    let fq_tags = fq_tag_lists(params);
    let facet_queries = facet_queries(index, params, default_field, base, &fq_tags)?;
    let (facet_fields, warnings) = match fused {
        Some((plan, agg_results)) => {
            render_facet_fields(index, config, params, base, plan, agg_results)?
        }
        None => facet_fields(index, config, params, base, nl, &fq_tags)?,
    };
    let mut counts = Map::new();
    counts.insert("facet_queries".to_string(), facet_queries);
    counts.insert("facet_fields".to_string(), facet_fields);
    counts.insert("facet_ranges".to_string(), facet_ranges);
    // Out of scope (PRD §5 leaves them for later): the keys are present and
    // empty because Solr always emits all five (findings fact 3).
    counts.insert("facet_intervals".to_string(), json!({}));
    counts.insert("facet_heatmaps".to_string(), json!({}));
    Ok((Value::Object(counts), warnings))
}

/// Clones `base` and adds `extra` as another `Must` clause.
pub(crate) fn narrowed(base: &BaseClauses, occur: Occur, extra: Box<dyn Query>) -> BooleanQuery {
    let mut clauses: BaseClauses = base
        .iter()
        .map(|(occur, query)| (*occur, query.box_clone()))
        .collect();
    clauses.push((occur, extra));
    BooleanQuery::from(clauses)
}

/// The comma-separated local-param `key` (e.g. `tag` or `ex`) on a param
/// *value*, as a list. `{!tag=a,b}...` -> `["a", "b"]`; a value with no block,
/// or an empty value (`{!ex=}`) -> `[]`. Reads `fq` tags and `facet.field`/
/// `facet.query` exclusions without re-parsing the query — the query itself is
/// parsed separately (`parse_query` strips these inert prefixes, #295).
fn local_param_csv(value: &str, key: &str) -> Vec<String> {
    let Some((local, _)) = local_params::parse_block(value) else {
        return Vec::new();
    };
    match local.get(key) {
        None | Some("") => Vec::new(),
        Some(s) => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
    }
}

/// One tag list per `fq` param, in request order — aligned positionally with
/// the trailing `fq` clauses of `base` (whose index 0 is the main `q` clause,
/// built in `select`). `base`'s `fq` clauses and these lists line up 1:1
/// whenever the request reached faceting, because a parse failure on any `fq`
/// already 400'd the whole request.
fn fq_tag_lists(params: &Params) -> Vec<Vec<String>> {
    params
        .get_all("fq")
        .into_iter()
        .map(|fq| local_param_csv(fq, "tag"))
        .collect()
}

/// `base` with every `fq` clause whose `{!tag=...}` intersects `excluded`
/// dropped. `base` is `[main_query, fq_0, fq_1, ...]` (the shape `select`
/// builds), so the leading main query is never excludable — a `{!tag}` on `q`
/// is inert (finding 136) — and the per-`fq` tags align positionally with
/// `fq_tags`. An `excluded` entry naming no set tag drops nothing, which is
/// the silent no-op finding 136 requires; matching is set-intersection, so a
/// single `{!tag=a,b}` fq is dropped by `{!ex=b}` (finding 137).
fn excluded_base_clauses(
    base: &BaseClauses,
    fq_tags: &[Vec<String>],
    excluded: &[String],
) -> BaseClauses {
    let mut clauses: BaseClauses = Vec::with_capacity(base.len());
    if let Some((occur, query)) = base.first() {
        clauses.push((*occur, query.box_clone()));
    }
    for (i, (occur, query)) in base.get(1..).unwrap_or(&[]).iter().enumerate() {
        let excluded_here = fq_tags
            .get(i)
            .is_some_and(|tags| tags.iter().any(|t| excluded.iter().any(|e| e == t)));
        if !excluded_here {
            clauses.push((*occur, query.box_clone()));
        }
    }
    clauses
}

/// `facet.query`, repeatable. The key is the query string verbatim and the
/// value is how many documents match it *and* `q` *and* every `fq`
/// (`facet_query_with_fq.json`). A facet query matching nothing keeps its key,
/// at 0 (`facet_query_zero.json`). A `{!ex=...}` prefix (#295, finding 139)
/// counts with the tagged `fq` clauses excluded, while the key still carries
/// the `{!ex=...}` text verbatim — Solr does not strip local params from
/// `facet_queries` keys.
///
/// ponytail: only `ex` is read off a `facet.query` block. Solr also honours
/// `facet.*` settings carried as local params there (the same
/// `SimpleFacets.parseParams` mechanism issue #296 implemented for
/// `facet.field`, finding 148), but nothing here captures that shape and a
/// `facet.query` bucket is a single count with no list to limit, sort or
/// mincount — so the only setting with any meaning would be `facet.missing`,
/// which has no `facet_queries` rendering at all. Capture before implementing.
fn facet_queries(
    index: &CoreIndex,
    params: &Params,
    default_field: &str,
    base: &BaseClauses,
    fq_tags: &[Vec<String>],
) -> Result<Value> {
    let mut out = Map::new();
    for facet_query in params.get_all("facet.query") {
        let parsed = index.parse_query(facet_query, default_field)?;
        let excluded = local_param_csv(facet_query, "ex");
        let count = if excluded.is_empty() {
            index.count(&narrowed(base, Occur::Must, parsed))?
        } else {
            let reduced = excluded_base_clauses(base, fq_tags, &excluded);
            index.count(&narrowed(&reduced, Occur::Must, parsed))?
        };
        out.insert(facet_query.to_string(), json!(count));
    }
    Ok(Value::Object(out))
}

/// Splits one `facet.field` value into `(response label, field to facet on)`.
///
/// `{!key=mylabel}category` counts `category` and labels the bucket `mylabel`
/// (issue #138, `facet_local_params_key.json`). The key is *only* a label: it
/// is never resolved as a field, not even when it names another declared field
/// — `{!key=body}category` is a 200 carrying `category`'s counts under `body`,
/// although `body` is not itself facetable
/// (`facet_local_params_key_as_other_field.json`).
///
/// Everything that is not a parseable block is its own label and field,
/// byte-for-byte. That is what keeps the un-prefixed path untouched, and it is
/// also why an unterminated `{!key=mylabel category` 400s: `parse_block`
/// reports "not a block", the whole value stays a field name, and no such
/// field exists (`facet_local_params_key_unterminated.json` — Solr 400s there
/// too, as a block syntax error). A block with nothing after it yields the
/// empty field name, which is the token the 400 then names, matching
/// `facet_local_params_key_empty_remainder.json`'s `undefined field: ""`.
///
/// ponytail: this function reads only `key` (the label). `ex` is read
/// separately in `plan_facet_fields`/`facet_queries` for #295's filter
/// exclusion, and the inline `facet.limit`/`facet.mincount`/`facet.sort`/
/// `facet.missing` settings in `FacetSettings::resolve` for #296's; all of
/// them compose in any order (finding 138) because they come off the same
/// parsed block. A `tag` on a `facet.field` value is meaningless (tags label
/// `fq` clauses) and is dropped, as is every other local param -- `facet.prefix`
/// and `facet.method` included, both uncaptured and unimplemented in either
/// form. A repeated `key` is first-wins, matching Solr's `{!key=a
/// key=b}category` capture (finding 108).
///
fn split_facet_key(value: &str) -> (String, &str) {
    match local_params::parse_block(value) {
        Some((local, consumed)) => {
            let field = &value[consumed..];
            let label = local.get("key").unwrap_or(field).to_string();
            (label, field)
        }
        None => (value.to_string(), value),
    }
}

/// `facet.field`, repeatable — one key per field, each counted independently
/// (`facet_multi_field.json`). Each value may carry a `{!key=...}` local-params
/// prefix, which relabels its bucket (see `split_facet_key`). Returns the
/// sub-object plus any `responseHeader.warnings` earned along the way.
fn facet_fields(
    index: &CoreIndex,
    config: &ServerConfig,
    params: &Params,
    base: &BaseClauses,
    nl: JsonNl,
    fq_tags: &[Vec<String>],
) -> Result<(Value, Vec<String>)> {
    let plan = plan_facet_fields(index, params)?;
    if plan.fields.is_empty() {
        return Ok((json!({}), Vec::new()));
    }

    // The full filter set (q AND every fq) every non-excluded facet counts
    // against, built once.
    let base_query = BooleanQuery::from(
        base.iter()
            .map(|(occur, query)| (*occur, query.box_clone()))
            .collect::<BaseClauses>(),
    );

    let mut out = Map::new();
    for field in &plan.fields {
        let shaping = BucketShaping::for_field(config, &field.settings);
        // #295: `{!ex=...}` on a facet.field counts against the filter set with
        // the tagged fq clauses dropped (finding 136). An empty or
        // non-matching `ex` reduces to the full set, so the common no-exclusion
        // path keeps one shared `base_query`. `facet.missing` (shape_field,
        // finding 140) must read the same base the buckets were counted
        // against, so the reduced set is threaded to it too.
        let reduced = if field.ex.is_empty() {
            None
        } else {
            Some(excluded_base_clauses(base, fq_tags, &field.ex))
        };
        let counts = match reduced.as_ref() {
            Some(clauses) => {
                let query = BooleanQuery::from(
                    clauses
                        .iter()
                        .map(|(occur, q)| (*occur, q.box_clone()))
                        .collect::<BaseClauses>(),
                );
                index.term_facet(&field.column, field.kind, &query)?
            }
            None => index.term_facet(&field.column, field.kind, &base_query)?,
        };
        let missing_base = reduced.as_ref().unwrap_or(base);
        out.insert(
            field.label.clone(),
            shape_field(index, missing_base, field, counts, &shaping, nl)?,
        );
    }
    Ok((Value::Object(out), plan.warnings.clone()))
}

/// One `facet.field` value, resolved and validated (issue #246's plan phase).
/// Everything here is decided purely from the request params and the schema —
/// no query runs — so the caller can build the aggregation request *before*
/// the main `/select` search and fuse the two into one pass.
#[derive(Debug)]
pub struct FacetFieldPlan {
    /// The key this facet's buckets appear under in `facet_fields`, i.e. the
    /// `{!key=...}` label if there is one, otherwise the field name.
    pub label: String,
    /// The Tantivy column actually aggregated over: `field_name` itself, or a
    /// dynamic field's catch-all JSON path (issue #66).
    pub column: String,
    /// The schema-declared kind backing `column`, which decides the bucket
    /// key rendering (`ValueKind::F64`'s Java `Double.toString`, dates).
    pub kind: Option<ValueKind>,
    /// This field's key inside the shared `Aggregations` map. Unique per
    /// requested value, not per field: `facet.field=category&
    /// facet.field={!key=other}category` is two aggregations over one column.
    pub agg_name: String,
    /// The effective `facet.missing` for this facet, after the
    /// `f.<field>.facet.missing` override and the local-param form.
    pub missing: bool,
    /// The effective `facet.limit`/`facet.mincount`/`facet.sort` for this
    /// facet, after the same precedence (issue #296, finding 151). Per facet
    /// rather than per request: two `facet.field` values over one column may
    /// legitimately disagree, and only the local-param form can say so
    /// (finding 149).
    pub settings: FacetSettings,
    /// The `{!ex=...}` tag list on this facet.field (#295): count this facet
    /// against the filter set with the `fq` clauses carrying any of these
    /// tags dropped. Empty for a plain `facet.field`, which counts the full
    /// set.
    pub ex: Vec<String>,
}

/// The whole `facet.field` request, planned: one entry per requested value,
/// the single `Aggregations` map that computes all of them in one pass, and
/// the `responseHeader.warnings` the plan itself earned.
#[derive(Debug)]
pub struct FacetFieldsPlan {
    pub fields: Vec<FacetFieldPlan>,
    pub aggregations: tantivy::aggregation::agg_req::Aggregations,
    pub warnings: Vec<String>,
    /// True when any planned facet.field carries a non-empty `{!ex=...}`
    /// (#295): such a facet counts a reduced filter set the fused `/select`
    /// aggregation (against the full q+fq set) cannot provide, so `select`
    /// falls back to the unfused path rather than fusing.
    pub exclusion_active: bool,
}

/// One facet's `facet.limit`/`facet.mincount`/`facet.sort`, resolved for that
/// facet alone (issue #296). The limit is the *requested* value, before
/// `query.facet_limit_max` clamping — clamping needs the server config, which
/// the plan phase does not have.
#[derive(Debug)]
pub struct FacetSettings {
    /// `facet.limit`, negative meaning "as many as the server allows".
    pub limit: i64,
    /// `facet.mincount`.
    pub mincount: u64,
    /// `facet.sort`: `Some(true)` is `index`, `Some(false)` is `count`, `None`
    /// is unset — which Solr resolves against this facet's own limit, not the
    /// global one (see [`BucketShaping::for_field`]).
    pub by_index: Option<bool>,
}

/// The value of one facet setting for one facet, taking Solr's *addressed*
/// forms only: the per-field param `f.<field>.<name>` first, then the local
/// param of the same name on this `facet.field` value. `None` leaves the
/// caller on the bare global.
///
/// That order is finding 151, and it is the counter-intuitive half of it: a
/// local param normally shadows the request, but `SimpleFacets.parseParams`
/// wraps the local params as *defaults* under the request params and then
/// reads the setting through `getFieldParam(field, name)`, which tries
/// `f.<field>.<name>` before the bare name — so a local param can only ever
/// beat the global, never the per-field form
/// (`facet_perfield_prec_lp_vs_field.json` / `_lp_vs_global.json`).
///
/// `field` is the field being faceted, never a `{!key=...}` label: finding
/// 147, the premise issue #296 was filed on and the one it got backwards.
fn addressed_setting<'a>(
    params: &'a Params,
    local: &'a local_params::LocalParams,
    field: &str,
    name: &str,
) -> Option<&'a str> {
    params
        .get(&format!("f.{field}.{name}"))
        .or_else(|| local.get(name))
}

/// [`addressed_setting`] parsed as a number, with Solr's own
/// `NumberFormatException` wording for a value that is not one
/// (`facet_perfield_err_bad_limit.json`: `For input string: "abc"`).
///
/// ponytail: only the *addressed* forms validate. A non-numeric bare global
/// `facet.limit`/`facet.mincount` is still silently defaulted below, as it was
/// before this function existed — real Solr 400s on that too, but no fixture
/// pins it and changing it would be a behaviour change outside issue #296.
fn addressed_number<T: std::str::FromStr>(
    params: &Params,
    local: &local_params::LocalParams,
    field: &str,
    name: &str,
) -> Result<Option<T>> {
    match addressed_setting(params, local, field, name) {
        None => Ok(None),
        Some(raw) => match raw.parse::<T>() {
            Ok(value) => Ok(Some(value)),
            Err(_) => bail!("For input string: \"{raw}\""),
        },
    }
}

impl FacetSettings {
    /// Resolves one facet's settings: addressed form (per-field param, then
    /// local param) over the bare global, per finding 151.
    fn resolve(
        params: &Params,
        local: &local_params::LocalParams,
        field: &str,
        global_mincount: u64,
    ) -> Result<FacetSettings> {
        let limit = match addressed_number::<i64>(params, local, field, "facet.limit")? {
            Some(limit) => limit,
            None => params
                .get("facet.limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_FACET_LIMIT),
        };
        let mincount = match addressed_number::<u64>(params, local, field, "facet.mincount")? {
            Some(mincount) => mincount,
            None => global_mincount,
        };
        let by_index = addressed_setting(params, local, field, "facet.sort")
            .or_else(|| params.get("facet.sort"))
            .map(|sort| sort == "index");
        Ok(FacetSettings {
            limit,
            mincount,
            by_index,
        })
    }
}

/// Plan/validate phase of `facet.field` (issue #246): resolves every requested
/// value to its label, column and `ValueKind`, applies the collision and
/// facetability checks, earns the Points-based `mincount` warning, and builds
/// the terms aggregations — all without executing a single query, so `select`
/// can attach the aggregations to the main search pass instead of paying for a
/// second walk over the same doc set.
///
/// Every error this returns is one `facet_fields` itself would have returned,
/// with the same wording: `select` discards a failed plan and falls back to
/// the unfused path, which re-derives the identical error at its original
/// point in the request lifecycle.
pub fn plan_facet_fields(index: &CoreIndex, params: &Params) -> Result<FacetFieldsPlan> {
    let values = params.get_all("facet.field");
    if values.is_empty() {
        return Ok(FacetFieldsPlan {
            fields: Vec::new(),
            aggregations: tantivy::aggregation::agg_req::Aggregations::default(),
            warnings: Vec::new(),
            exclusion_active: false,
        });
    }

    let global_mincount: u64 = params
        .get("facet.mincount")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    // Issue #187: Solr's own boolean parsing, so `facet.missing=yes`/`on`/
    // `TRUE`/`truestuff` all count as on and `nope` is a 400. The `WfError`
    // is deliberately let out through this module's `anyhow` result rather
    // than returned directly: `select` rebuilds it from `e.to_string()` on
    // the non-`PreQueryFacetError` path, which is what attaches the base
    // query's `response` block (`bool_facet_missing_invalid.json`).
    let global_missing = params.bool_or("facet.missing", false)?;

    let mut fields: Vec<FacetFieldPlan> = Vec::new();
    let mut aggregations = tantivy::aggregation::agg_req::Aggregations::default();
    let mut warnings = Vec::new();
    for (i, value) in values.into_iter().enumerate() {
        // The label reaches the response envelope; the field reaches
        // resolution, validation and every error message (issue #138).
        let (label, field_name) = split_facet_key(value);
        // #295: `{!ex=...}` on this facet.field -- the tags to drop from the
        // filter set when counting it (finding 136). `key`/`ex` compose in
        // either order, and `split_facet_key` already took the `key` label.
        let ex = local_param_csv(value, "ex");
        // #296 (finding 148): the same block may also carry this facet's
        // `facet.limit`/`facet.mincount`/`facet.sort`/`facet.missing`. An
        // unparseable value is not a block at all, which is the pre-existing
        // "the whole value is a field name" path (`split_facet_key`), so an
        // empty set of local params is the right reading here.
        let local = local_params::parse_block(value)
            .map(|(local, _)| local)
            .unwrap_or_default();
        // Finding 102: Solr can emit duplicate `facet_fields` object members,
        // but serde_json's Map cannot represent them. Refuse before validating
        // or aggregating the second field rather than silently overwriting the
        // first; `facet.query` intentionally remains coalesced above.
        if fields.iter().any(|f| f.label == label) {
            bail!("colliding facet.field response label: {label}");
        }
        check_facetable(&index.wf_schema, field_name, true)?;
        // The Tantivy column to actually aggregate over: `field_name` itself
        // for a static field, or the catch-all JSON path for a field that
        // only matches a `[[dynamic_fields]]` pattern (issue #66) —
        // `check_facetable` above already proved one of the two resolves.
        let column = index
            .wf_schema
            .resolved_fast_column(field_name)
            .expect("check_facetable proved this field resolves");

        // Solr's own behaviour (issue #24, `facet_field_numeric_all.json`):
        // `facet.field` on a Points-based (numeric/date) column raises the
        // effective `facet.mincount` from 0 to 1 and says so in
        // `responseHeader.warnings`, verbatim wording included ("Points-based"
        // is Solr's term, not a description of Wayfinder's own schema). It
        // never applies to a string field — `facet_field_string_control_subset`
        // has no such warning — and never to `facet.range`, which is a
        // separate code path this function does not touch. The raise itself
        // has no observable effect on the counts (no zero-count numeric bucket
        // can exist for `min_doc_count: 0` to introduce), so it is purely a
        // header-honesty concern; the actual `mincount` filtering below is
        // left at its requested value.
        //
        // #296: the mincount it tests is this facet's effective one, not the
        // bare global — a facet that asked for `facet.mincount=1` through
        // either addressed form has nothing to be raised.
        let settings = FacetSettings::resolve(params, &local, field_name, global_mincount)?;
        let kind = index.wf_schema.resolved_value_kind(field_name);
        let is_points_based = kind.is_some_and(|kind| kind != ValueKind::Text);
        if is_points_based && settings.mincount == 0 {
            warnings.push(format!(
                "Raising facet.mincount from 0 to 1, because field {field_name} is Points-based."
            ));
        }

        // `f.<field>.facet.missing` overrides the global `facet.missing` for
        // this field, unconditionally — it wins whether the global is unset,
        // `true` or `false`, in both directions (issue #140, finding 97:
        // `facet_missing_field_override_wins_over_global_true.json` /
        // `_false.json`). It keys off `field_name`, the field actually being
        // faceted, never `label` from a `{!key=...}` prefix
        // (`facet_local_params_key_f_field.json` / `_f_key.json`), so an
        // override naming a field nobody passed to `facet.field` is inert.
        //
        // #296 (finding 148): the local-param form of the same setting sits
        // between the two — below `f.<field>.facet.missing`, above the global
        // (`facet_perfield_lp_missing.json`). It goes through the same
        // `parse_bool` as every other boolean, so `{!facet.missing=yes}` is on
        // and `{!facet.missing=nope}` is the usual 400.
        let missing = match params.per_field_bool(field_name, "facet.missing")? {
            Some(missing) => missing,
            None => match local.get("facet.missing") {
                Some(raw) => crate::params::parse_bool(raw)
                    .ok_or_else(|| anyhow!(crate::params::invalid_bool_msg(raw)))?,
                None => global_missing,
            },
        };

        // Keyed by request position, not by field name: two `facet.field`
        // values may legitimately name the same column under different
        // `{!key=}` labels, and a name collision here would silently give
        // them one shared bucket list.
        let agg_name = format!("wf_facet_{i}");
        aggregations.insert(
            agg_name.clone(),
            crate::core_index::terms_aggregation(&column),
        );

        fields.push(FacetFieldPlan {
            label,
            column,
            kind,
            agg_name,
            missing,
            settings,
            ex,
        });
    }
    let exclusion_active = fields.iter().any(|f| !f.ex.is_empty());
    Ok(FacetFieldsPlan {
        fields,
        aggregations,
        warnings,
        exclusion_active,
    })
}

/// Render phase of `facet.field` (issue #246): takes the buckets the fused
/// `/select` pass already computed and applies `facet.mincount` /
/// `facet.sort` / `facet.limit` / `facet.missing` to them, producing exactly
/// the `facet_fields` object — and the same warnings — the unfused
/// `facet_fields` would have produced from its own separate passes.
pub fn render_facet_fields(
    index: &CoreIndex,
    config: &ServerConfig,
    params: &Params,
    base: &BaseClauses,
    plan: &FacetFieldsPlan,
    agg_results: &tantivy::aggregation::agg_result::AggregationResults,
) -> Result<(Value, Vec<String>)> {
    if plan.fields.is_empty() {
        return Ok((json!({}), Vec::new()));
    }
    let nl = JsonNl::from_params(params);

    let mut out = Map::new();
    for field in &plan.fields {
        let shaping = BucketShaping::for_field(config, &field.settings);
        let counts = crate::core_index::render_term_facet_buckets(
            &field.column,
            field.kind,
            agg_results,
            &field.agg_name,
        )?;
        out.insert(
            field.label.clone(),
            shape_field(index, base, field, counts, &shaping, nl)?,
        );
    }
    Ok((Value::Object(out), plan.warnings.clone()))
}

/// The response-shaping half of `facet.field`: `facet.mincount`,
/// `facet.limit` (clamped by `query.facet_limit_max`) and `facet.sort`.
/// Derived per facet from that facet's own [`FacetSettings`] (issue #296), by
/// both the fused and the unfused path.
struct BucketShaping {
    mincount: u64,
    limit: usize,
    by_index: bool,
}

impl BucketShaping {
    fn for_field(config: &ServerConfig, settings: &FacetSettings) -> BucketShaping {
        // `query.facet_limit_max` is a Wayfinder cap with no Solr equivalent,
        // so (like `rows_limit`) an over-limit request is clamped rather than
        // rejected, and `-1` means "as many as the server allows".
        let limit = if settings.limit < 0 {
            config.query.facet_limit_max
        } else {
            (settings.limit as usize).min(config.query.facet_limit_max)
        };
        // Solr's `facet.sort` default is `count` when the requested limit is
        // positive and `index` otherwise — against *this facet's* limit, since
        // both are now per facet.
        let by_index = settings.by_index.unwrap_or(settings.limit <= 0);
        BucketShaping {
            mincount: settings.mincount,
            limit,
            by_index,
        }
    }
}

/// Filters, orders, truncates and renders one field's raw term buckets, and
/// appends the `facet.missing` bucket if this field asked for one. The buckets
/// come either from a standalone `term_facet` pass or from the fused
/// `/select` aggregation — identical either way, which is the whole premise
/// issue #246 rests on.
fn shape_field(
    index: &CoreIndex,
    base: &BaseClauses,
    field: &FacetFieldPlan,
    mut counts: Vec<(String, crate::core_index::FacetOrderKey, u64)>,
    shaping: &BucketShaping,
    nl: JsonNl,
) -> Result<Value> {
    counts.retain(|(_, _, count)| *count >= shaping.mincount);
    if shaping.by_index {
        counts.sort_by(|a, b| a.1.cmp(&b.1));
    } else {
        counts.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    }
    counts.truncate(shaping.limit);

    let mut buckets: Vec<(Option<String>, u64)> = counts
        .into_iter()
        .map(|(term, _, count)| (Some(term), count))
        .collect();
    if field.missing {
        // Solr emits the `null` bucket last and unconditionally — it is not
        // subject to `facet.mincount` or `facet.limit`. Its count is the
        // number of *hits* with no value in the field, read from the fast
        // field column (`ExistsQuery`), never from stored values.
        let has_value = ExistsQuery::new(field.column.clone(), false);
        let absent = index.count(&narrowed(base, Occur::MustNot, Box::new(has_value)))?;
        buckets.push((None, absent as u64));
    }
    Ok(render_buckets(&buckets, nl))
}

/// `facet.range` + `facet.range.start` / `.end` / `.gap`, repeatable per field.
/// Each bucket is counted with a real range query over the fast field, so an
/// empty interior bucket is still emitted, at 0 (`facet_range_date.json`).
///
/// ponytail: only the global `facet.range.*` params, no `f.<field>.facet.range.*`
/// per-field overrides and no `facet.range.other` / `.include` / `.hardend`.
fn facet_ranges(
    index: &CoreIndex,
    params: &Params,
    base: &BaseClauses,
    nl: JsonNl,
) -> Result<Value> {
    let fields = params.get_all("facet.range");
    if fields.is_empty() {
        return Ok(json!({}));
    }

    let mut out = Map::new();
    for field_name in fields {
        check_facetable(&index.wf_schema, field_name, false)?;
        let field = index
            .wf_schema
            .field(field_name)
            .expect("check_facetable proved the field is declared");
        let kind = index
            .wf_schema
            .value_kind(field_name)
            .expect("a declared field always has a value kind");

        let start = required(params, "facet.range.start", field_name)?;
        let end = required(params, "facet.range.end", field_name)?;
        let gap = required(params, "facet.range.gap", field_name)?;

        let bucket_spans = range_buckets(field_name, kind, start, end, gap)?;
        // A span not divisible by the gap extends the last bucket to the gap
        // boundary (`[20,30)` for start 0/end 22/gap 10) and Solr echoes THAT
        // aligned boundary back as `end`, not the requested value (finding
        // 41b, `facet_range_end_not_gap_aligned.json`: end 30, not 22). With
        // zero buckets (an empty span) there is no walked boundary to align
        // to, so the requested `end` is echoed verbatim — unpinned by any
        // fixture, but the least surprising fallback.
        //
        // ponytail: two more shapes past what is captured are unfixtured
        // here. (a) The `F64` walk accumulates `lower + gap` bucket by
        // bucket, so an *aligned* double request (start 0 / end 0.3 / gap
        // 0.1) now echoes the walked `0.30000000000000004` rather than the
        // requested `0.3` — a real behaviour change from plain `echo_bound`
        // on a path no fixture exercises. (b) The date echo now goes through
        // `echo_range_end` -> `format_date`, so a millisecond-precision
        // request with an exactly-zero fraction (`end=2020-01-06T00:00:00.000Z`)
        // echoes `...:00Z`, while `start` still echoes the request string
        // verbatim via `echo_bound` — `start` and `end` now render by
        // different rules for the same kind of value. The only cases actually
        // captured are aligned-date (`facet_range_date.json`) and
        // non-aligned-i64 (`facet_range_end_not_gap_aligned.json`); the
        // raw-vs-normalised `start`/`end` asymmetry, and the float
        // accumulation drift, both need a capture before relying on them.
        let end_echo = match bucket_spans.last() {
            Some((_, _, upper)) => echo_range_end(kind, *upper),
            None => echo_bound(kind, end),
        };

        let mut buckets = Vec::new();
        for (key, lower, upper) in bucket_spans {
            let bucket = RangeQuery::new(
                Bound::Included(lower.to_term(field)),
                Bound::Excluded(upper.to_term(field)),
            );
            let count = index.count(&narrowed(base, Occur::Must, Box::new(bucket)))?;
            buckets.push((Some(key), count as u64));
        }

        out.insert(
            field_name.to_string(),
            json!({
                "counts": render_buckets(&buckets, nl),
                "gap": echo_bound(kind, gap),
                "start": echo_bound(kind, start),
                "end": end_echo,
            }),
        );
    }
    Ok(Value::Object(out))
}

/// One end of a range-facet bucket, in the field's own type so the range query
/// gets an exact `Term` rather than a lossy `f64`.
#[derive(Debug, Clone, Copy)]
enum RangeEnd {
    I64(i64),
    F64(f64),
    Date(DateTime),
}

impl RangeEnd {
    fn to_term(self, field: tantivy::schema::Field) -> Term {
        match self {
            RangeEnd::I64(v) => Term::from_field_i64(field, v),
            RangeEnd::F64(v) => Term::from_field_f64(field, v),
            RangeEnd::Date(v) => Term::from_field_date(field, v),
        }
    }
}

/// The bucket list for one range facet: `(key, lower_inclusive,
/// upper_exclusive)`, walking `start` towards `end` in `gap` steps. The key is
/// always a string, even for a numeric field (`facet_range_numeric.json`).
fn range_buckets(
    field_name: &str,
    kind: ValueKind,
    start: &str,
    end: &str,
    gap: &str,
) -> Result<Vec<(String, RangeEnd, RangeEnd)>> {
    let mut out = Vec::new();
    match kind {
        ValueKind::I64 => {
            let (start, end) = (
                parse_i64(field_name, "start", start)?,
                parse_i64(field_name, "end", end)?,
            );
            let gap = parse_i64(field_name, "gap", gap)?;
            if gap <= 0 {
                bail!("facet.range.gap for field `{field_name}` must be positive, got `{gap}`");
            }
            let mut lower = start;
            while lower < end {
                let upper = lower.saturating_add(gap);
                out.push((
                    lower.to_string(),
                    RangeEnd::I64(lower),
                    RangeEnd::I64(upper),
                ));
                guard_bucket_count(field_name, out.len())?;
                lower = upper;
            }
        }
        ValueKind::F64 => {
            let (start, end) = (
                parse_f64(field_name, "start", start)?,
                parse_f64(field_name, "end", end)?,
            );
            let gap = parse_f64(field_name, "gap", gap)?;
            // `NaN` has to fail here too, or the bucket walk below never
            // terminates — hence the explicit `is_nan`, not a negated `>`.
            if gap.is_nan() || gap <= 0.0 {
                bail!("facet.range.gap for field `{field_name}` must be positive, got `{gap}`");
            }
            let mut lower = start;
            while lower < end {
                let upper = lower + gap;
                out.push((
                    // ponytail: this is the exact "5" vs "5.0" bug finding 39
                    // just fixed for `facet.field` (`CoreIndex::term_facet` /
                    // `render_double`) — an integral `facet.range` bucket
                    // boundary on a double/float field renders `"0"`/`"10"`
                    // here via plain `f64::to_string()`, not Java
                    // `Double.toString`'s `"0.0"`/`"10.0"`. Out of this
                    // issue's scope (no `facet.range` fixture on `price`/
                    // `rating` was captured) and left unfixed, but it is the
                    // same divergence, not a different one — revisit
                    // alongside `render_double` if/when a range fixture on a
                    // double/float field lands.
                    lower.to_string(),
                    RangeEnd::F64(lower),
                    RangeEnd::F64(upper),
                ));
                guard_bucket_count(field_name, out.len())?;
                lower = upper;
            }
        }
        ValueKind::Date => {
            let start = parse_date(field_name, "start", start)?;
            let end = parse_date(field_name, "end", end)?;
            let gap = parse_date_gap(field_name, gap)?;
            let mut lower = start;
            while lower < end {
                // `OffsetDateTime + Duration` *panics* on overflow (time 0.3's
                // `Add` impl unwraps `checked_add` internally), and both ends of
                // this walk come straight from the request — so a start near
                // year 9999 with a day gap would take the handler task down and
                // drop the connection instead of answering the error envelope.
                let upper = lower.checked_add(gap).ok_or_else(|| {
                    anyhow!(
                        "facet.range on field `{field_name}` overflows the representable date \
                         range at `{}`; narrow facet.range.end or facet.range.gap",
                        format_date(lower).unwrap_or_else(|_| "?".to_string())
                    )
                })?;
                out.push((
                    format_date(lower)?,
                    RangeEnd::Date(DateTime::from_utc(lower)),
                    RangeEnd::Date(DateTime::from_utc(upper)),
                ));
                guard_bucket_count(field_name, out.len())?;
                lower = upper;
            }
        }
        ValueKind::Text => bail!(
            "can not range facet on the text field `{field_name}`: \
             facet.range needs a numeric or date field"
        ),
    }
    Ok(out)
}

fn guard_bucket_count(field_name: &str, count: usize) -> Result<()> {
    if count > MAX_RANGE_BUCKETS {
        bail!(
            "facet.range on field `{field_name}` asks for more than {MAX_RANGE_BUCKETS} buckets; \
             widen facet.range.gap"
        );
    }
    Ok(())
}

/// `gap`/`start`/`end` echo back as JSON numbers for a numeric field and as
/// strings for a date field, the date gap verbatim as the date-math expression
/// it was given (`facet_range_numeric.json` vs `facet_range_date.json`).
fn echo_bound(kind: ValueKind, raw: &str) -> Value {
    match kind {
        ValueKind::I64 => raw.parse::<i64>().map(Value::from).unwrap_or(json!(raw)),
        ValueKind::F64 => raw.parse::<f64>().map(Value::from).unwrap_or(json!(raw)),
        _ => json!(raw),
    }
}

/// Echoes a walked bucket boundary (a `RangeEnd`, already typed) the same way
/// `echo_bound` echoes a requested one: a JSON number for a numeric field, an
/// RFC3339 string for a date field — used for the gap-aligned `end` (finding
/// 41b), where the value comes from the bucket walk rather than straight off
/// the request string.
fn echo_range_end(kind: ValueKind, end: RangeEnd) -> Value {
    match (kind, end) {
        (ValueKind::I64, RangeEnd::I64(v)) => json!(v),
        (ValueKind::F64, RangeEnd::F64(v)) => json!(v),
        (ValueKind::Date, RangeEnd::Date(v)) => match format_date(v.into_utc()) {
            Ok(s) => json!(s),
            Err(_) => Value::Null,
        },
        // `range_buckets` only ever produces the `RangeEnd` variant matching
        // its own `kind` argument, so a mismatch here would be a Wayfinder
        // bug, not a request one — panicking is louder than silently
        // rendering the wrong shape.
        (kind, end) => unreachable!("range end variant {end:?} does not match field kind {kind:?}"),
    }
}

fn required<'a>(params: &'a Params, key: &str, field_name: &str) -> Result<&'a str> {
    params
        .get(key)
        .ok_or_else(|| anyhow!("facet.range on field `{field_name}` requires `{key}`"))
}

fn parse_i64(field_name: &str, which: &str, raw: &str) -> Result<i64> {
    raw.trim().parse().map_err(|_| {
        anyhow!("facet.range.{which} for field `{field_name}` is not an integer: `{raw}`")
    })
}

fn parse_f64(field_name: &str, which: &str, raw: &str) -> Result<f64> {
    raw.trim().parse().map_err(|_| {
        anyhow!("facet.range.{which} for field `{field_name}` is not a number: `{raw}`")
    })
}

fn parse_date(field_name: &str, which: &str, raw: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(raw.trim(), &Rfc3339).map_err(|e| {
        anyhow!(
            "facet.range.{which} for field `{field_name}` is not an RFC3339 date: `{raw}` ({e})"
        )
    })
}

/// Solr date math, restricted to the fixed-length units.
///
/// ponytail: `+1MONTH` / `+1YEAR` need a calendar-aware DateMathParser (month
/// lengths vary), so they are refused by name rather than silently rounded.
/// `facet_range_date.json` pins `+1DAY`, which is all this issue needs.
fn parse_date_gap(field_name: &str, raw: &str) -> Result<Duration> {
    let spec = raw.trim().strip_prefix('+').unwrap_or(raw.trim());
    let split = spec
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| anyhow!("facet.range.gap for field `{field_name}` has no unit: `{raw}`"))?;
    let (amount, unit) = spec.split_at(split);
    let amount: i64 = amount.parse().map_err(|_| {
        anyhow!("facet.range.gap for field `{field_name}` has no leading amount: `{raw}`")
    })?;
    if amount <= 0 {
        bail!("facet.range.gap for field `{field_name}` must be positive, got `{raw}`");
    }
    let unit_seconds = match unit.trim_end_matches('S') {
        "SECOND" => 1,
        "MINUTE" => 60,
        "HOUR" => 3_600,
        "DAY" => 86_400,
        "WEEK" => 604_800,
        "MONTH" | "YEAR" => bail!(
            "facet.range.gap `{raw}` on field `{field_name}` needs calendar-aware date math, \
             which Wayfinder does not implement yet; use a DAY/HOUR/MINUTE/SECOND gap"
        ),
        other => bail!("unsupported facet.range.gap unit `{other}` on field `{field_name}`"),
    };
    Ok(Duration::seconds(amount.saturating_mul(unit_seconds)))
}

fn format_date(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .map_err(|e| anyhow!("could not render date bucket key: {e}"))
}

/// Renders a Solr `NamedList` (an ordered name→value sequence) as the JSON
/// shape `json.nl` selects: `Flat`'s alternating `[name, value, ...]` array
/// (the default), `Map`'s object, `ArrArr`'s `[name, value]` pairs, or
/// `ArrMap`'s one-entry `{name: value}` objects. Each entry's `value` is
/// already-rendered JSON, placed verbatim into the chosen shape.
///
/// Used by the extract route's `file_metadata` (issue #274, finding 128),
/// whose NamedList is a plain one in Solr and so honours `json.nl` — unlike
/// `responseHeader`, which is a `SimpleOrderedMap` and stays an object under
/// every value. Keys here are always strings (extract `file_metadata` has no
/// null-key analogue to facet's `facet.missing` bucket), so unlike
/// [`render_buckets`] there is no null→`""`/`null` rendering to negotiate.
pub(crate) fn render_named_list(entries: &[(String, Value)], nl: JsonNl) -> Value {
    match nl {
        JsonNl::Map => {
            let mut map = Map::new();
            for (name, value) in entries {
                // Solr's NamedList permits duplicate names (last wins in the
                // object render); no extract fixture has any — verified across
                // every flat capture — so this is the documented fallback
                // rather than a path exercised by a fixture.
                map.insert(name.clone(), value.clone());
            }
            Value::Object(map)
        }
        JsonNl::ArrArr => Value::Array(
            entries
                .iter()
                .map(|(name, value)| Value::Array(vec![Value::String(name.clone()), value.clone()]))
                .collect(),
        ),
        JsonNl::ArrMap => Value::Array(
            entries
                .iter()
                .map(|(name, value)| {
                    let mut m = Map::new();
                    m.insert(name.clone(), value.clone());
                    Value::Object(m)
                })
                .collect(),
        ),
        JsonNl::Flat => {
            let mut flat = Vec::with_capacity(entries.len() * 2);
            for (name, value) in entries {
                flat.push(Value::String(name.clone()));
                flat.push(value.clone());
            }
            Value::Array(flat)
        }
    }
}

/// Renders a bucket list as Solr's `json.nl` shape (findings fact 1, finding
/// 41c): `Flat`'s alternating array (default), `Map`'s object, `ArrArr`'s
/// nested two-element arrays, or `ArrMap`'s nested one-entry objects. Applies
/// identically to `facet_fields.<name>` and `facet_ranges.<name>.counts`.
/// `None` is the `facet.missing` bucket's literal `null` key.
fn render_buckets(buckets: &[(Option<String>, u64)], nl: JsonNl) -> Value {
    match nl {
        JsonNl::Map => {
            let mut map = Map::new();
            for (term, count) in buckets {
                // `json.nl=map` plus `facet.missing` keys the null bucket as
                // the empty string — a JSON object cannot have a `null` key,
                // and this is exactly what Wayfinder already did before it
                // was pinned by a fixture (`facet_json_nl_map_missing.json`,
                // finding 41d).
                let key = term.clone().unwrap_or_default();
                map.insert(key, json!(count));
            }
            Value::Object(map)
        }
        JsonNl::ArrArr => Value::Array(
            buckets
                .iter()
                .map(|(term, count)| {
                    // ponytail: no fixture pins `arrarr` plus `facet.missing`
                    // together; a JSON array *can* hold a `null` element
                    // (unlike `arrmap`'s object keys), so the null bucket's
                    // term renders as JSON `null` here, consistent with the
                    // flat array's own treatment of it above.
                    let key = match term {
                        Some(term) => json!(term),
                        None => Value::Null,
                    };
                    json!([key, count])
                })
                .collect(),
        ),
        JsonNl::ArrMap => Value::Array(
            buckets
                .iter()
                .map(|(term, count)| {
                    // ponytail: no fixture pins `arrmap` plus `facet.missing`
                    // — a JSON object key still cannot be `null`, so this
                    // mirrors `Map`'s empty-string choice rather than
                    // inventing a new shape. Capture before relying on it.
                    let key = term.clone().unwrap_or_default();
                    json!({ key: count })
                })
                .collect(),
        ),
        JsonNl::Flat => {
            let mut flat = Vec::with_capacity(buckets.len() * 2);
            for (term, count) in buckets {
                flat.push(match term {
                    Some(term) => json!(term),
                    None => Value::Null,
                });
                flat.push(json!(count));
            }
            Value::Array(flat)
        }
    }
}

/// Refuses a facet Tantivy cannot compute, rather than returning empty counts.
///
/// This is the whole point of the issue: aggregation needs a fast (docValues)
/// column, and without one the only honest answers are an error or a lie.
/// Deliberate divergence from Solr, which answers 200 with an empty array for
/// all three of these cases — see the module docs and finding 105.
///
/// `allow_dynamic` also resolves `field_name` through a `[[dynamic_fields]]`
/// pattern match, mirroring the static-before-dynamic precedence indexing
/// already uses (issue #66). `facet.field` (`facet_fields`) opts in — its
/// aggregation runs against whatever Tantivy column name it is given, so the
/// catch-all JSON path works exactly like a real one. `facet.range`
/// (`facet_ranges`) does not: it also needs the field's own physical `Field`
/// handle via `WayfinderSchema::field`, which a dynamic-only match has none of
/// (only the catch-all container does) — so it keeps the pre-#66 static-only
/// check rather than resolving through here into a field with no handle.
fn check_facetable(schema: &WayfinderSchema, field_name: &str, allow_dynamic: bool) -> Result<()> {
    let fast = if allow_dynamic {
        schema.resolved_fast(field_name)
    } else {
        schema.field_config(field_name).map(|f| f.fast)
    };
    match fast {
        None if !allow_dynamic && schema.resolved_fast(field_name) == Some(true) => {
            // ponytail: `facet.range` only resolves statically-declared
            // fields (see the module doc above `check_facetable`) because it
            // needs the field's physical `Field` handle for
            // `Term::from_field_i64`/etc, which a dynamic-only match has no
            // handle for (only the catch-all container field does). So a
            // field that matches a `[[dynamic_fields]]` pattern — and would
            // be perfectly facetable via `facet.field` — is still refused
            // here. Upgrade when `RangeEnd::to_term` (or its callers) no
            // longer needs a physical `Field`, then flip this to
            // `allow_dynamic = true` like `facet.field` already is.
            bail!(
                "field {field_name} matches a dynamic pattern but facet.range does not yet support dynamic fields"
            )
        }
        None => bail!("can not facet on undefined field: {field_name}"),
        Some(false) => bail!("can not facet on a field w/o fast values (docValues): {field_name}"),
        Some(true) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    // -----------------------------------------------------------------
    // Issue #246: split `facet_fields` into a pure plan/validate phase
    // (`plan_facet_fields`) and a render phase (`render_facet_fields`) that
    // takes aggregation buckets back. Neither exists yet -- every test below
    // is a compile-error red until issue #246 adds them.
    //
    // Contract these tests pin (additive, chosen by stage 1 since the task
    // spec describes the shape but not the exact signature):
    //
    //   pub struct FacetFieldPlan {
    //       pub label: String,
    //       pub field_name: String,
    //       pub column: String,       // `resolved_fast_column`'s output
    //       pub kind: Option<ValueKind>,
    //       pub agg_name: String,     // key inside `aggregations`
    //       pub missing: bool,
    //   }
    //   pub struct FacetFieldsPlan {
    //       pub fields: Vec<FacetFieldPlan>,
    //       pub aggregations: tantivy::aggregation::agg_req::Aggregations,
    //       pub warnings: Vec<String>,
    //   }
    //   pub fn plan_facet_fields(index: &CoreIndex, params: &Params) -> Result<FacetFieldsPlan>;
    //   pub fn render_facet_fields(
    //       index: &CoreIndex,
    //       config: &ServerConfig,
    //       params: &Params,
    //       base: &BaseClauses,
    //       plan: &FacetFieldsPlan,
    //       agg_results: &tantivy::aggregation::agg_result::AggregationResults,
    //   ) -> Result<(Value, Vec<String>)>;
    // -----------------------------------------------------------------

    const PLAN_RENDER_SCHEMA_TOML: &str = r#"
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
name = "category"
type = "string"
stored = true
fast = true

[[fields]]
name = "views"
type = "int"
stored = true
fast = true
"#;

    fn open_plan_render_index() -> (TempDir, CoreIndex) {
        let dir = TempDir::new().expect("create temp dir");
        let schema_path = dir.path().join("schema.toml");
        std::fs::write(&schema_path, PLAN_RENDER_SCHEMA_TOML).expect("write schema.toml");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let index = CoreIndex::open(&schema_path, &data_dir, &ServerConfig::default())
            .expect("open test index");
        (dir, index)
    }

    /// Two commits (two segments) so the fused/unfused equivalence test below
    /// has real segment-merge behaviour to get wrong, a string field
    /// (`category`) and a numeric field (`views`), and enough docs missing
    /// each that `facet.missing` and `facet.mincount` both have real material.
    fn open_plan_render_corpus() -> (TempDir, CoreIndex) {
        let (dir, index) = open_plan_render_index();
        for (batch, ids) in [(0, 0..15), (1, 15..30)] {
            let docs: Vec<Value> = ids
                .map(|i: i32| {
                    let body = if i % 4 == 0 {
                        "quick brown fox".to_string()
                    } else {
                        "quick fox jumps".to_string()
                    };
                    let mut doc = json!({"id": format!("doc{i:02}"), "body": body});
                    if i % 4 != 0 {
                        doc["category"] = json!(["animals", "birds", "fish"][(i % 3) as usize]);
                        doc["views"] = json!((i % 5) as i64 * 10);
                    }
                    doc
                })
                .collect();
            index.add_documents(&docs, true).expect("add_documents");
            index.commit().expect("commit");
            let _ = batch;
        }
        (dir, index)
    }

    /// The plan phase resolves label/field/column/kind purely from `params` +
    /// schema, with no search executed at all -- proven here by succeeding
    /// against an index with zero committed segments, which a query-executing
    /// implementation would have no principled reason to special-case.
    #[test]
    fn plan_facet_fields_does_not_require_any_committed_data() {
        let (_dir, index) = open_plan_render_index();
        let params = Params::parse("facet.field=category");

        let plan = plan_facet_fields(&index, &params)
            .expect("the plan phase must succeed with zero committed segments");
        assert_eq!(plan.fields.len(), 1);
        assert_eq!(plan.fields[0].label, "category");
        // `column` and `kind` are the two resolutions the render phase
        // actually consumes (the aggregated Tantivy column, and the bucket-key
        // rendering rule), so a plan that "succeeded" without resolving them
        // would satisfy the label assertion alone while being useless.
        assert_eq!(plan.fields[0].column, "category");
        assert_eq!(plan.fields[0].kind, Some(ValueKind::Text));
    }

    /// The `TermsAggregation` the plan phase builds must be the exact shape
    /// `CoreIndex::term_facet` builds today: full-dictionary `size`/
    /// `segment_size` and `min_doc_count: 0`. A plan that requested a smaller
    /// size, or a nonzero `min_doc_count`, would silently truncate or drop
    /// zero-count buckets the unfused path never drops.
    #[test]
    fn plan_facet_fields_terms_aggregation_matches_term_facets_shape() {
        use tantivy::aggregation::DEFAULT_BUCKET_LIMIT;
        use tantivy::aggregation::agg_req::AggregationVariants;

        let (_dir, index) = open_plan_render_corpus();
        let params = Params::parse("facet.field=category");

        let plan = plan_facet_fields(&index, &params).expect("plan_facet_fields");
        assert_eq!(plan.fields.len(), 1);
        let agg = plan
            .aggregations
            .get(&plan.fields[0].agg_name)
            .expect("the plan's Aggregations map must carry an entry for its own field");
        let AggregationVariants::Terms(terms) = &agg.agg else {
            panic!("expected a Terms aggregation, got {:?}", agg.agg);
        };
        assert_eq!(terms.field, "category");
        assert_eq!(terms.size, Some(DEFAULT_BUCKET_LIMIT));
        assert_eq!(terms.segment_size, Some(DEFAULT_BUCKET_LIMIT));
        assert_eq!(terms.min_doc_count, Some(0));
    }

    /// Issue #24's Points-based mincount warning is a plan-time fact (it
    /// depends only on the field's declared kind and the requested
    /// `facet.mincount`, never on the hit set), so the plan phase must
    /// surface it exactly as `facet_fields` does today, verbatim wording
    /// included.
    #[test]
    fn plan_facet_fields_warns_on_points_based_field_at_mincount_zero() {
        let (_dir, index) = open_plan_render_corpus();
        let params = Params::parse("facet.field=views");

        let plan = plan_facet_fields(&index, &params).expect("plan_facet_fields");
        assert_eq!(
            plan.warnings,
            vec![
                "Raising facet.mincount from 0 to 1, because field views is Points-based."
                    .to_string()
            ]
        );

        let params_string_field = Params::parse("facet.field=category");
        let plan_string =
            plan_facet_fields(&index, &params_string_field).expect("plan_facet_fields");
        assert!(
            plan_string.warnings.is_empty(),
            "a string field must never earn the Points-based warning"
        );
    }

    /// Error wording is part of the wire contract (it lands verbatim in the
    /// JSON `error` block), so the plan phase's errors must match
    /// `facet_fields`'s own errors byte-for-byte, not just "also fail".
    #[test]
    fn plan_facet_fields_errors_match_facet_fields_errors() {
        let (_dir, index) = open_plan_render_corpus();
        let config = ServerConfig::default();
        let query = index.parse_query("quick", "body").expect("parse_query");
        let base: BaseClauses = vec![(Occur::Must, query.box_clone())];

        for query_string in [
            "facet.field=nosuchfield",
            "facet.field=category&facet.field={!key=category}views",
            "facet.field=body",
        ] {
            let params = Params::parse(query_string);
            let nl = JsonNl::from_params(&params);

            let plan_err = plan_facet_fields(&index, &params)
                .expect_err(&format!("`{query_string}` must be a plan-time error"))
                .to_string();
            let facet_fields_err = facet_fields(&index, &config, &params, &base, nl, &[])
                .expect_err(&format!(
                    "`{query_string}` must still error out of facet_fields"
                ))
                .to_string();

            assert_eq!(
                plan_err, facet_fields_err,
                "plan_facet_fields and facet_fields must disagree on nothing for `{query_string}`"
            );
        }
    }

    /// The whole point of the split: plan then fuse then render must produce
    /// byte-identical `facet_fields` output (and the same warnings) to
    /// today's single-pass `facet_fields`, across mincount/limit/sort/missing
    /// combinations, single and multiple `facet.field`, and with and without
    /// `fq` -- proving the render phase's key-rendering rules (F64
    /// `Double.toString`, date keys, `key_as_string`) moved rather than
    /// changed, and that the fused aggregation ran over the right doc set
    /// (identical counts, not just identical shape).
    #[test]
    fn plan_then_render_matches_facet_fields_across_param_combinations() {
        use tantivy::aggregation::agg_result::AggregationResults;

        let (_dir, index) = open_plan_render_corpus();
        let config = ServerConfig::default();
        let query = index.parse_query("quick", "body").expect("parse_query");

        for query_string in [
            "facet.field=category",
            "facet.field=views",
            "facet.field=category&facet.field=views",
            "facet.field=category&facet.mincount=2",
            "facet.field=category&facet.limit=1",
            "facet.field=category&facet.sort=index",
            "facet.field=category&facet.missing=true",
            "facet.field=views&facet.missing=true&facet.limit=-1",
            "facet.field={!key=mylabel}category",
        ] {
            for with_fq in [false, true] {
                let qs = if with_fq {
                    format!("{query_string}&fq=category:animals")
                } else {
                    query_string.to_string()
                };
                let params = Params::parse(&qs);

                let filter_queries: Vec<Box<dyn Query>> = params
                    .get_all("fq")
                    .into_iter()
                    .map(|fq| index.parse_query(fq, "body").expect("parse fq"))
                    .collect();
                let base: BaseClauses = std::iter::once((Occur::Must, query.box_clone()))
                    .chain(
                        filter_queries
                            .iter()
                            .map(|fq| (Occur::Must, fq.box_clone())),
                    )
                    .collect();
                let nl = JsonNl::from_params(&params);

                let (expected_fields, expected_warnings) =
                    facet_fields(&index, &config, &params, &base, nl, &[])
                        .unwrap_or_else(|e| panic!("facet_fields for `{qs}`: {e}"));

                let plan = plan_facet_fields(&index, &params)
                    .unwrap_or_else(|e| panic!("plan_facet_fields for `{qs}`: {e}"));
                let (_top, agg_results): (crate::collector::TopOutcome, AggregationResults) = index
                    .search_top_with_aggs(
                        query.as_ref(),
                        &filter_queries,
                        &[],
                        5,
                        plan.aggregations.clone(),
                    )
                    .unwrap_or_else(|e| panic!("search_top_with_aggs for `{qs}`: {e}"));
                let (fused_fields, fused_warnings) =
                    render_facet_fields(&index, &config, &params, &base, &plan, &agg_results)
                        .unwrap_or_else(|e| panic!("render_facet_fields for `{qs}`: {e}"));

                assert_eq!(
                    fused_fields, expected_fields,
                    "facet_fields JSON diverged between the fused and unfused paths for `{qs}`"
                );
                assert_eq!(
                    fused_warnings, expected_warnings,
                    "facet warnings diverged between the fused and unfused paths for `{qs}`"
                );
            }
        }
    }

    /// Mutation-test gap closed here (issue #138): replacing `parse_block` with
    /// a `split('}')`-style prefix strip passes every request-level test in
    /// `tests/facet_local_params_key.rs`, because none of them sends a value
    /// that contains a `}` *without* being a well-formed block. These cases do.
    /// A value that is not a `{!...}` block is a field name byte-for-byte —
    /// `parse_block` requires the `{!` sigil and a closing `}`, and Solr only
    /// treats a value as local params under the same condition — so a `}` in
    /// ordinary text must not be mistaken for a block terminator.
    #[test]
    fn a_value_that_is_not_a_block_is_its_own_label_and_field() {
        for value in [
            "category",
            // A stray `}` mid-value: a `split('}')` strip would facet on
            // `egory` (or label the bucket `cat`) instead of refusing the whole
            // string as an undefined field.
            "cat}egory",
            "}category",
            "category}",
            // Not a block: `{` without the `!` sigil (Tantivy range syntax).
            "{a TO b}",
            // A block sigil with no closing brace is not a block either, which
            // is what makes `facet_local_params_key_unterminated.json`'s 400
            // fall out of the undefined-field path.
            "{!key=mylabel category",
        ] {
            assert_eq!(
                split_facet_key(value),
                (value.to_string(), value),
                "`{value}` is not a local-params block, so it must pass through untouched"
            );
        }
    }

    /// The block grammar, not a brace scan: a `}` inside a quoted local-param
    /// value does not end the block, so the field is what follows the *real*
    /// terminator. Pins that `split_facet_key` inherits
    /// `local_params::parse_block`'s quoting rules rather than reimplementing a
    /// looser scan.
    #[test]
    fn a_quoted_brace_inside_the_block_does_not_end_it() {
        assert_eq!(
            split_facet_key("{!key='a} b'}category"),
            ("a} b".to_string(), "category")
        );
    }

    /// The three shapes the fixtures pin, at the unit level: a differing key, a
    /// key equal to the field, and a key naming another declared field (which
    /// is still only a label).
    #[test]
    fn the_key_is_the_label_and_the_remainder_is_the_field() {
        assert_eq!(
            split_facet_key("{!key=mylabel}category"),
            ("mylabel".to_string(), "category")
        );
        assert_eq!(
            split_facet_key("{!key=category}category"),
            ("category".to_string(), "category")
        );
        assert_eq!(
            split_facet_key("{!key=body}category"),
            ("body".to_string(), "category")
        );
    }

    /// `facet_local_params_key_empty_remainder.json`: the parsed empty
    /// remainder is what gets validated, so the field must be `""` — not the
    /// key, and not the raw value.
    #[test]
    fn a_block_with_no_remainder_yields_the_empty_field_name() {
        assert_eq!(
            split_facet_key("{!key=mylabel}"),
            ("mylabel".to_string(), "")
        );
    }

    /// A block carrying no `key` at all falls back to the field name as its own
    /// label, so `{!ex=tagname}category` still buckets under `category` rather
    /// than under the raw value. Unfixtured — see `split_facet_key`'s ponytail.
    #[test]
    fn a_block_without_a_key_labels_with_the_field_name() {
        assert_eq!(
            split_facet_key("{!ex=tagname}category"),
            ("category".to_string(), "category")
        );
    }
}
