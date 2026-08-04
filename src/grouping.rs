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
//! ## The other components, and the two flags that reshape them (issue #338)
//!
//! A grouped response is not `grouped`-only: it carries `facet_counts`,
//! `stats`, `highlighting` and `spellcheck` exactly as an ungrouped one does,
//! each gated by its own `facet`/`stats`/`hl`/`spellcheck` param, with
//! `grouped` standing where `response` would (findings 159/160/161). `select`
//! therefore *falls through* into the shared component code rather than
//! returning early, and this module hands it back the pieces only the grouping
//! pass can know:
//!
//! - [`GroupedOutcome::rendered`] — every document the doclists actually
//!   rendered, deduplicated across all `group.field` blocks and all groups.
//!   `highlighting` covers exactly that set, the way it covers `response.docs`
//!   on the ungrouped path (`g338_hl`).
//! - [`GroupedOutcome::truncate_docs`] — `group.truncate=true`'s *collapsed*
//!   document set: each group's first document in `group.sort` order, from the
//!   **first** `group.field`, taken straight off the collector's fruit and so
//!   independent of `rows`/`start`/`group.limit`/`group.offset`. `select`
//!   intersects it into the facet/stats base query (as a [`DocSetQuery`]), so
//!   facets, `stats`, `facet.query` and `facet.range` are all computed over the
//!   collapsed set while `grouped` itself stays untouched (`g338_truncate*`).
//! - [`GroupedOutcome::group_facet`] — `group.facet=true`'s counting context:
//!   every facet count becomes a count of *distinct matching groups* of the
//!   first `group.field`, for field facets, `facet.query` and `facet.range`
//!   alike. `stats` is deliberately unaffected (`g338_groupfacet_stats`).
//!
//! The two flags compose without a special case: `group.truncate` restricts the
//! base query, `group.facet` counts groups over whatever base it is given, and
//! in the collapsed set every document is its own group — which is why
//! `g338_groupfacet_truncate`'s facet block equals `g338_truncate`'s.
//!
//! Ground truth: every shape decision here is pinned by a fixture in
//! `solr-ref/responses/group_*.json` (issue #290) or
//! `solr-ref/responses/g338_*.json` (issue #338), captured against a dedicated
//! `grouping` Solr core (`solr-ref/capture.sh`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tantivy::index::SegmentId;
use tantivy::query::{BooleanQuery, EnableScoring, Explanation, Occur, Query, Scorer, Weight};
use tantivy::{DocAddress, DocId, DocSet, Score, SegmentReader, TERMINATED, TantivyError, Term};

use crate::collector::{GroupingFruit, SortClause, SortKey, SortValue};
use crate::core_index::{CoreIndex, FacetOrderKey};
use crate::error::WfError;
use crate::params::Params;
use crate::schema::{ValueKind, WayfinderSchema};

/// The already-parsed `q` plus its filter queries, borrowed. `None` when the
/// request has no `q` (matches nothing). Named so the handler call site reads
/// cleanly and clippy's `type_complexity` stays quiet.
type ParsedQuery<'a> = Option<(&'a dyn Query, &'a [Box<dyn Query>])>;

/// Everything a grouped request produces that the rest of `select` needs: the
/// `grouped` block itself plus the three pieces only the grouping pass can
/// know. See this module's doc comment for what each one is for.
pub struct GroupedOutcome {
    /// The `grouped` response object, keyed by `group.field` in request order.
    pub block: Value,
    /// Every document the rendered doclists returned, deduplicated, in
    /// first-rendered order. `highlighting`'s doc set.
    pub rendered: Vec<(Score, DocAddress)>,
    /// `group.truncate=true`: the collapsed document set (one per group of the
    /// first `group.field`). `None` when the flag is off or false.
    pub truncate_docs: Option<Vec<DocAddress>>,
    /// `group.facet=true`: the distinct-group counting context. `None` when the
    /// flag is off or false.
    pub group_facet: Option<GroupFacet>,
}

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
) -> Result<Option<GroupedOutcome>, WfError> {
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
    // Both #338 flags are gated exactly like `group`/`facet`/`stats`/`hl`: only
    // a literal `true` turns them on, and `false` is byte-identical to omitting
    // them (`g338_truncate_false`).
    let truncate = params.bool_or("group.truncate", false)?;
    let group_facet_requested = params.bool_or("group.facet", false)?;

    let schema = &index.wf_schema;
    let mut grouped = Map::new();
    let mut rendered: Vec<(Score, DocAddress)> = Vec::new();
    let mut rendered_seen: HashSet<DocAddress> = HashSet::new();
    let mut truncate_docs = None;
    let mut group_facet = None;
    for (position, field) in fields.iter().enumerate() {
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
        // Both flags read the FIRST `group.field`'s grouping and no other
        // (`g338_truncate_multi`, `g338_groupfacet_multi`), and both read the
        // *fruit* -- every matching doc of every group, in `group.sort` order,
        // unpaginated -- so neither can be perturbed by `rows`/`start` or
        // `group.limit`/`group.offset` (`g338_truncate_rows`,
        // `g338_groupfacet_rows`).
        if position == 0 {
            if truncate {
                truncate_docs = Some(collapsed_docs(&fruit));
            }
            if group_facet_requested {
                let column = schema
                    .resolved_fast_column(field)
                    .expect("validate_group_field confirmed this name resolves");
                group_facet = Some(GroupFacet::from_fruit(&fruit, column));
            }
        }
        let (block, block_docs) = build_group_block(
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
        // A document can be rendered by more than one `group.field` block (and,
        // with `group.limit`, more than once within one); `highlighting` is a
        // map keyed by unique key, so the union is deduplicated here rather
        // than highlighting the same document twice.
        for doc in block_docs {
            if rendered_seen.insert(doc.1) {
                rendered.push(doc);
            }
        }
        grouped.insert(field.clone(), block);
    }
    Ok(Some(GroupedOutcome {
        block: Value::Object(grouped),
        rendered,
        truncate_docs,
        group_facet,
    }))
}

/// `group.truncate`'s collapsed document set: each group's first document in
/// `group.sort` order. Read off the fruit, so it covers every group of the
/// whole match set regardless of paging.
fn collapsed_docs(fruit: &GroupingFruit) -> Vec<DocAddress> {
    fruit
        .groups
        .iter()
        .filter_map(|group| group.docs.first().map(|(_, addr)| *addr))
        .collect()
}

/// `group.facet=true`'s counting context, derived from the first
/// `group.field`'s fruit: which group each matching document belongs to, plus
/// what is needed to count *distinct groups* per facet bucket.
pub struct GroupFacet {
    /// Matching document -> its group's index in the fruit. Drives
    /// `facet.query` / `facet.range` (and `facet.missing`), where the bucket is
    /// defined by a query and the answer is "how many distinct groups do the
    /// matching documents fall into?".
    doc_group: HashMap<DocAddress, usize>,
    /// The Tantivy column of the group field, for the terms sub-aggregation
    /// that answers the same question for a field facet without walking
    /// documents in Wayfinder.
    group_column: String,
    /// The documents of the "field is absent" group. Solr's `null` group is a
    /// real group and counts, but a terms sub-aggregation on the group column
    /// cannot see documents with no value there, so these are counted
    /// separately (see [`GroupFacet::term_facet`]).
    null_docs: Vec<DocAddress>,
}

impl GroupFacet {
    fn from_fruit(fruit: &GroupingFruit, group_column: String) -> GroupFacet {
        let mut doc_group = HashMap::new();
        let mut null_docs = Vec::new();
        for (index, group) in fruit.groups.iter().enumerate() {
            for (_, addr) in &group.docs {
                doc_group.insert(*addr, index);
                if group.value.is_none() {
                    null_docs.push(*addr);
                }
            }
        }
        GroupFacet {
            doc_group,
            group_column,
            null_docs,
        }
    }

    /// How many distinct groups the documents matching `query` fall into — the
    /// group-counting replacement for `CoreIndex::count` behind `facet.query`,
    /// each `facet.range` bucket and `facet.missing`.
    pub(crate) fn distinct_groups(
        &self,
        index: &CoreIndex,
        query: &dyn Query,
    ) -> anyhow::Result<usize> {
        let mut seen: HashSet<usize> = HashSet::new();
        for addr in index.doc_set(query)? {
            // Every document matching `query` also matches the base query (`q`
            // AND every `fq`), which is exactly the set the fruit bucketed, so
            // this always hits. Skipping a miss is the conservative answer if
            // the two ever disagreed.
            if let Some(group) = self.doc_group.get(&addr) {
                seen.insert(*group);
            }
        }
        Ok(seen.len())
    }

    /// One field facet's buckets, counted in distinct groups instead of
    /// documents. Delegates the aggregation (and all bucket-key rendering) to
    /// `CoreIndex::term_facet_grouped`, then adds the one group that
    /// aggregation structurally cannot see: the `null` group, whose documents
    /// have no value in the group column at all. A term present on any
    /// still-matching `null`-group document gains exactly +1, because those
    /// documents are all one group.
    ///
    /// ponytail: the `null`-group correction below is unfixtured. The g338
    /// `group.facet` corpus has exactly one `null`-group document (`g6`), and it
    /// carries neither a `type` nor a `category` value, so no captured field
    /// facet has a term on a `null`-group document and mutating this branch away
    /// keeps the suite green. It is kept because the `null` group demonstrably
    /// *is* a group elsewhere in the same fixtures (`g338_groupfacet_blog`'s
    /// 0-25 range bucket counts 3, including `g6`'s group), so dropping it would
    /// undercount by one on any corpus where such a document has a facet value.
    pub(crate) fn term_facet(
        &self,
        index: &CoreIndex,
        column: &str,
        kind: Option<ValueKind>,
        query: &dyn Query,
    ) -> anyhow::Result<Vec<(String, FacetOrderKey, u64)>> {
        let mut buckets = index.term_facet_grouped(column, kind, &self.group_column, query)?;
        if self.null_docs.is_empty() {
            return Ok(buckets);
        }
        let null_only = BooleanQuery::from(vec![
            (Occur::Must, query.box_clone()),
            (
                Occur::Must,
                Box::new(doc_set_query(index, &self.null_docs)) as Box<dyn Query>,
            ),
        ]);
        let in_null_group: HashSet<String> = index
            .term_facet(column, kind, &null_only)?
            .into_iter()
            .filter(|(_, _, count)| *count > 0)
            .map(|(term, _, _)| term)
            .collect();
        for (term, _, count) in &mut buckets {
            if in_null_group.contains(term) {
                *count += 1;
            }
        }
        Ok(buckets)
    }
}

/// A [`DocSetQuery`] over `docs`, resolved against the index's current segment
/// ordinals. The one construction path, so no caller has to know that the
/// query keys its doc lists by `SegmentId` rather than by ordinal.
pub(crate) fn doc_set_query(index: &CoreIndex, docs: &[DocAddress]) -> DocSetQuery {
    DocSetQuery::new(&index.segment_ids(), docs)
}

/// A query matching exactly a fixed set of documents.
///
/// `group.truncate` needs the facet/stats base query restricted to the
/// collapsed group set, which is a set of `DocAddress`es rather than anything
/// the term dictionary can express. The alternative — a Boolean OR of one
/// `unique_key` term per group — is O(groups) clauses and degrades badly on a
/// real corpus, where the group count is unbounded; this is a flat per-segment
/// sorted doc list, so a whole pass costs one linear walk of it.
///
/// Doc lists are keyed by `SegmentId`, not by the segment ordinal the
/// `DocAddress`es carry: a `Weight` is handed a `&SegmentReader`, and the
/// ordinal is only meaningful relative to the exact searcher the addresses came
/// from. `SegmentId` is stable, so a searcher taken between collection and
/// counting cannot silently shift the doc set onto the wrong segment.
#[derive(Clone, Debug)]
pub struct DocSetQuery {
    docs: Arc<HashMap<SegmentId, Vec<DocId>>>,
}

impl DocSetQuery {
    fn new(segment_ids: &[SegmentId], docs: &[DocAddress]) -> DocSetQuery {
        let mut by_segment: HashMap<SegmentId, Vec<DocId>> = HashMap::new();
        for addr in docs {
            // An address whose ordinal is out of range cannot be matched
            // against any segment of this searcher, so it is dropped rather
            // than mapped onto the wrong one.
            if let Some(segment_id) = segment_ids.get(addr.segment_ord as usize) {
                by_segment.entry(*segment_id).or_default().push(addr.doc_id);
            }
        }
        // `DocSet` requires ascending, distinct doc ids; the collector hands
        // its groups back in `group.sort` order, which is neither.
        for docs in by_segment.values_mut() {
            docs.sort_unstable();
            docs.dedup();
        }
        DocSetQuery {
            docs: Arc::new(by_segment),
        }
    }
}

impl Query for DocSetQuery {
    fn weight(&self, _enable_scoring: EnableScoring<'_>) -> Result<Box<dyn Weight>, TantivyError> {
        Ok(Box::new(DocSetWeight {
            docs: Arc::clone(&self.docs),
        }))
    }

    fn query_terms<'a>(&'a self, _visitor: &mut dyn FnMut(&'a Term, bool)) {
        // Membership is an explicit doc list; there are no term clauses.
    }
}

struct DocSetWeight {
    docs: Arc<HashMap<SegmentId, Vec<DocId>>>,
}

impl Weight for DocSetWeight {
    fn scorer(
        &self,
        reader: &SegmentReader,
        _boost: Score,
    ) -> Result<Box<dyn Scorer>, TantivyError> {
        let docs = self
            .docs
            .get(&reader.segment_id())
            .cloned()
            .unwrap_or_default();
        // Tantivy's `DocSet` iteration is `doc()`-first, so a fresh scorer must
        // already sit ON its first match -- cursor 0 does.
        Ok(Box::new(DocSetScorer { docs, cursor: 0 }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> Result<Explanation, TantivyError> {
        let matches = self
            .docs
            .get(&reader.segment_id())
            .is_some_and(|docs| docs.binary_search(&doc).is_ok());
        Ok(Explanation::new_with_string(
            "DocSetQuery".to_string(),
            if matches { 1.0 } else { 0.0 },
        ))
    }
}

struct DocSetScorer {
    docs: Vec<DocId>,
    cursor: usize,
}

impl DocSet for DocSetScorer {
    fn advance(&mut self) -> DocId {
        self.cursor = self.cursor.saturating_add(1);
        self.doc()
    }

    fn doc(&self) -> DocId {
        self.docs.get(self.cursor).copied().unwrap_or(TERMINATED)
    }

    fn size_hint(&self) -> u32 {
        u32::try_from(self.docs.len()).unwrap_or(u32::MAX)
    }
}

impl Scorer for DocSetScorer {
    fn score(&mut self) -> Score {
        // Constant score: this query only ever restricts a base query.
        1.0
    }
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
/// (`rows`/`start`). Also returns every document the block actually rendered,
/// which is the set `highlighting` covers (issue #338).
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
) -> Result<(Value, Vec<(Score, DocAddress)>), WfError> {
    // Key order matches the fixtures: `matches`, `ngroups`, `groups`.
    let mut rendered = Vec::new();
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
            rendered.push((*score, *addr));
        }
        doclist.insert("docs".to_string(), json!(docs));

        group_obj.insert("doclist".to_string(), Value::Object(doclist));
        groups_arr.push(Value::Object(group_obj));
    }
    block.insert("groups".to_string(), json!(groups_arr));
    Ok((Value::Object(block), rendered))
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
