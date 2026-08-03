//! "Collect every match, order it deterministically" collector, with optional
//! fast-field ordering for the `sort` parameter (issue #2).
//!
//! With no `sort`, result order only needs to be *deterministic* and to match
//! Solr's observed behaviour for a single-segment, equally-scored corpus: ties
//! break by ascending document order. That case is expressed here as the single
//! implicit clause `score desc`, so the unsorted path is literally the same code
//! path as `sort=score desc` — which is what the fixtures say Solr does
//! (`select_sort_score_all.json` is byte-identical in ordering to
//! `select_all.json`). Using our own collector instead of trusting `TopDocs`'
//! internal tie-break keeps that guarantee explicit rather than incidental.
//!
//! **Why not `TopDocs::order_by_fast_field`?** The issue proposed it, but it
//! cannot express what the fixtures require: it orders by exactly one fast
//! field, so there is no way to compose `score desc,id asc`; it has no
//! ascending-`score` mode; and it applies no min/max selector, so it cannot
//! reproduce Solr's `SortedSetSortField` semantics on a multiValued field
//! (`asc` = each doc's minimum value, `desc` = each doc's maximum, missing
//! last). Ordering therefore lives here, alongside the tie-break it has to
//! coexist with.
//!
//! ponytail: sort keys are materialised per matching document (strings are
//! resolved out of the term dictionary during collection) and the whole match
//! set is sorted in memory. Ceiling: fine for a corpus that fits a single
//! `Vec`, wrong for a large index, where a bounded heap over `start + rows`
//! with segment-local term ordinals is the real answer. `search()` already
//! returns the whole match list unpaginated, so this is not the first place
//! that ceiling bites.

use std::cmp::Ordering;

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::{Column, StrColumn};
use tantivy::{DateTime, DocAddress, DocId, Score, SegmentOrdinal, SegmentReader};

use crate::schema::ValueKind;

/// What a single sort clause orders by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// Relevance score. Solr's `score` pseudo-field.
    Score,
    /// A `fast = true` schema field, by name.
    Field(String),
}

/// One `<key> <asc|desc>` clause of a `sort` spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortClause {
    pub key: SortKey,
    pub descending: bool,
    /// The schema-declared value kind of `key`, when it is a `Field` — `None`
    /// for `Score` (never missing, so the kind is never consulted). Only used
    /// to pick the *type* of a segment-wide missing default in the defensive
    /// `Absent` arm (finding 36/37): an absent column carries no type
    /// information of its own, so the clause has to carry it in from the
    /// schema instead (`check_sort` resolves it via
    /// `WayfinderSchema::value_kind`, which already folds in any custom
    /// `[[field_types]]` — those only ever resolve to `Text`, so there is no
    /// numeric/date custom-type case this can miss). See `Absent`'s doc
    /// comment for when that arm is actually reached.
    pub value_kind: Option<ValueKind>,
}

impl SortClause {
    pub fn new(key: SortKey, descending: bool, value_kind: Option<ValueKind>) -> SortClause {
        SortClause {
            key,
            descending,
            value_kind,
        }
    }
}

/// A materialised sort key value. Dates collapse into `I64` (their UTC
/// timestamp) because that is the order Lucene sorts them in too.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SortValue {
    Str(String),
    I64(i64),
    F64(f64),
}

impl SortValue {
    /// Total order within a variant. Mixed variants cannot occur — a clause's
    /// values all come from one column type — so they compare equal rather than
    /// panicking.
    fn cmp_value(&self, other: &SortValue) -> Ordering {
        match (self, other) {
            (SortValue::Str(a), SortValue::Str(b)) => a.cmp(b),
            (SortValue::I64(a), SortValue::I64(b)) => a.cmp(b),
            // `unwrap_or(Equal)` is currently unreachable: `a`/`b` are either a
            // relevance score (never NaN) or a `pfloat` fast-field value, and
            // `serde_json` has no NaN literal to parse into one. If a NaN ever
            // did reach here, the consequence is a possible `sort_by` panic
            // (a non-total order), not UB.
            (SortValue::F64(a), SortValue::F64(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            _ => Ordering::Equal,
        }
    }
}

/// One collected match plus one sort key per clause, in clause order. A `None`
/// key means the document has no value for that clause's field.
///
/// `pub` only because it is the `SegmentCollector::Fruit` element type, which
/// Tantivy exposes in the trait's public interface; the module itself is private.
pub struct Hit {
    addr: DocAddress,
    score: Score,
    keys: Vec<Option<SortValue>>,
}

pub struct AllScoredHits {
    clauses: Vec<SortClause>,
}

impl AllScoredHits {
    /// Orders by `clauses`, then always by ascending `DocAddress` as the final
    /// tie-break. An empty `clauses` becomes the implicit `score desc`, which is
    /// both Solr's default sort and the unsorted order the tracer-bullet
    /// fixtures pin.
    pub fn new(mut clauses: Vec<SortClause>) -> AllScoredHits {
        if clauses.is_empty() {
            clauses.push(SortClause::new(SortKey::Score, true, None));
        }
        AllScoredHits { clauses }
    }

    fn compare(&self, a: &Hit, b: &Hit) -> Ordering {
        compare_hits(&self.clauses, a, b)
    }
}

/// The one ordering both collectors share: clause by clause, then always by
/// ascending `DocAddress`. Never `Equal` for two distinct documents, so any
/// bounded prefix of it is deterministic. Extracted from `AllScoredHits` so
/// `TopScoredHits` (issue #242) cannot drift from it.
fn compare_hits(clauses: &[SortClause], a: &Hit, b: &Hit) -> Ordering {
    for (i, clause) in clauses.iter().enumerate() {
        let ord = match (&a.keys[i], &b.keys[i]) {
            (None, None) => Ordering::Equal,
            // `None` only reaches here for a string-typed key now: numeric/
            // float/date clauses materialise a missing value as 0 (or the
            // epoch) before this comparator ever runs (finding 36/37,
            // `SegmentSortColumn::value`/`Absent`), so this arm is reached
            // only by `SortKey::Field` on a `Str`/`Absent(None)` column.
            // Missing sorts last in *both* directions for those, so the
            // direction is deliberately not applied here (finding:
            // `select_sort_mv_asc` and `select_sort_mv_desc` both put doc5
            // last).
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(x), Some(y)) => {
                let ord = x.cmp_value(y);
                if clause.descending {
                    ord.reverse()
                } else {
                    ord
                }
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.addr.cmp(&b.addr)
}

impl Collector for AllScoredHits {
    type Fruit = Vec<(Score, DocAddress)>;
    type Child = AllScoredHitsSegmentCollector;

    fn for_segment(
        &self,
        segment_ord: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let mut columns = Vec::with_capacity(self.clauses.len());
        for clause in &self.clauses {
            // The direction travels with the column because the multiValued
            // selector depends on it (min for `asc`, max for `desc`), so a
            // clause's key is only meaningful together with its own direction.
            columns.push((SegmentSortColumn::open(segment, clause)?, clause.descending));
        }
        Ok(AllScoredHitsSegmentCollector {
            segment_ord,
            columns,
            hits: Vec::new(),
            scratch: String::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        // Always true, even when no clause mentions `score`: the collector's
        // fruit carries the score for the caller, and keeping it unconditional
        // keeps the unsorted path bit-identical to before ordering existed.
        true
    }

    fn merge_fruits(&self, segment_fruits: Vec<Vec<Hit>>) -> tantivy::Result<Self::Fruit> {
        let mut all: Vec<Hit> = segment_fruits.into_iter().flatten().collect();
        all.sort_by(|a, b| self.compare(a, b));
        Ok(all.into_iter().map(|h| (h.score, h.addr)).collect())
    }
}

/// The per-segment reader backing one sort clause.
enum SegmentSortColumn {
    Score,
    Str(StrColumn),
    I64(Column<i64>),
    F64(Column<f64>),
    Date(Column<DateTime>),
    /// Live and reachable via dynamic-only fields (issue #66): a
    /// dynamic-only column (e.g. Drupal's `its_`/`ds_` classes matched only
    /// through `check_sort`'s dynamic-field fallback, not a schema-declared
    /// static field) is a JSON-path column that Tantivy only materialises in
    /// segments where *some* document actually carried that key. A segment
    /// built entirely from a batch of docs that never used the key has no
    /// column for it at all — unlike a schema-declared fast field, which
    /// Tantivy 0.26 materialises in every segment regardless of whether any
    /// doc in it used the field (`FastFieldsWriter::new` calls
    /// `record_column_type` for each declared fast field at writer
    /// construction). So this variant is not reachable for a static
    /// schema-declared field, but is reachable for a dynamic-only match
    /// whenever the core has a multi-segment index where only some segments'
    /// source batches carried the key (covered by the multi-segment test in
    /// `tests/dynamic_sort_facet.rs`).
    ///
    /// The missing-value handling below is still correct in that case: since
    /// the column is absent for every document in the segment, every doc
    /// reads as `missing` — `None` for a string-typed field (missing-last,
    /// finding 16), or the type's zero value for a numeric/float/date-typed
    /// field (missing-as-zero, finding 36/37) — resolved once at `open` time
    /// from `clause.value_kind`, since an absent column carries no type of
    /// its own to read the default from.
    Absent(Option<SortValue>),
}

impl SegmentSortColumn {
    fn open(segment: &SegmentReader, clause: &SortClause) -> tantivy::Result<SegmentSortColumn> {
        let name = match &clause.key {
            SortKey::Score => return Ok(SegmentSortColumn::Score),
            SortKey::Field(name) => name,
        };
        let fast = segment.fast_fields();
        // Probed in turn rather than driven off the schema's declared type: the
        // columnar reader is the authority on what is actually stored, and a
        // type that does not match simply reports no column.
        if let Some(col) = fast.str(name)? {
            return Ok(SegmentSortColumn::Str(col));
        }
        if let Some(col) = fast.column_opt::<i64>(name)? {
            return Ok(SegmentSortColumn::I64(col));
        }
        if let Some(col) = fast.column_opt::<f64>(name)? {
            return Ok(SegmentSortColumn::F64(col));
        }
        if let Some(col) = fast.column_opt::<DateTime>(name)? {
            return Ok(SegmentSortColumn::Date(col));
        }
        let missing = match clause.value_kind {
            Some(ValueKind::I64) => Some(SortValue::I64(0)),
            Some(ValueKind::F64) => Some(SortValue::F64(0.0)),
            // Dates collapse into `I64` timestamps elsewhere in this module
            // (see `SortValue`'s doc comment); the epoch is timestamp 0.
            Some(ValueKind::Date) => Some(SortValue::I64(0)),
            Some(ValueKind::Text) | None => None,
        };
        Ok(SegmentSortColumn::Absent(missing))
    }

    /// This document's sort value for the clause, applying Lucene's
    /// `SortedSetSortField`/`SortedNumericSortField` selector: the minimum of a
    /// multi-valued field under `asc`, the maximum under `desc`. Single-valued
    /// fields are unaffected (min == max).
    ///
    /// A document with no value is type-dependent (findings 16/36/37): a
    /// string-typed field yields `None`, which the comparator sorts last
    /// regardless of direction; a numeric/float/date-typed field materialises
    /// the value `0` (epoch for dates) *before* the direction/comparison ever
    /// runs, so it participates in ordering like any other value rather than
    /// being pinned last or first.
    fn value(
        &self,
        doc: DocId,
        score: Score,
        descending: bool,
        scratch: &mut String,
    ) -> Option<SortValue> {
        match self {
            SegmentSortColumn::Score => Some(SortValue::F64(score as f64)),
            SegmentSortColumn::Absent(missing) => missing.clone(),
            SegmentSortColumn::Str(col) => {
                // The term dictionary is ordered, so selecting the min/max
                // ordinal selects the min/max string.
                let ord = select(col.term_ords(doc), descending)?;
                scratch.clear();
                match col.ord_to_str(ord, scratch) {
                    Ok(true) => Some(SortValue::Str(scratch.clone())),
                    _ => None,
                }
            }
            SegmentSortColumn::I64(col) => {
                let selected = select(col.values_for_doc(doc), descending).unwrap_or(0);
                Some(SortValue::I64(selected))
            }
            SegmentSortColumn::F64(col) => {
                // `f64` is not `Ord`, so `select` cannot be reused here.
                let values = col.values_for_doc(doc);
                let selected = if descending {
                    values.reduce(f64::max)
                } else {
                    values.reduce(f64::min)
                };
                Some(SortValue::F64(selected.unwrap_or(0.0)))
            }
            SegmentSortColumn::Date(col) => {
                let selected = select(col.values_for_doc(doc), descending)
                    .map(|d| d.into_timestamp_nanos())
                    .unwrap_or(0);
                Some(SortValue::I64(selected))
            }
        }
    }
}

/// The maximum of `values` when `descending`, the minimum otherwise; `None` for
/// an empty iterator.
fn select<T: Ord>(values: impl Iterator<Item = T>, descending: bool) -> Option<T> {
    if descending {
        values.max()
    } else {
        values.min()
    }
}

pub struct AllScoredHitsSegmentCollector {
    segment_ord: SegmentOrdinal,
    columns: Vec<(SegmentSortColumn, bool)>,
    hits: Vec<Hit>,
    /// Reused string buffer for term-dictionary lookups, so ordering on a string
    /// field does not allocate twice per document per clause.
    scratch: String,
}

impl SegmentCollector for AllScoredHitsSegmentCollector {
    type Fruit = Vec<Hit>;

    fn collect(&mut self, doc: DocId, score: Score) {
        // Moved out so `columns` can be iterated while `scratch` is borrowed
        // mutably; put back before returning.
        let mut scratch = std::mem::take(&mut self.scratch);
        let keys: Vec<Option<SortValue>> = self
            .columns
            .iter()
            .map(|(column, descending)| column.value(doc, score, *descending, &mut scratch))
            .collect();
        self.scratch = scratch;
        self.hits.push(Hit {
            addr: DocAddress::new(self.segment_ord, doc),
            score,
            keys,
        });
    }

    fn harvest(self) -> Self::Fruit {
        self.hits
    }
}

/// What a bounded search returns: the true totals plus only the first
/// `limit` hits of the exact order `AllScoredHits` would have produced.
#[derive(Debug, PartialEq)]
pub struct TopOutcome {
    /// How many documents matched in total — `response.numFound`.
    pub num_found: usize,
    /// Max score across *all* matches, not just the kept prefix — the
    /// unpaginated-global semantics `/select`'s `maxScore` already had.
    /// `None` when nothing matched.
    pub max_score: Option<Score>,
    /// The first `min(limit, num_found)` hits, ordered identically to
    /// `AllScoredHits`' full sort.
    pub top: Vec<(Score, DocAddress)>,
}

/// Bounded counterpart of [`AllScoredHits`] (issue #242): same clauses, same
/// comparator (`compare_hits`), same tie-break — but each segment keeps at
/// most ~`2 * limit` candidates instead of every match, so a `rows=10`
/// request over a 2M-doc match set stops allocating and sorting the whole
/// set. Counting and max-score tracking still see every match.
pub struct TopScoredHits {
    clauses: Vec<SortClause>,
    limit: usize,
}

impl TopScoredHits {
    /// `limit` is the number of leading hits to keep (`start + rows` for a
    /// paginated request). `0` is valid and collects totals only. An empty
    /// `clauses` becomes the implicit `score desc`, exactly as
    /// [`AllScoredHits::new`] does.
    pub fn new(mut clauses: Vec<SortClause>, limit: usize) -> TopScoredHits {
        if clauses.is_empty() {
            clauses.push(SortClause::new(SortKey::Score, true, None));
        }
        TopScoredHits { clauses, limit }
    }
}

/// Per-segment totals plus that segment's own leading candidates. The global
/// top-`limit` is a subset of the union of per-segment top-`limit`s, so
/// truncating per segment loses nothing.
pub struct TopSegmentFruit {
    hits: Vec<Hit>,
    count: usize,
    max_score: Option<Score>,
}

impl Collector for TopScoredHits {
    type Fruit = TopOutcome;
    type Child = TopScoredHitsSegmentCollector;

    fn for_segment(
        &self,
        segment_ord: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let mut columns = Vec::with_capacity(self.clauses.len());
        for clause in &self.clauses {
            columns.push((SegmentSortColumn::open(segment, clause)?, clause.descending));
        }
        Ok(TopScoredHitsSegmentCollector {
            segment_ord,
            clauses: self.clauses.clone(),
            columns,
            limit: self.limit,
            // Deliberately not `with_capacity(limit)`: `limit` is
            // client-controlled via `start`, and a huge `start` must not
            // become a huge allocation before a single doc has matched.
            hits: Vec::new(),
            pruned: false,
            count: 0,
            max_score: None,
            scratch: String::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        // Same rationale as `AllScoredHits`: the fruit carries scores for the
        // caller (`maxScore`, per-doc `score`) whatever the sort says.
        true
    }

    fn merge_fruits(&self, segment_fruits: Vec<TopSegmentFruit>) -> tantivy::Result<Self::Fruit> {
        let mut num_found = 0usize;
        let mut max_score: Option<Score> = None;
        let mut all: Vec<Hit> = Vec::new();
        for fruit in segment_fruits {
            num_found += fruit.count;
            if let Some(s) = fruit.max_score {
                max_score = Some(max_score.map_or(s, |a| a.max(s)));
            }
            all.extend(fruit.hits);
        }
        all.sort_by(|a, b| compare_hits(&self.clauses, a, b));
        all.truncate(self.limit);
        Ok(TopOutcome {
            num_found,
            max_score,
            top: all.into_iter().map(|h| (h.score, h.addr)).collect(),
        })
    }
}

pub struct TopScoredHitsSegmentCollector {
    segment_ord: SegmentOrdinal,
    clauses: Vec<SortClause>,
    columns: Vec<(SegmentSortColumn, bool)>,
    limit: usize,
    hits: Vec<Hit>,
    /// Whether `hits[limit - 1]` is currently this segment's `limit`-th best
    /// (true after the first prune), making it a valid discard threshold.
    pruned: bool,
    count: usize,
    max_score: Option<Score>,
    scratch: String,
}

impl SegmentCollector for TopScoredHitsSegmentCollector {
    type Fruit = TopSegmentFruit;

    fn collect(&mut self, doc: DocId, score: Score) {
        self.count += 1;
        self.max_score = Some(self.max_score.map_or(score, |a| a.max(score)));
        if self.limit == 0 {
            return;
        }

        let mut scratch = std::mem::take(&mut self.scratch);
        let keys: Vec<Option<SortValue>> = self
            .columns
            .iter()
            .map(|(column, descending)| column.value(doc, score, *descending, &mut scratch))
            .collect();
        self.scratch = scratch;
        let hit = Hit {
            addr: DocAddress::new(self.segment_ord, doc),
            score,
            keys,
        };

        // After the first prune, `hits[limit - 1]` is the segment's current
        // `limit`-th best (select_nth put it there, and later pushes only
        // append). Anything that sorts after it can never enter this
        // segment's top `limit`, and the global top is a subset of the
        // per-segment tops, so discarding here is lossless.
        if self.pruned && compare_hits(&self.clauses, &hit, &self.hits[self.limit - 1]).is_gt() {
            return;
        }
        self.hits.push(hit);

        if self.hits.len() >= self.limit.saturating_mul(2) {
            let clauses = std::mem::take(&mut self.clauses);
            self.hits
                .select_nth_unstable_by(self.limit - 1, |a, b| compare_hits(&clauses, a, b));
            self.clauses = clauses;
            self.hits.truncate(self.limit);
            self.pruned = true;
        }
    }

    fn harvest(self) -> Self::Fruit {
        TopSegmentFruit {
            hits: self.hits,
            count: self.count,
            max_score: self.max_score,
        }
    }
}

/// Result grouping (issue #290, PRD §5 v3). Tantivy has no native grouping
/// collector, so this buckets each matching document by its fast-field group
/// value and keeps the within-group-sorted doc list per bucket.
///
/// Two orderings are in play, and they are NOT the same param:
/// - **Group order** (the order `groups[]` is emitted in) is the relevance of
///   each group's top document under the *main* `sort` param. The collector
///   therefore sorts every match by `main_clauses` first; the first time a
///   group value is seen in that order is its rank.
/// - **Within-group order** is `group.sort` (defaulting to the main `sort`).
///   Each bucket is sorted by `within_clauses` independently.
///
/// `group.limit`/`group.offset` (paging *within* a group) and `rows`/`start`
/// (paging the *groups* list) are applied by the envelope builder in
/// `src/grouping.rs`, not here: the collector hands back every group with its
/// full within-group-sorted doc list, so the builder can re-paginate without
/// re-collecting. ponytail: that materialises the whole match set, same
/// ceiling `AllScoredHits` already carries ("fine for a corpus that fits a
/// single `Vec`").
///
/// Only single-valued non-text fields are groupable -- the caller validates
/// that and constructs `group_clause` (a `SortClause` over the group field's
/// fast column) so this collector can read each doc's value through the same
/// `SegmentSortColumn` machinery sorting already uses. A doc with no value
/// for the field lands in the `None` (null) group, exactly as Solr emits a
/// `groupValue: null` group.
pub struct GroupingCollector {
    main_clauses: Vec<SortClause>,
    within_clauses: Vec<SortClause>,
    group_clause: SortClause,
    /// `true` when `within_clauses` is identical to `main_clauses` (the common
    /// case: `group.sort` absent). Lets `collect` reuse the already-materialised
    /// main keys for within-group ordering instead of reading the same columns
    /// twice per document.
    within_is_main: bool,
}

impl GroupingCollector {
    /// `main_clauses` is the request's `sort` (default `score desc`).
    /// `within_clauses` is `group.sort`, or a clone of `main_clauses` when the
    /// request omits `group.sort`. `group_clause` reads the group field's value.
    ///
    /// An empty `main_clauses`/`within_clauses` becomes the implicit
    /// `score desc`, exactly as [`AllScoredHits::new`] and
    /// [`TopScoredHits::new`] do — so an unsorted grouped request ranks groups
    /// by their top doc's relevance, not by document address.
    pub fn new(
        mut main_clauses: Vec<SortClause>,
        mut within_clauses: Vec<SortClause>,
        group_clause: SortClause,
    ) -> GroupingCollector {
        if main_clauses.is_empty() {
            main_clauses.push(SortClause::new(SortKey::Score, true, None));
        }
        if within_clauses.is_empty() {
            within_clauses.push(SortClause::new(SortKey::Score, true, None));
        }
        let within_is_main = within_clauses == main_clauses;
        GroupingCollector {
            main_clauses,
            within_clauses,
            group_clause,
            within_is_main,
        }
    }
}

/// One collected match for grouping: where it lives, its score, its group
/// value, and the sort keys for both orderings.
pub struct GroupRecord {
    addr: DocAddress,
    score: Score,
    /// The group field's value for this doc, `None` for the null group.
    group: Option<SortValue>,
    /// Sort keys under the main `sort` (drives group ranking).
    main_keys: Vec<Option<SortValue>>,
    /// Sort keys under `group.sort`. Empty when `within_is_main` (use
    /// `main_keys`).
    within_keys: Vec<Option<SortValue>>,
}

/// A groupable bucket key. `f64` is not `Eq`/`Hash`, so floating group values
/// key on their bit pattern (a NaN group is vanishingly unlikely for a
/// single-valued docValues field, and two distinct NaN patterns collapsing to
/// one bucket is no worse than `f64`'s own NaN-equality).
#[derive(Clone, PartialEq, Eq, Hash)]
enum GroupKey {
    None,
    Str(String),
    I64(i64),
    F64Bits(u64),
}

impl GroupKey {
    fn from_value(value: &Option<SortValue>) -> GroupKey {
        match value {
            None => GroupKey::None,
            Some(SortValue::Str(s)) => GroupKey::Str(s.clone()),
            Some(SortValue::I64(i)) => GroupKey::I64(*i),
            Some(SortValue::F64(f)) => GroupKey::F64Bits(f.to_bits()),
        }
    }
}

/// The structured grouping result, before pagination/rendering. Every group
/// appears in group-rank order with its full within-group-sorted doc list;
/// `src/grouping.rs` applies `group.limit`/`group.offset`/`rows`/`start` and
/// renders the docs.
pub struct GroupingFruit {
    /// Total documents matching `q` AND every `fq` -- `grouped.<field>.matches`.
    pub matches: usize,
    /// Groups in group-rank order (main sort of each group's top doc).
    pub groups: Vec<RankedGroup>,
}

/// One group: its value, its full size, and its docs ordered by `group.sort`.
pub struct RankedGroup {
    /// The group field value, or `None` for the null group (docs missing the
    /// field). Kept as the first-seen `SortValue` so the envelope can render
    /// the original typed value (string vs number) without re-reading it.
    pub value: Option<SortValue>,
    /// Docs in the group, regardless of `group.limit` -- the full count.
    pub num_found: usize,
    /// The group's docs in within-group order (`group.sort`, default main
    /// sort). Not yet limited/offset.
    pub docs: Vec<(Score, DocAddress)>,
}

impl Collector for GroupingCollector {
    type Fruit = GroupingFruit;
    type Child = GroupingSegmentCollector;

    fn for_segment(
        &self,
        segment_ord: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let main_columns = self
            .main_clauses
            .iter()
            .map(|c| SegmentSortColumn::open(segment, c).map(|col| (col, c.descending)))
            .collect::<tantivy::Result<Vec<_>>>()?;
        let within_columns = if self.within_is_main {
            Vec::new()
        } else {
            self.within_clauses
                .iter()
                .map(|c| SegmentSortColumn::open(segment, c).map(|col| (col, c.descending)))
                .collect::<tantivy::Result<Vec<_>>>()?
        };
        let group_column = SegmentSortColumn::open(segment, &self.group_clause)?;
        Ok(GroupingSegmentCollector {
            segment_ord,
            main_columns,
            within_columns,
            group_column,
            within_is_main: self.within_is_main,
            records: Vec::new(),
            scratch: String::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        // Scores are carried for `doclist.maxScore` and for the default
        // `score desc` ordering, exactly as the other collectors do.
        true
    }

    fn merge_fruits(&self, segment_fruits: Vec<Vec<GroupRecord>>) -> tantivy::Result<Self::Fruit> {
        let mut all: Vec<GroupRecord> = segment_fruits.into_iter().flatten().collect();
        let matches = all.len();

        // Group rank = first-seen order in main-sort order. Sorting the whole
        // match set by `main_clauses` (then DocAddress, via `compare_hits`)
        // makes each group's first occurrence its top document under the main
        // sort, which is exactly Solr's group-ordering rule.
        let main_clauses = self.main_clauses.clone();
        all.sort_by(|a, b| {
            let ha = Hit {
                addr: a.addr,
                score: a.score,
                keys: a.main_keys.clone(),
            };
            let hb = Hit {
                addr: b.addr,
                score: b.score,
                keys: b.main_keys.clone(),
            };
            compare_hits(&main_clauses, &ha, &hb)
        });

        // Bucket preserving first-seen order.
        let mut order: Vec<GroupKey> = Vec::new();
        let mut index: std::collections::HashMap<GroupKey, usize> =
            std::collections::HashMap::new();
        let mut buckets: Vec<Vec<GroupRecord>> = Vec::new();
        for rec in all {
            let key = GroupKey::from_value(&rec.group);
            let idx = match index.get(&key) {
                Some(&i) => i,
                None => {
                    let i = order.len();
                    index.insert(key, i);
                    order.push(GroupKey::None); // placeholder, fixed below
                    buckets.push(Vec::new());
                    i
                }
            };
            buckets[idx].push(rec);
        }

        let within_clauses = self.within_clauses.clone();
        let within_is_main = self.within_is_main;
        let groups = buckets
            .into_iter()
            .map(|mut bucket| {
                let value = bucket.first().and_then(|r| r.group.clone());
                let num_found = bucket.len();
                if !within_is_main {
                    let wc = within_clauses.clone();
                    bucket.sort_by(|a, b| {
                        let ha = Hit {
                            addr: a.addr,
                            score: a.score,
                            keys: a.within_keys.clone(),
                        };
                        let hb = Hit {
                            addr: b.addr,
                            score: b.score,
                            keys: b.within_keys.clone(),
                        };
                        compare_hits(&wc, &ha, &hb)
                    });
                }
                // When within == main, the bucket is already in main-sort
                // order from the global sort above, which is the correct
                // within-group order too.
                let docs = bucket.into_iter().map(|r| (r.score, r.addr)).collect();
                RankedGroup {
                    value,
                    num_found,
                    docs,
                }
            })
            .collect();
        Ok(GroupingFruit { matches, groups })
    }
}

pub struct GroupingSegmentCollector {
    segment_ord: SegmentOrdinal,
    main_columns: Vec<(SegmentSortColumn, bool)>,
    within_columns: Vec<(SegmentSortColumn, bool)>,
    group_column: SegmentSortColumn,
    within_is_main: bool,
    records: Vec<GroupRecord>,
    scratch: String,
}

impl SegmentCollector for GroupingSegmentCollector {
    type Fruit = Vec<GroupRecord>;

    fn collect(&mut self, doc: DocId, score: Score) {
        let mut scratch = std::mem::take(&mut self.scratch);
        let main_keys: Vec<Option<SortValue>> = self
            .main_columns
            .iter()
            .map(|(col, desc)| col.value(doc, score, *desc, &mut scratch))
            .collect();
        // The group value: `descending` is irrelevant for a single-valued
        // field (min == max), so `false` is a no-op selector.
        let group = self.group_column.value(doc, score, false, &mut scratch);
        let within_keys = if self.within_is_main {
            Vec::new()
        } else {
            self.within_columns
                .iter()
                .map(|(col, desc)| col.value(doc, score, *desc, &mut scratch))
                .collect()
        };
        self.scratch = scratch;
        self.records.push(GroupRecord {
            addr: DocAddress::new(self.segment_ord, doc),
            score,
            group,
            main_keys,
            within_keys,
        });
    }

    fn harvest(self) -> Self::Fruit {
        self.records
    }
}
