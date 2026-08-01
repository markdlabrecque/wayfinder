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
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonNl {
    Flat,
    Map,
    ArrArr,
    ArrMap,
}

impl JsonNl {
    fn from_params(params: &Params) -> JsonNl {
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
    let nl = JsonNl::from_params(params);

    // Evaluation order is `facet.range` -> `facet.query` -> `facet.field`
    // (finding 38 / issue #30): when more than one facet param is broken at
    // once, Solr reports exactly one error and that precedence decides which.
    // The *emitted* key order of `facet_counts` below stays
    // queries/fields/ranges/intervals/heatmaps regardless — that is a
    // separate, order-sensitive contract (`tests/json_key_order.rs`) — so the
    // results are hoisted into bindings here, evaluated range-first, and only
    // placed into the `json!` object in the unchanged key order.
    let facet_ranges = facet_ranges(index, params, base, nl)
        .map_err(|e| anyhow::Error::new(PreQueryFacetError(e)))?;
    let facet_queries = facet_queries(index, params, default_field, base)?;
    let (facet_fields, warnings) = facet_fields(index, config, params, base, nl)?;
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

/// `facet.query`, repeatable. The key is the query string verbatim and the
/// value is how many documents match it *and* `q` *and* every `fq`
/// (`facet_query_with_fq.json`). A facet query matching nothing keeps its key,
/// at 0 (`facet_query_zero.json`).
fn facet_queries(
    index: &CoreIndex,
    params: &Params,
    default_field: &str,
    base: &BaseClauses,
) -> Result<Value> {
    let mut out = Map::new();
    for facet_query in params.get_all("facet.query") {
        let parsed = index.parse_query(facet_query, default_field)?;
        let count = index.count(&narrowed(base, Occur::Must, parsed))?;
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
/// ponytail: `key` is the only local param read; every other one is parsed and
/// dropped. `tag`/`ex` need multi-select faceting and inline `facet.*` params
/// inside the block are adjacent to issue #140's `f.<field>.facet.*`, neither
/// of which is captured here — so a request using them is answered as if they
/// were absent rather than refused. Capture before relying on that. A repeated
/// `key` is first-wins, matching Solr's `{!key=a key=b}category` capture
/// (finding 108).
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
) -> Result<(Value, Vec<String>)> {
    let fields = params.get_all("facet.field");
    if fields.is_empty() {
        return Ok((json!({}), Vec::new()));
    }

    let mincount: u64 = params
        .get("facet.mincount")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let requested_limit: i64 = params
        .get("facet.limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_FACET_LIMIT);
    // `query.facet_limit_max` is a Wayfinder cap with no Solr equivalent, so
    // (like `rows_limit`) an over-limit request is clamped rather than
    // rejected, and `-1` means "as many as the server allows".
    let limit = if requested_limit < 0 {
        config.query.facet_limit_max
    } else {
        (requested_limit as usize).min(config.query.facet_limit_max)
    };
    // Solr's `facet.sort` default is `count` when the requested limit is
    // positive and `index` otherwise.
    let by_index = match params.get("facet.sort") {
        Some("index") => true,
        Some(_) => false,
        None => requested_limit <= 0,
    };
    // Issue #187: Solr's own boolean parsing, so `facet.missing=yes`/`on`/
    // `TRUE`/`truestuff` all count as on and `nope` is a 400. The `WfError`
    // is deliberately let out through this module's `anyhow` result rather
    // than returned directly: `select` rebuilds it from `e.to_string()` on
    // the non-`PreQueryFacetError` path, which is what attaches the base
    // query's `response` block (`bool_facet_missing_invalid.json`).
    let global_missing = params.bool_or("facet.missing", false)?;

    let base_query = BooleanQuery::from(
        base.iter()
            .map(|(occur, query)| (*occur, query.box_clone()))
            .collect::<BaseClauses>(),
    );

    let mut out = Map::new();
    let mut warnings = Vec::new();
    for value in fields {
        // The label reaches the response envelope; the field reaches
        // resolution, validation and every error message (issue #138).
        let (label, field_name) = split_facet_key(value);
        // Finding 102: Solr can emit duplicate `facet_fields` object members,
        // but serde_json's Map cannot represent them. Refuse before validating
        // or aggregating the second field rather than silently overwriting the
        // first; `facet.query` intentionally remains coalesced above.
        if out.contains_key(&label) {
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
        let kind = index.wf_schema.resolved_value_kind(field_name);
        let is_points_based = kind.is_some_and(|kind| kind != ValueKind::Text);
        if is_points_based && mincount == 0 {
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
        let missing = params
            .per_field_bool(field_name, "facet.missing")?
            .unwrap_or(global_missing);

        let mut counts = index.term_facet(&column, kind, &base_query)?;
        counts.retain(|(_, _, count)| *count >= mincount);
        if by_index {
            counts.sort_by(|a, b| a.1.cmp(&b.1));
        } else {
            counts.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        }
        counts.truncate(limit);

        let mut buckets: Vec<(Option<String>, u64)> = counts
            .into_iter()
            .map(|(term, _, count)| (Some(term), count))
            .collect();
        if missing {
            // Solr emits the `null` bucket last and unconditionally — it is not
            // subject to `facet.mincount` or `facet.limit`. Its count is the
            // number of *hits* with no value in the field, read from the fast
            // field column (`ExistsQuery`), never from stored values.
            let has_value = ExistsQuery::new(column.clone(), false);
            let absent = index.count(&narrowed(base, Occur::MustNot, Box::new(has_value)))?;
            buckets.push((None, absent as u64));
        }

        out.insert(label, render_buckets(&buckets, nl));
    }
    Ok((Value::Object(out), warnings))
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
    use super::*;

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
