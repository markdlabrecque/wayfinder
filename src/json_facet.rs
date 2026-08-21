//! `json.facet` — Solr's JSON Facet API (issue #343, PRD §5), the read path
//! `search_api_solr` uses for its `_version_` watermark.
//!
//! One request param, `json.facet`, whose value is a JSON object; one response
//! block, the top-level `facets` sibling. Structured like `crate::facet` and
//! `crate::stats`: Solr wire semantics live here, the Tantivy primitives stay
//! on `CoreIndex`, and the base query (`q` plus every `fq`) is taken as a
//! `facet::BaseClauses` rather than rebuilt — a second base-query pathway is
//! exactly how `count` and every bucket count would silently stop tracking
//! `q`/`fq`.
//!
//! Facts pinned by the committed `solr-ref/responses/jf343_*.json` fixtures
//! (findings 175-178):
//!
//! - **The aggregation wire form is a bare string** (finding 175):
//!   `{"maxVersion":"max(_version_)"}`. Solarium's `JsonAggregation::serialize()`
//!   emits the string; `function` is the PHP *option* name and never reaches
//!   the wire, and the object form `{"type":"func","func":…}` is never sent.
//! - `facets` is a top-level sibling emitted **after `facet_counts` and before
//!   `stats`** (`jf343_with_classic_stats.json`); `serde_json`'s
//!   `preserve_order` makes insertion order wire order, so the slot is chosen
//!   in `select`, not here.
//! - `facets` always carries an implicit scalar **`count`**, even for
//!   `json.facet={}` (`jf343_empty_object.json` -> `{"count":6}`), and the
//!   client reads it unguarded. It is the `numFound` of `q`+`fq`, not the whole
//!   index (`jf343_terms_q.json` count 3, `jf343_terms_fq.json` count 5).
//! - An aggregation renders as a bare scalar at its key, and over an integer
//!   column as a **raw JSON integer** (`"maxVersion":1872604773983715328`) —
//!   *not* the float the `stats` component emits for the identical column
//!   (finding 177). The two renderers therefore cannot be shared, which is why
//!   this module does not route `max()` through `stats.rs`.
//! - A terms facet renders `{"buckets":[{"val":…,"count":…}, …]}`, and a
//!   sub-facet appears **inline inside each bucket object**, as a sibling of
//!   `val`/`count` (`jf343_terms_nested.json`, `jf343_deep_max.json`).
//! - Bucket defaults: sort count-descending (ties broken by index ascending,
//!   `jf343_terms_limit.json`), `mincount` 1, `limit: -1` meaning unlimited;
//!   `"sort":"index asc"` gives lexicographic order
//!   (`jf343_terms_sort_index.json`).
//! - The error-envelope split is `facet::PreQueryFacetError`'s exactly:
//!   a parse failure (`jf343_err_bad_json`, `jf343_err_bad_type`) emits
//!   `responseHeader, error` with **no `response` block**, while a
//!   field-resolution failure (`jf343_err_unknown_field`) emits
//!   `responseHeader, response, error`.
//!
//! **Documented divergences** (finding 178), both captured so they stay
//! visible:
//!
//! - `jf343_err_no_docvalues.json` — a terms facet on a field without
//!   docValues is 200 with `{"buckets":[]}` in Solr. Wayfinder 400s, reusing
//!   `facet::check_facetable`'s own wording, consistent with finding 105's
//!   classic-facet divergence.
//! - `jf343_err_agg_text.json` — `max(body)` over a text field returns the
//!   lexicographic maximum (`"zeta"`) in Solr. Wayfinder 400s: no client path
//!   is evidenced for it, and handing a client that expects a number a string
//!   is worse than failing loudly.
//!
//! Everything outside the evidenced surface **400s rather than being silently
//! ignored** — a silently ignored facet setting produces wrong counts that
//! look right, which is strictly worse than a visible divergence. Each ceiling
//! carries a `ponytail:` comment below.

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tantivy::aggregation::Key;
use tantivy::aggregation::agg_req::{Aggregation, AggregationVariants, Aggregations};
use tantivy::aggregation::agg_result::{
    AggregationResult, AggregationResults, BucketEntry, BucketResult, MetricResult,
};
use tantivy::aggregation::metric::MaxAggregation;

use crate::core_index::CoreIndex;
use crate::facet::{self, BaseClauses, PreQueryFacetError};
use crate::params::Params;
use crate::schema::{VERSION_FIELD, ValueKind, WayfinderSchema};

/// Solr's `json.facet` `limit` default when the key is absent. The captured
/// client always sends an explicit `limit` (`-1`), so no fixture pins this.
///
/// ponytail: taken from Solr's documented default rather than from a capture.
const DEFAULT_JSON_FACET_LIMIT: i64 = 10;

/// Solr's `json.facet` `mincount` default: buckets with no matching document
/// are dropped (`jf343_terms.json` lists no zero-count bucket).
const DEFAULT_JSON_FACET_MINCOUNT: u64 = 1;

/// The keys a `type: terms` facet may carry. Anything else is refused by name
/// (see the module docs' "wrong counts that look right").
///
/// ponytail: the unevidenced Solr settings `domain`, `offset`, `numBuckets`,
/// `allBuckets`, `missing`, `prefix`, `method`, `refine`, `overrequest` and
/// `excludeTags` are all deliberately *absent* from this list, so each 400s.
/// The captured client (finding 175) sends none of them. Implement one only
/// alongside a capture that pins its semantics.
const TERMS_KEYS: &[&str] = &["type", "field", "limit", "mincount", "sort", "facet"];

// --- parse phase -------------------------------------------------------------
//
// Everything detectable without touching the schema or running a query, so
// `select` can answer with the no-`response` envelope
// (`jf343_err_bad_json`/`jf343_err_bad_type`).

/// One requested `json.facet` member, before field resolution.
#[derive(Debug, PartialEq)]
struct ParsedEntry {
    /// The response key — Solarium's `local_key`, echoed verbatim.
    key: String,
    node: ParsedNode,
}

#[derive(Debug, PartialEq)]
enum ParsedNode {
    /// `"max(<field>)"`, the sole evidenced aggregation (finding 175).
    Max {
        field: String,
    },
    Terms(ParsedTerms),
}

#[derive(Debug, PartialEq)]
struct ParsedTerms {
    field: String,
    limit: i64,
    mincount: u64,
    /// `"index asc"` instead of the count-descending default.
    by_index: bool,
    children: Vec<ParsedEntry>,
}

/// Wraps a parse-phase failure so `select` omits the `response` block, exactly
/// as `facet.range` already does (`facet::PreQueryFacetError`).
fn pre_query(e: anyhow::Error) -> anyhow::Error {
    PreQueryFacetError::wrap(e)
}

/// Parses the whole `json.facet` value. Every error here is pre-query.
fn parse_json_facet(raw: &str) -> Result<Vec<ParsedEntry>> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| anyhow!("could not parse json.facet: {e}"))
        .map_err(pre_query)?;
    let Value::Object(obj) = value else {
        return Err(pre_query(anyhow!(
            "json.facet must be a JSON object, got {value}"
        )));
    };
    parse_entries(&obj).map_err(pre_query)
}

fn parse_entries(obj: &Map<String, Value>) -> Result<Vec<ParsedEntry>> {
    let mut entries = Vec::with_capacity(obj.len());
    for (key, value) in obj {
        let node = match value {
            Value::String(s) => parse_aggregation(key, s)?,
            Value::Object(facet) => ParsedNode::Terms(parse_terms(key, facet)?),
            other => bail!(
                "json.facet member `{key}` must be a facet object or an aggregation string, got {other}"
            ),
        };
        entries.push(ParsedEntry {
            key: key.clone(),
            node,
        });
    }
    Ok(entries)
}

/// The bare-string aggregation form (finding 175). Only `max(<field>)` is
/// implemented.
///
/// ponytail: every other Solr aggregation — `min`, `sum`, `avg`, `sumsq`,
/// `unique`, `hll`, `percentile`, `missing`, `countvals` — 400s here rather
/// than being ignored, and so does the object form `{"type":"func","func":…}`
/// (refused by `parse_terms`, since its `type` is not `terms`). The captured
/// client only ever sends `max(_version_)`.
fn parse_aggregation(key: &str, raw: &str) -> Result<ParsedNode> {
    let text = raw.trim();
    let inner = text
        .strip_prefix("max(")
        .and_then(|rest| rest.strip_suffix(')'));
    match inner {
        Some(field) if !field.trim().is_empty() => Ok(ParsedNode::Max {
            field: field.trim().to_string(),
        }),
        _ => bail!(
            "unsupported json.facet aggregation for `{key}`: `{raw}` -- Wayfinder implements only max(<field>)"
        ),
    }
}

fn parse_terms(key: &str, facet: &Map<String, Value>) -> Result<ParsedTerms> {
    // `type` is checked before the unknown-key sweep so an out-of-scope facet
    // type is named in the error rather than one of its own sub-keys (e.g.
    // `type: query`'s `q`).
    // ponytail: Solr defaults a `type`-less facet object to `terms`; Wayfinder
    // requires `type` explicitly. The client always sends it (spec §1a: Solarium's
    // `JsonFacetTrait::serialize()` injects `type` unconditionally), so there is no
    // fixture for the omitted form and no evidenced default to match -- inferring
    // one here would be guessing at Solr behaviour we have not captured. The error
    // therefore names the missing key rather than claiming `terms` is unsupported.
    let Some(kind) = facet.get("type") else {
        bail!(
            "json.facet member `{key}` is missing the required `type` key: Wayfinder does not \
             infer Solr's `terms` default"
        );
    };
    match kind.as_str() {
        Some("terms") => {}
        // ponytail: `type: query`, `type: range`, `type: func` (the object
        // aggregation form) and every other Solr facet type 400 here. Solr
        // accepts them, so this is a divergence -- but accepting one and
        // ignoring it would answer with counts that are wrong and look right.
        Some(other) => bail!(
            "unsupported json.facet type `{other}` for `{key}`: Wayfinder implements only `terms`"
        ),
        None => bail!("json.facet member `{key}` has a non-string `type`: {kind}"),
    }

    for name in facet.keys() {
        if !TERMS_KEYS.contains(&name.as_str()) {
            bail!(
                "unsupported json.facet setting `{name}` on `{key}`: Wayfinder implements only {}",
                TERMS_KEYS.join(", ")
            );
        }
    }

    let field = match facet.get("field") {
        Some(Value::String(field)) if !field.is_empty() => field.clone(),
        Some(other) => {
            bail!("json.facet `field` on `{key}` must be a non-empty string, got {other}")
        }
        None => bail!("json.facet member `{key}` has no `field`"),
    };

    let limit = match facet.get("limit") {
        None => DEFAULT_JSON_FACET_LIMIT,
        Some(v) => v
            .as_i64()
            .ok_or_else(|| anyhow!("json.facet `limit` on `{key}` must be an integer, got {v}"))?,
    };
    let mincount = match facet.get("mincount") {
        None => DEFAULT_JSON_FACET_MINCOUNT,
        Some(v) => v.as_u64().ok_or_else(|| {
            anyhow!("json.facet `mincount` on `{key}` must be a non-negative integer, got {v}")
        })?,
    };
    // ponytail: only the two evidenced spellings are accepted -- the
    // count-descending default and `"index asc"`
    // (`jf343_terms_sort_index.json`). Solr also understands `count asc`,
    // `index desc` and sorting by an aggregation's value; none is captured, so
    // each 400s rather than silently sorting some other way.
    let by_index = match facet.get("sort") {
        None => false,
        Some(Value::String(s)) if s.trim() == "index asc" => true,
        Some(Value::String(s)) if s.trim() == "count desc" => false,
        Some(other) => bail!(
            "unsupported json.facet `sort` on `{key}`: {other} -- Wayfinder implements `count desc` (the default) and `index asc`"
        ),
    };

    let children = match facet.get("facet") {
        None => Vec::new(),
        Some(Value::Object(sub)) => parse_entries(sub)?,
        Some(other) => bail!("json.facet `facet` on `{key}` must be a JSON object, got {other}"),
    };

    Ok(ParsedTerms {
        field,
        limit,
        mincount,
        by_index,
        children,
    })
}

// --- resolve phase -----------------------------------------------------------
//
// Schema-dependent, so its errors are *not* pre-query: they carry the base
// query's `response` block (`jf343_err_unknown_field`).

/// One resolved `json.facet` member: the Tantivy column, its rendering kind,
/// and the name its aggregation carries inside the request tree.
#[derive(Debug)]
struct PlanEntry {
    key: String,
    node: PlanNode,
}

#[derive(Debug)]
enum PlanNode {
    Max {
        agg_name: String,
        /// Whether the column is an integer one, i.e. whether the result
        /// renders as a raw JSON integer (finding 177).
        integral: bool,
    },
    Terms {
        agg_name: String,
        limit: i64,
        mincount: u64,
        by_index: bool,
        children: Vec<PlanEntry>,
    },
}

/// The Tantivy column and rendering kind behind `max(<field>)`.
///
/// `_version_` is the reason this exists at all: it is deliberately absent
/// from `WayfinderSchema::field_handles` (`src/schema.rs`), so
/// `field_config`, `value_kind` and `resolved_fast_column` all miss it, and the
/// captured client's only `json.facet` aggregation is over exactly that field
/// (finding 132). This mirrors `stats::check_statable`'s own `VERSION_FIELD`
/// exception rather than making `_version_` a generally schema-resolved field —
/// `tests/version_field.rs` pins that faceting and sorting on it stay 400s.
fn resolve_aggregation_column(
    schema: &WayfinderSchema,
    field_name: &str,
) -> Result<(String, bool)> {
    if field_name == VERSION_FIELD {
        // An i64 fast column, hence integral: `max(_version_)` renders as a
        // raw integer, which is the whole point of finding 177.
        return Ok((VERSION_FIELD.to_string(), true));
    }
    match schema.resolved_fast(field_name) {
        None => bail!("can not facet on undefined field: {field_name}"),
        Some(false) => {
            bail!("can not aggregate on a field w/o fast values (docValues): {field_name}")
        }
        Some(true) => {}
    }
    let column = schema
        .resolved_fast_column(field_name)
        .expect("resolved_fast proved this field resolves");
    // ponytail: `max()` is numeric only. A **text** column is refused rather
    // than answered with Solr's lexicographic maximum
    // (`jf343_err_agg_text.json`, finding 178), and a **date** column is
    // refused because no capture pins whether Solr renders that maximum as an
    // RFC3339 string or as a raw millisecond long -- guessing would be an
    // invisible divergence. Capture one before implementing it.
    match schema.resolved_value_kind(field_name) {
        Some(ValueKind::I64) => Ok((column, true)),
        Some(ValueKind::F64) => Ok((column, false)),
        Some(kind) => bail!(
            "can not compute max() on field `{field_name}`: json.facet aggregation needs a numeric field, not {kind:?}"
        ),
        None => bail!("can not compute max() on field `{field_name}`: unknown field type"),
    }
}

/// Turns the parsed tree into a plan plus the single nested Tantivy
/// aggregation request that computes all of it. `next` numbers the
/// aggregations by position rather than by response key, so two members naming
/// the same column under different keys cannot share one bucket list — the
/// same reasoning as the classic facet planner's `wf_facet_{i}`.
fn resolve_entries(
    schema: &WayfinderSchema,
    parsed: &[ParsedEntry],
    next: &mut usize,
    terms_count: &mut usize,
) -> Result<(Vec<PlanEntry>, Aggregations)> {
    let mut plan = Vec::with_capacity(parsed.len());
    let mut aggs = Aggregations::default();
    for entry in parsed {
        let agg_name = format!("wf_jf_{}", *next);
        *next += 1;
        match &entry.node {
            ParsedNode::Max { field } => {
                let (column, integral) = resolve_aggregation_column(schema, field)?;
                aggs.insert(
                    agg_name.clone(),
                    Aggregation {
                        agg: AggregationVariants::Max(MaxAggregation::from_field_name(column)),
                        sub_aggregation: Aggregations::default(),
                    },
                );
                plan.push(PlanEntry {
                    key: entry.key.clone(),
                    node: PlanNode::Max { agg_name, integral },
                });
            }
            ParsedNode::Terms(terms) => {
                // The same facetability contract classic `facet.field` uses,
                // including its exact "fast values (docValues)" wording — the
                // divergence in finding 105 and the one in finding 178 are the
                // same refusal for the same reason, so they share one check.
                facet::check_facetable(schema, &terms.field, true)?;
                let column = schema
                    .resolved_fast_column(&terms.field)
                    .expect("check_facetable proved this field resolves");
                *terms_count += 1;
                let (children, sub_aggregation) =
                    resolve_entries(schema, &terms.children, next, terms_count)?;
                let mut agg = crate::core_index::terms_aggregation(&column);
                // The whole nesting design: a child facet or aggregation is a
                // Tantivy *sub-aggregation* of its parent bucket, so its
                // domain is that bucket's documents and nothing else. This is
                // what makes `entity:node` under `index_a` report 30 in
                // `jf343_deep_max.json` rather than the global maximum 60.
                agg.sub_aggregation = sub_aggregation;
                aggs.insert(agg_name.clone(), agg);
                plan.push(PlanEntry {
                    key: entry.key.clone(),
                    node: PlanNode::Terms {
                        agg_name,
                        limit: terms.limit,
                        mincount: terms.mincount,
                        by_index: terms.by_index,
                        children,
                    },
                });
            }
        }
    }
    Ok((plan, aggs))
}

// --- render phase ------------------------------------------------------------

/// Renders one aggregation's scalar result. An integer column renders as a raw
/// JSON integer, *not* as the float the `stats` component emits for the very
/// same column (finding 177) — the two paths cannot share a renderer.
///
/// Tantivy's metric aggregations accumulate in `f64`, so an integral result is
/// exact up to 2^53. Wayfinder's `_version_` is seeded from epoch
/// milliseconds (~1.7e12), five orders of magnitude below that ceiling.
///
/// ponytail: an aggregation with no value (an empty domain) renders `null`.
/// No fixture captures that case, and Solr may omit the key instead; `null` is
/// the honest "no value" and keeps the key the client reads present.
fn render_max(value: Option<f64>, integral: bool) -> Value {
    match value {
        None => Value::Null,
        Some(v) if integral => json!(v as i64),
        Some(v) => json!(v),
    }
}

/// A terms bucket's `val`. Solr echoes the term in the column's own JSON type.
///
/// ponytail: only string columns are fixture-pinned
/// (`hash`/`index_id`/`ss_search_api_datasource` are all strings). A numeric
/// column's `val` renders as a JSON number here, which matches Solr's
/// documented behaviour but is not captured.
fn render_bucket_val(key: &Key) -> Value {
    match key {
        Key::Str(s) => json!(s),
        Key::I64(v) => json!(v),
        Key::U64(v) => json!(v),
        Key::F64(v) => json!(v),
    }
}

/// Total ordering on a bucket key, so `index asc` and the count-descending
/// tie-break are both deterministic. `jf343_terms_limit.json` is the fixture
/// that needs the tie-break: `index_b` and `index_c` both have count 1, and
/// `limit: 2` keeps `index_b`.
fn key_cmp(a: &Key, b: &Key) -> std::cmp::Ordering {
    match (a, b) {
        (Key::Str(a), Key::Str(b)) => a.cmp(b),
        _ => key_as_f64(a).total_cmp(&key_as_f64(b)),
    }
}

fn key_as_f64(key: &Key) -> f64 {
    match key {
        Key::Str(_) => f64::NAN,
        Key::I64(v) => *v as f64,
        Key::U64(v) => *v as f64,
        Key::F64(v) => *v,
    }
}

fn render_entries(
    entries: &[PlanEntry],
    results: &AggregationResults,
) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for entry in entries {
        let value = match &entry.node {
            PlanNode::Max { agg_name, integral } => {
                let Some(AggregationResult::MetricResult(MetricResult::Max(max))) =
                    results.0.get(agg_name)
                else {
                    return Err(anyhow!(
                        "could not compute json.facet `{}`: unexpected aggregation result",
                        entry.key
                    ));
                };
                render_max(max.value, *integral)
            }
            PlanNode::Terms {
                agg_name,
                limit,
                mincount,
                by_index,
                children,
            } => {
                let Some(AggregationResult::BucketResult(BucketResult::Terms { buckets, .. })) =
                    results.0.get(agg_name)
                else {
                    return Err(anyhow!(
                        "could not compute json.facet `{}`: unexpected aggregation result",
                        entry.key
                    ));
                };
                let mut kept: Vec<&BucketEntry> = buckets
                    .iter()
                    .filter(|bucket| bucket.doc_count >= *mincount)
                    .collect();
                if *by_index {
                    kept.sort_by(|a, b| key_cmp(&a.key, &b.key));
                } else {
                    kept.sort_by(|a, b| {
                        b.doc_count
                            .cmp(&a.doc_count)
                            .then_with(|| key_cmp(&a.key, &b.key))
                    });
                }
                // ponytail: `limit: -1` is unlimited only up to
                // `core_index::terms_aggregation`'s own `MAX_FACET_TERMS`
                // dictionary ceiling, the same ceiling classic `facet.field`
                // already carries.
                if *limit >= 0 {
                    kept.truncate(*limit as usize);
                }
                let mut rendered = Vec::with_capacity(kept.len());
                for bucket in kept {
                    let mut obj = Map::new();
                    obj.insert("val".to_string(), render_bucket_val(&bucket.key));
                    obj.insert("count".to_string(), json!(bucket.doc_count));
                    // Sub-facets render inline, as further siblings of
                    // `val`/`count` (`jf343_terms_nested.json`).
                    for (key, value) in render_entries(children, &bucket.sub_aggregation)? {
                        obj.insert(key, value);
                    }
                    rendered.push(Value::Object(obj));
                }
                json!({ "buckets": rendered })
            }
        };
        out.insert(entry.key.clone(), value);
    }
    Ok(out)
}

// --- entry point -------------------------------------------------------------

/// Builds the whole `facets` block, or `Ok(None)` when `json.facet` was not
/// requested at all — an absent param leaves the key out of the envelope
/// entirely, never present-and-empty, the same contract `facet=true` has.
///
/// `base` is `q` plus every `fq` (and `group.truncate`'s restriction, for
/// free): the implicit `count` and every bucket count are computed against it.
pub fn json_facets(
    index: &CoreIndex,
    params: &Params,
    base: &BaseClauses,
) -> Result<Option<Value>> {
    let Some(raw) = params.get("json.facet") else {
        return Ok(None);
    };

    let parsed = parse_json_facet(raw)?;

    let mut next = 0usize;
    let mut terms_count = 0usize;
    let (plan, aggs) = resolve_entries(&index.wf_schema, &parsed, &mut next, &mut terms_count)?;

    let base_query = facet::base_query(base);
    // The implicit `count` the client reads unguarded: the `numFound` of
    // `q`+`fq`, present even for `json.facet={}` (`jf343_empty_object.json`),
    // and first in the object.
    let mut out = Map::new();
    out.insert("count".to_string(), json!(index.count(&base_query)?));

    if !plan.is_empty() {
        let results = index.run_aggregations(aggs, &base_query, terms_count)?;
        for (key, value) in render_entries(&plan, &results)? {
            out.insert(key, value);
        }
    }
    Ok(Some(Value::Object(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<Vec<ParsedEntry>> {
        parse_json_facet(raw)
    }

    fn is_pre_query(e: &anyhow::Error) -> bool {
        e.downcast_ref::<PreQueryFacetError>().is_some()
    }

    // --- parse phase ---------------------------------------------------

    #[test]
    fn bare_string_is_the_aggregation_form() {
        let parsed = parse(r#"{"maxVersion":"max(_version_)"}"#).expect("must parse");
        assert_eq!(
            parsed,
            vec![ParsedEntry {
                key: "maxVersion".to_string(),
                node: ParsedNode::Max {
                    field: "_version_".to_string()
                },
            }]
        );
    }

    #[test]
    fn object_aggregation_form_is_refused() {
        // Finding 165: Solarium never sends this; accepting it would be
        // building to the ticket's PHP option name instead of the wire.
        let err = parse(r#"{"x":{"type":"func","func":"max(popularity)"}}"#)
            .expect_err("the object form must be refused");
        assert!(err.to_string().contains("func"), "got {err}");
    }

    #[test]
    fn every_parse_failure_is_pre_query() {
        for raw in [
            r#"{"siteHashes":{"field":"#,
            r#"{"siteHashes":{"type":"nosuchtype","field":"hash"}}"#,
            r#"{"x":{"type":"terms","field":"hash","domain":{}}}"#,
            r#"{"x":"sum(popularity)"}"#,
            r#"[]"#,
        ] {
            let err = parse(raw).expect_err("must fail");
            assert!(
                is_pre_query(&err),
                "`{raw}` must be a PreQueryFacetError so select omits the response block, got {err}"
            );
        }
    }

    #[test]
    fn unevidenced_settings_are_named_in_the_error() {
        for setting in [
            "domain",
            "offset",
            "numBuckets",
            "allBuckets",
            "missing",
            "prefix",
            "method",
            "refine",
            "overrequest",
            "excludeTags",
        ] {
            let raw = format!(r#"{{"x":{{"type":"terms","field":"hash","{setting}":1}}}}"#);
            let err = parse(&raw).expect_err("must fail");
            assert!(
                err.to_string().contains(setting),
                "the 400 for `{setting}` must name it, got {err}"
            );
        }
    }

    #[test]
    fn terms_defaults_are_solrs() {
        let parsed = parse(r#"{"x":{"type":"terms","field":"hash"}}"#).expect("must parse");
        let ParsedNode::Terms(terms) = &parsed[0].node else {
            panic!("must be a terms facet");
        };
        assert_eq!(terms.limit, DEFAULT_JSON_FACET_LIMIT);
        assert_eq!(terms.mincount, DEFAULT_JSON_FACET_MINCOUNT);
        assert!(!terms.by_index, "the default sort is count desc");
    }

    #[test]
    fn sort_index_asc_parses_and_other_sorts_do_not() {
        let parsed = parse(r#"{"x":{"type":"terms","field":"hash","sort":"index asc"}}"#)
            .expect("must parse");
        let ParsedNode::Terms(terms) = &parsed[0].node else {
            panic!("must be a terms facet");
        };
        assert!(terms.by_index);
        assert!(
            parse(r#"{"x":{"type":"terms","field":"hash","sort":"count asc"}}"#).is_err(),
            "an unevidenced sort must 400 rather than silently sort some other way"
        );
    }

    #[test]
    fn request_order_is_preserved() {
        let parsed =
            parse(r#"{"zzz":"max(popularity)","aaa":"max(popularity)"}"#).expect("must parse");
        let keys: Vec<&str> = parsed.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["zzz", "aaa"],
            "serde_json's preserve_order must carry request order into the response"
        );
    }

    // --- `_version_`-aware column resolution ---------------------------
    //
    // Stage 1 flagged this specifically: `_version_` is deliberately absent
    // from `field_handles`, so a resolver built on `field_config` /
    // `value_kind` / `resolved_fast_column` alone silently misses it.

    fn schema() -> WayfinderSchema {
        let toml = r#"
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
name = "popularity"
type = "int"
stored = true
fast = true

[[fields]]
name = "score_f"
type = "double"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true
"#;
        crate::schema::parse(toml).expect("schema must parse")
    }

    #[test]
    fn version_field_resolves_even_though_it_has_no_schema_handle() {
        let schema = schema();
        assert!(
            schema.field_config(VERSION_FIELD).is_none(),
            "premise: _version_ is deliberately absent from the schema's field handles"
        );
        assert!(
            schema.resolved_fast_column(VERSION_FIELD).is_none(),
            "premise: the normal fast-column resolver misses _version_"
        );
        assert_eq!(
            resolve_aggregation_column(&schema, VERSION_FIELD).expect("must resolve"),
            (VERSION_FIELD.to_string(), true),
            "max(_version_) must resolve to the _version_ column and render as an integer"
        );
    }

    #[test]
    fn integer_and_float_columns_render_differently() {
        let schema = schema();
        assert_eq!(
            resolve_aggregation_column(&schema, "popularity").expect("must resolve"),
            ("popularity".to_string(), true)
        );
        assert_eq!(
            resolve_aggregation_column(&schema, "score_f").expect("must resolve"),
            ("score_f".to_string(), false)
        );
        assert_eq!(render_max(Some(60.0), true), json!(60));
        assert_eq!(render_max(Some(60.0), false), json!(60.0));
        assert_ne!(
            render_max(Some(60.0), true),
            render_max(Some(60.0), false),
            "finding 177: the integer form is not the float form the stats component emits"
        );
    }

    #[test]
    fn max_refuses_columns_it_cannot_render() {
        let schema = schema();
        // `jf343_err_agg_text.json`: Solr answers "zeta"; Wayfinder 400s.
        assert!(resolve_aggregation_column(&schema, "body").is_err());
        // A fast *string* column passes the docValues check but is still text.
        assert!(resolve_aggregation_column(&schema, "id").is_err());
        // Unevidenced rendering, so refused rather than guessed.
        assert!(resolve_aggregation_column(&schema, "created").is_err());
        let err = resolve_aggregation_column(&schema, "no_such_field")
            .expect_err("an undefined field must fail");
        assert!(err.to_string().contains("no_such_field"), "got {err}");
    }

    #[test]
    fn version_max_stays_exact_at_epoch_millis_scale() {
        // Wayfinder seeds `_version_` from epoch milliseconds; Tantivy's
        // metric aggregations accumulate in f64. This pins that the round
        // trip is lossless at that scale (the 2^53 ceiling is ~5 orders of
        // magnitude away).
        let version: i64 = 1_754_300_000_123;
        assert_eq!(render_max(Some(version as f64), true), json!(version));
    }

    // --- sub-aggregation scoping ---------------------------------------
    //
    // The other thing stage 1 flagged. `jf343_deep_max.json`'s
    // `entity:node`-under-`index_a` leaf is 30, not the global 60: that is
    // only true if each child is a Tantivy *sub*-aggregation of its parent
    // bucket rather than a second top-level one.

    #[test]
    fn children_become_sub_aggregations_of_their_parent_bucket() {
        let schema = schema();
        let parsed = parse(
            r#"{"top":{"type":"terms","field":"id","facet":{"inner":{"type":"terms","field":"id","facet":{"leaf":"max(popularity)"}}}}}"#,
        )
        .expect("must parse");
        let mut next = 0;
        let mut terms_count = 0;
        let (plan, aggs) =
            resolve_entries(&schema, &parsed, &mut next, &mut terms_count).expect("must resolve");

        assert_eq!(
            aggs.len(),
            1,
            "only the outermost facet may be a top-level aggregation -- a child \
             promoted to the top level would be scoped to the whole result set"
        );
        assert_eq!(terms_count, 2, "both terms levels must be counted");

        let outer = aggs.values().next().expect("one top-level aggregation");
        assert_eq!(outer.sub_aggregation.len(), 1, "`inner` must nest under it");
        let inner = outer
            .sub_aggregation
            .values()
            .next()
            .expect("one sub-aggregation");
        assert_eq!(
            inner.sub_aggregation.len(),
            1,
            "`leaf` must nest under `inner`, not under the outer facet or the root"
        );
        assert!(
            matches!(
                inner.sub_aggregation.values().next().map(|a| &a.agg),
                Some(AggregationVariants::Max(_))
            ),
            "the leaf must be a real per-bucket MAX aggregation"
        );

        // Every aggregation in the tree gets its own positional name, so two
        // members over the same column cannot share a bucket list.
        let PlanNode::Terms {
            agg_name, children, ..
        } = &plan[0].node
        else {
            panic!("outer must be a terms facet");
        };
        assert_eq!(agg_name, "wf_jf_0");
        let PlanNode::Terms {
            agg_name, children, ..
        } = &children[0].node
        else {
            panic!("inner must be a terms facet");
        };
        assert_eq!(agg_name, "wf_jf_1");
        let PlanNode::Max { agg_name, .. } = &children[0].node else {
            panic!("leaf must be an aggregation");
        };
        assert_eq!(agg_name, "wf_jf_2");
    }

    #[test]
    fn sibling_members_over_one_column_get_distinct_aggregation_names() {
        let schema = schema();
        let parsed = parse(r#"{"a":"max(popularity)","b":"max(popularity)"}"#).expect("must parse");
        let mut next = 0;
        let mut terms_count = 0;
        let (plan, aggs) =
            resolve_entries(&schema, &parsed, &mut next, &mut terms_count).expect("must resolve");
        assert_eq!(aggs.len(), 2, "two members must not collapse into one");
        let names: Vec<&str> = plan
            .iter()
            .map(|entry| match &entry.node {
                PlanNode::Max { agg_name, .. } => agg_name.as_str(),
                PlanNode::Terms { agg_name, .. } => agg_name.as_str(),
            })
            .collect();
        assert_eq!(names, vec!["wf_jf_0", "wf_jf_1"]);
    }

    // --- bucket shaping -----------------------------------------------

    #[test]
    fn count_desc_breaks_ties_on_index_asc() {
        // `jf343_terms_limit.json`: index_a:3, index_b:1, index_c:1 at
        // `limit: 2` keeps index_a and index_b, so the count:1 tie resolves
        // alphabetically rather than arbitrarily.
        let mut keys = [
            (Key::Str("index_c".to_string()), 1u64),
            (Key::Str("index_a".to_string()), 3),
            (Key::Str("index_b".to_string()), 1),
        ];
        keys.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| key_cmp(&a.0, &b.0)));
        assert_eq!(
            keys.iter()
                .map(|(k, _)| render_bucket_val(k))
                .collect::<Vec<_>>(),
            vec![json!("index_a"), json!("index_b"), json!("index_c")]
        );
    }
}
