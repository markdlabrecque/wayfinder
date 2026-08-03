//! Solr result grouping (issue #290, PRD §5 v3): `group=true` +
//! `group.field` (repeatable) -> the
//! `grouped: {<field>: {matches, ngroups, groups: [{groupValue, doclist}]}}`
//! response envelope.
//!
//! Tantivy has no native grouping collector, so the collection + bucketing
//! lives in [`crate::collector::GroupingCollector`] (reusing the sort
//! machinery there). This module owns the request half: parsing the `group.*`
//! params, validating each `group.field` (single-valued, fast, non-text -- the
//! module refuses to group on a fulltext or multiValued field, finding 130,
//! and so does Solr itself), and shaping the per-field envelope, including
//! the `group.limit`/`group.offset` (within-group) and `rows`/`start`
//! (group-list) paging Solr applies.
//!
//! `group.format` and `group.main` are never sent by the module (finding 130)
//! and are deliberately NOT in `SELECT_PARAMS`: they 400 under `strict_params`
//! rather than being silently accepted as a param Wayfinder does not
//! implement.
//!
//! Ground truth: every shape decision here is pinned by a fixture in
//! `solr-ref/responses/group_*.json`, captured against a dedicated `grouping`
//! Solr core (`solr-ref/capture.sh`'s issue-#290 block).

use serde_json::{Map, Value, json};
use tantivy::Score;
use tantivy::query::Query;

use crate::collector::{GroupingFruit, SortClause, SortKey, SortValue};
use crate::core_index::CoreIndex;
use crate::error::WfError;
use crate::params::Params;
use crate::schema::WayfinderSchema;

/// The already-parsed `q` plus its filter queries, borrowed. `None` when the
/// request has no `q` (matches nothing). Named so the handler call site reads
/// cleanly and clippy's `type_complexity` stays quiet.
type ParsedQuery<'a> = Option<(&'a dyn Query, &'a [Box<dyn Query>])>;

/// Builds the `grouped` response object when `group=true`, else `None`.
///
/// `parsed` is the already-parsed `q` plus its filter queries (`None` when the
/// request has no `q`, which matches nothing). `main_sort` is the request's
/// `sort` (drives group ordering); `rows`/`start` paginate the *groups* list;
/// `fl`/`wants_score` are the same `fl` select already resolves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn grouping(
    index: &CoreIndex,
    params: &Params,
    parsed: ParsedQuery,
    main_sort: &[SortClause],
    rows: usize,
    start: usize,
    fl: Option<&[String]>,
    wants_score: bool,
) -> Result<Option<Value>, WfError> {
    // `group=true` gates the whole component, the same way `facet=true` gates
    // `facet_counts` (finding 4). Only literal `true` enables it.
    if !params.bool_or("group", false)? {
        return Ok(None);
    }

    // `group.field` is repeatable; every value groups the same match set
    // independently, producing one keyed block per field in request order.
    let fields: Vec<String> = params
        .get_all("group.field")
        .into_iter()
        .map(str::to_string)
        .collect();
    if fields.is_empty() {
        // Solr: "Specify at least one field, function or query to group by."
        return Err(WfError::bad_request(
            "wayfinder::GroupingError",
            "Specify at least one field, function or query to group by.".to_string(),
        )
        .with_params(params));
    }

    // Within-group paging. `group.limit` defaults to 1 -- which is exactly why
    // the module omits it unless set & != 1 (finding 130): Solr's default is 1.
    let limit: usize = params
        .get("group.limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let offset: usize = params
        .get("group.offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // The module sends `group.ngroups=true` unconditionally (finding 130);
    // absent, the `ngroups` key is absent too (not zero).
    let ngroups = params.bool_or("group.ngroups", false)?;
    // `group.sort` orders docs WITHIN a group; it defaults to the main `sort`.
    // Group ORDER is always the main sort (of each group's top doc), never
    // `group.sort` -- pinned by `group_sort`, whose group order is unchanged
    // from `group_basic` even though within-group order is `id desc`.
    let within_sort = match params.get("group.sort") {
        Some(spec) => crate::parse_sort_spec(&index.wf_schema, params, spec)?,
        None => main_sort.to_vec(),
    };

    let schema = &index.wf_schema;
    let mut grouped = Map::new();
    for field in &fields {
        let group_clause = validate_group_field(schema, params, field)?;
        // No `q` matches nothing, so every group is empty -- same empty-fruit
        // shortcut `select` takes for the ungrouped `response` block.
        let fruit = match parsed {
            None => GroupingFruit {
                matches: 0,
                groups: Vec::new(),
            },
            Some((query, fqs)) => index
                .search_grouping(
                    query,
                    fqs,
                    main_sort.to_vec(),
                    within_sort.clone(),
                    group_clause,
                )
                .map_err(|e| {
                    WfError::internal("wayfinder::GroupingError", e.to_string()).with_params(params)
                })?,
        };
        let block = build_group_block(
            index,
            params,
            &fruit,
            limit,
            offset,
            rows,
            start,
            ngroups,
            wants_score,
            fl,
        )?;
        grouped.insert(field.clone(), block);
    }
    Ok(Some(Value::Object(grouped)))
}

/// Validates one `group.field` and returns the `SortClause` that reads its
/// fast-field value. Three 400s, all mirroring Solr:
/// - undefined field (`undefined field: "x"`);
/// - not a fast/docValues field;
/// - multiValued (`can not use FieldCache on multivalued field: x`).
///
/// A text field is not `fast`, so it falls through to the not-fast 400 -- the
/// module's "refuses to group on a fulltext field" rule (finding 130) is
/// enforced by the schema, not a separate text check.
fn validate_group_field(
    schema: &WayfinderSchema,
    params: &Params,
    field: &str,
) -> Result<SortClause, WfError> {
    match schema.resolved_fast(field) {
        None => {
            return Err(WfError::bad_request(
                "wayfinder::GroupingError",
                format!("undefined field: \"{field}\""),
            )
            .with_params(params));
        }
        Some(false) => {
            return Err(WfError::bad_request(
                "wayfinder::GroupingError",
                format!("can not group on a field without fast values (docValues): {field}"),
            )
            .with_params(params));
        }
        Some(true) => {}
    }
    if resolved_multi_valued(schema, field) {
        return Err(WfError::bad_request(
            "wayfinder::GroupingError",
            format!("can not use FieldCache on multivalued field: {field}"),
        )
        .with_params(params));
    }
    let column = schema
        .resolved_fast_column(field)
        .expect("resolved_fast confirmed this name resolves");
    let value_kind = schema.resolved_value_kind(field);
    // `descending` is irrelevant for the group clause: the collector reads a
    // single-valued field's one value, where min == max.
    Ok(SortClause::new(SortKey::Field(column), false, value_kind))
}

/// Whether `name` is multiValued, resolved with the same static-before-dynamic
/// precedence `resolved_fast` uses. Mirrors `WayfinderSchema::resolved_fast`'s
/// shape; grouping needs it to reject a multiValued `group.field` the way Solr
/// does, and there is no shared helper for it yet.
fn resolved_multi_valued(schema: &WayfinderSchema, name: &str) -> bool {
    if let Some(fc) = schema.field_config(name) {
        return fc.multi_valued;
    }
    schema
        .match_dynamic(name)
        .map(|rule| rule.multi_valued)
        .unwrap_or(false)
}

/// Shapes one `grouped.<field>` block from the collector's fruit, applying
/// within-group paging (`limit`/`offset`) and group-list paging
/// (`rows`/`start`).
#[allow(clippy::too_many_arguments)]
fn build_group_block(
    index: &CoreIndex,
    params: &Params,
    fruit: &GroupingFruit,
    limit: usize,
    offset: usize,
    rows: usize,
    start: usize,
    ngroups: bool,
    wants_score: bool,
    fl: Option<&[String]>,
) -> Result<Value, WfError> {
    // Key order matches the fixtures: `matches`, `ngroups`, `groups`.
    let mut block = Map::new();
    block.insert("matches".to_string(), json!(fruit.matches));
    if ngroups {
        block.insert("ngroups".to_string(), json!(fruit.groups.len()));
    }

    // `rows`/`start` paginate the GROUPS list (not docs): `rows=2&start=1`
    // drops the first group and returns the next `rows` (`group_rows_start`).
    let mut groups_arr = Vec::new();
    for g in fruit.groups.iter().skip(start).take(rows) {
        let mut group_obj = Map::new();
        group_obj.insert("groupValue".to_string(), group_value_json(&g.value));

        let mut doclist = Map::new();
        doclist.insert("numFound".to_string(), json!(g.num_found));
        // `doclist.start` mirrors `group.offset`, not the group-list `start`.
        doclist.insert("start".to_string(), json!(offset));

        // `maxScore` appears only when `fl` includes `score` (finding: the
        // doclist mirrors `response`'s own maxScore rule), and is the max
        // across the WHOLE group (every doc, not just the paged page) -- the
        // same unpaginated-global semantics `response.maxScore` already has.
        if wants_score {
            let max = g
                .docs
                .iter()
                .map(|(s, _)| *s)
                .fold(None::<Score>, |acc, s| {
                    Some(acc.map_or(s, |a: Score| a.max(s)))
                });
            if let Some(max) = max {
                doclist.insert("maxScore".to_string(), json!(max));
            }
        }
        doclist.insert("numFoundExact".to_string(), json!(true));

        let mut docs = Vec::new();
        for (score, addr) in g.docs.iter().skip(offset).take(limit) {
            docs.push(index.render_doc(*addr, fl, Some(*score)).map_err(|e| {
                WfError::internal("wayfinder::DocError", e.to_string()).with_params(params)
            })?);
        }
        doclist.insert("docs".to_string(), json!(docs));

        group_obj.insert("doclist".to_string(), Value::Object(doclist));
        groups_arr.push(Value::Object(group_obj));
    }
    block.insert("groups".to_string(), json!(groups_arr));
    Ok(Value::Object(block))
}

/// Renders a group value the way Solr emits `groupValue`: the typed value, or
/// JSON `null` for the "missing field" group.
fn group_value_json(value: &Option<SortValue>) -> Value {
    match value {
        None => Value::Null,
        Some(SortValue::Str(s)) => Value::String(s.clone()),
        Some(SortValue::I64(i)) => json!(i),
        Some(SortValue::F64(f)) => json!(f),
    }
}
