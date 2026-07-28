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
    /// to pick the *type* of a segment-wide missing default when the field's
    /// column is entirely `Absent` in a segment (finding 36/37): an absent
    /// column carries no type information of its own, so the clause has to
    /// carry it in from the schema instead (`check_sort` resolves it via
    /// `WayfinderSchema::value_kind`, which already folds in any custom
    /// `[[field_types]]` — those only ever resolve to `Text`, so there is no
    /// numeric/date custom-type case this can miss).
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
enum SortValue {
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
        for (i, clause) in self.clauses.iter().enumerate() {
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
    /// The field is fast in the schema but has no column in this segment (no
    /// document in it carried a value). Every document in this segment reads
    /// as `missing`: `None` for a string-typed field (missing-last, finding
    /// 16), or the type's zero value for a numeric/float/date-typed field
    /// (missing-as-zero, finding 36/37) — resolved once at `open` time from
    /// `clause.value_kind`, since the absent column itself carries no type.
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
