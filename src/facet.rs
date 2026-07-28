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
//! **Documented divergence** (findings 16, narrowed by issue #26): real Solr
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

use std::ops::Bound;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tantivy::query::{BooleanQuery, ExistsQuery, Occur, Query, RangeQuery};
use tantivy::time::format_description::well_known::Rfc3339;
use tantivy::time::{Duration, OffsetDateTime};
use tantivy::{DateTime, Term};

use crate::config::ServerConfig;
use crate::core_index::CoreIndex;
use crate::params::Params;
use crate::schema::{ValueKind, WayfinderSchema};

/// Solr's `facet.limit` default.
const DEFAULT_FACET_LIMIT: i64 = 100;

/// Ceiling on the number of `facet.range` buckets one request may ask for, so a
/// tiny `facet.range.gap` over a huge span cannot spin the server.
const MAX_RANGE_BUCKETS: usize = 65_536;

/// The base query a facet is computed against: `q` and every `fq`, as `Must`
/// clauses ready to be extended per facet.
pub type BaseClauses = Vec<(Occur, Box<dyn Query>)>;

/// Builds the whole `facet_counts` block. Every error is caused by the request
/// (an unfacetable field, an unparseable `facet.query`, a bad range spec), so
/// the caller renders them all as 400s.
pub fn facet_counts(
    index: &CoreIndex,
    config: &ServerConfig,
    params: &Params,
    default_field: &str,
    base: &BaseClauses,
) -> Result<Value> {
    let as_map = params.get("json.nl") == Some("map");

    Ok(json!({
        "facet_queries": facet_queries(index, params, default_field, base)?,
        "facet_fields": facet_fields(index, config, params, base, as_map)?,
        "facet_ranges": facet_ranges(index, params, base, as_map)?,
        // Out of scope (PRD §5 leaves them for later): the keys are present
        // and empty because Solr always emits all five (findings fact 3).
        "facet_intervals": {},
        "facet_heatmaps": {},
    }))
}

/// Clones `base` and adds `extra` as another `Must` clause.
fn narrowed(base: &BaseClauses, occur: Occur, extra: Box<dyn Query>) -> BooleanQuery {
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

/// `facet.field`, repeatable — one key per field, each counted independently
/// (`facet_multi_field.json`).
fn facet_fields(
    index: &CoreIndex,
    config: &ServerConfig,
    params: &Params,
    base: &BaseClauses,
    as_map: bool,
) -> Result<Value> {
    let fields = params.get_all("facet.field");
    if fields.is_empty() {
        return Ok(json!({}));
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
    let missing = params.get("facet.missing") == Some("true");

    let base_query = BooleanQuery::from(
        base.iter()
            .map(|(occur, query)| (*occur, query.box_clone()))
            .collect::<BaseClauses>(),
    );

    let mut out = Map::new();
    for field_name in fields {
        check_facetable(&index.wf_schema, field_name)?;

        let mut counts = index.term_facet(field_name, &base_query)?;
        counts.retain(|(_, count)| *count >= mincount);
        if by_index {
            counts.sort_by(|a, b| a.0.cmp(&b.0));
        } else {
            counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }
        counts.truncate(limit);

        let mut buckets: Vec<(Option<String>, u64)> = counts
            .into_iter()
            .map(|(term, count)| (Some(term), count))
            .collect();
        if missing {
            // Solr emits the `null` bucket last and unconditionally — it is not
            // subject to `facet.mincount` or `facet.limit`. Its count is the
            // number of *hits* with no value in the field, read from the fast
            // field column (`ExistsQuery`), never from stored values.
            let has_value = ExistsQuery::new(field_name.to_string(), false);
            let absent = index.count(&narrowed(base, Occur::MustNot, Box::new(has_value)))?;
            buckets.push((None, absent as u64));
        }

        out.insert(field_name.to_string(), render_buckets(&buckets, as_map));
    }
    Ok(Value::Object(out))
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
    as_map: bool,
) -> Result<Value> {
    let fields = params.get_all("facet.range");
    if fields.is_empty() {
        return Ok(json!({}));
    }

    let mut out = Map::new();
    for field_name in fields {
        check_facetable(&index.wf_schema, field_name)?;
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

        let mut buckets = Vec::new();
        for (key, lower, upper) in range_buckets(field_name, kind, start, end, gap)? {
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
                "counts": render_buckets(&buckets, as_map),
                "gap": echo_bound(kind, gap),
                "start": echo_bound(kind, start),
                "end": echo_bound(kind, end),
            }),
        );
    }
    Ok(Value::Object(out))
}

/// One end of a range-facet bucket, in the field's own type so the range query
/// gets an exact `Term` rather than a lossy `f64`.
#[derive(Clone, Copy)]
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

/// Renders a bucket list either as Solr's flat alternating array (default) or
/// as an object under `json.nl=map` (findings fact 1). `None` is the
/// `facet.missing` bucket's literal `null` key.
fn render_buckets(buckets: &[(Option<String>, u64)], as_map: bool) -> Value {
    if as_map {
        let mut map = Map::new();
        for (term, count) in buckets {
            // ponytail: `json.nl=map` plus `facet.missing` has no fixture — a
            // JSON object cannot have a `null` key, and this renders it as the
            // empty string. Capture it before relying on it.
            let key = term.clone().unwrap_or_default();
            map.insert(key, json!(count));
        }
        return Value::Object(map);
    }
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

/// Refuses a facet Tantivy cannot compute, rather than returning empty counts.
///
/// This is the whole point of the issue: aggregation needs a fast (docValues)
/// column, and without one the only honest answers are an error or a lie.
/// Deliberate divergence from Solr, which answers 200 with an empty array for
/// all three of these cases — see the module docs and findings 16.
fn check_facetable(schema: &WayfinderSchema, field_name: &str) -> Result<()> {
    match schema.field_config(field_name) {
        None => bail!("can not facet on undefined field: {field_name}"),
        Some(field) if !field.fast => {
            bail!("can not facet on a field w/o fast values (docValues): {field_name}")
        }
        Some(_) => Ok(()),
    }
}
