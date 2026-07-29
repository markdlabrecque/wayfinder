//! The Tantivy-backed core: build, index, and search a single Wayfinder
//! core (PRD open question 1 — single-core-per-process, so there's exactly
//! one of these per running `app()`).

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use tantivy::aggregation::agg_req::{Aggregation, AggregationVariants, Aggregations};
use tantivy::aggregation::agg_result::{AggregationResult, BucketResult};
use tantivy::aggregation::bucket::TermsAggregation;
use tantivy::aggregation::{
    AggContextParams, AggregationCollector, AggregationLimitsGuard, DEFAULT_BUCKET_LIMIT, Key,
};
use tantivy::collector::{Count, DocSetCollector};
use tantivy::query::{AllQuery, Query, QueryParser};
use tantivy::schema::OwnedValue;
use tantivy::time::OffsetDateTime;
use tantivy::time::format_description::well_known::Rfc3339;
use tantivy::{
    DateTime, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument,
};

use crate::collector::{AllScoredHits, SortClause};
use crate::config::ServerConfig;
use crate::schema::{self, ValueKind, WayfinderSchema};

/// Ceiling on how many distinct terms one `facet.field` enumerates. Tantivy's
/// own aggregation bucket limit is the binding constraint, so the request asks
/// for exactly that many and no more.
const MAX_FACET_TERMS: u32 = DEFAULT_BUCKET_LIMIT;

/// Renders one stored Tantivy value as the JSON Solr would return for it.
fn render_value<'a>(v: impl tantivy::schema::Value<'a>) -> Value {
    if let Some(s) = v.as_str() {
        return json!(s);
    }
    if let Some(i) = v.as_i64() {
        return json!(i);
    }
    if let Some(f) = v.as_f64() {
        return json!(f);
    }
    if let Some(d) = v.as_datetime() {
        // Solr renders dates as RFC3339 with a `Z`, seconds precision.
        return match d.into_utc().format(&Rfc3339) {
            Ok(s) => json!(s),
            Err(_) => Value::Null,
        };
    }
    Value::Null
}

/// Renders an `f64` the way Java's `Double.toString` renders a `pdouble`/
/// `pfloat` facet key (finding 39): an integral value gets a trailing `.0`
/// (`5.0`, `12.0`); a fractional one is unchanged from Rust's own
/// `to_string` for the shapes this issue's fixtures pin (`0.25`, `7.5`).
///
/// ponytail: this is not full `Double.toString` — Java switches to
/// scientific notation outside `1e-3..1e7`, and Rust's `f64::to_string`
/// disagrees with Java on digit count/rounding in general. No fixture pins
/// either of those; only the plain-decimal integral/fractional shapes above
/// are captured, so that is the ceiling here.
fn render_double(v: f64) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{v:.1}")
    } else {
        v.to_string()
    }
}

fn as_text<'a>(field: &str, v: &'a Value) -> Result<&'a str> {
    v.as_str()
        .ok_or_else(|| anyhow!("field `{field}` expects a string value, got {v}"))
}

fn as_i64(field: &str, v: &Value) -> Result<i64> {
    v.as_i64()
        .ok_or_else(|| anyhow!("field `{field}` expects an integer value, got {v}"))
}

fn as_f64(field: &str, v: &Value) -> Result<f64> {
    v.as_f64()
        .ok_or_else(|| anyhow!("field `{field}` expects a numeric value, got {v}"))
}

/// Solr dates are RFC3339 in UTC (`1995-12-31T23:59:59Z`).
fn as_date(field: &str, v: &Value) -> Result<DateTime> {
    let s = v
        .as_str()
        .ok_or_else(|| anyhow!("field `{field}` expects an RFC3339 date string, got {v}"))?;
    let parsed = OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| anyhow!("field `{field}` value `{s}` is not a valid RFC3339 date: {e}"))?;
    Ok(DateTime::from_utc(parsed))
}

/// Coerces a value destined for a catch-all JSON field, so a dynamic rule's
/// declared type still governs how the value is indexed (`*_i` gets an integer,
/// not the string `"7"`).
fn coerce_json(field: &str, kind: ValueKind, value: &Value) -> Result<Value> {
    if let Value::Array(vs) = value {
        return vs
            .iter()
            .map(|v| coerce_json(field, kind, v))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array);
    }
    Ok(match kind {
        ValueKind::Text => json!(as_text(field, value)?),
        ValueKind::I64 => json!(as_i64(field, value)?),
        ValueKind::F64 => json!(as_f64(field, value)?),
        // Kept as the original RFC3339 string, validated: Tantivy's JSON fields
        // parse date-shaped strings themselves.
        ValueKind::Date => {
            as_date(field, value)?;
            value.clone()
        }
    })
}

pub struct CoreIndex {
    pub wf_schema: WayfinderSchema,
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
}

impl CoreIndex {
    pub fn open(schema_path: &Path, data_dir: &Path, config: &ServerConfig) -> Result<CoreIndex> {
        let schema_toml = std::fs::read_to_string(schema_path)
            .with_context(|| format!("reading schema file {}", schema_path.display()))?;
        let wf_schema = schema::load(schema_path)?;
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;

        // Startup schema check (PRD §3 / open question 4): an index carries the
        // schema it was built with, and an incompatible change must refuse to
        // start rather than silently return wrong results — which is what
        // falling through to `open_in_dir` with the *old* schema would do.
        let snapshot = schema::snapshot_path(data_dir);
        if snapshot.exists() {
            let previous = std::fs::read_to_string(&snapshot)
                .with_context(|| format!("reading stored schema {}", snapshot.display()))?;
            schema::check_compatible(&previous, &schema_toml).with_context(|| {
                format!(
                    "the index in {} was built with an incompatible schema",
                    data_dir.display()
                )
            })?;
        }

        // `settings` only apply to a newly created index; re-opening an
        // existing one keeps the doc-store settings it was built with, which
        // is Tantivy's own rule and worth knowing before tuning them.
        let mut index = Index::builder()
            .schema(wf_schema.tantivy_schema.clone())
            .settings(config.index_settings()?)
            .create_in_dir(data_dir)
            .or_else(|_| Index::open_in_dir(data_dir))
            .context("opening/creating Tantivy index")?;
        index.set_tokenizers(wf_schema.tokenizers.clone());
        std::fs::write(&snapshot, &schema_toml)
            .with_context(|| format!("writing stored schema {}", snapshot.display()))?;

        // `writer_threads` defaults to 1: a single writer thread allocates doc
        // ids in insertion order, which is what the tie-break in
        // `AllScoredHits` relies on to match Solr's observed (insertion) order
        // on equally-scored matches.
        let writer: IndexWriter = index
            .writer_with_num_threads(config.indexing.writer_threads, config.indexing.writer_heap)
            .context("creating index writer")?;
        writer.set_merge_policy(config.merge_policy()?);

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("creating index reader")?;

        Ok(CoreIndex {
            wf_schema,
            index,
            writer: Mutex::new(writer),
            reader,
        })
    }

    /// Adds documents from a Solr-style JSON array-of-docs body. Returns the
    /// number of documents added.
    pub fn add_documents(&self, docs: &[Value]) -> Result<usize> {
        let writer = self.writer.lock().expect("index writer mutex poisoned");
        for doc in docs {
            let obj = doc
                .as_object()
                .ok_or_else(|| anyhow!("each document in the update body must be a JSON object"))?;
            let tantivy_doc = self.build_document(obj)?;
            writer.add_document(tantivy_doc)?;
        }
        Ok(docs.len())
    }

    /// Commits pending writes and makes them visible to subsequent searches.
    pub fn commit(&self) -> Result<()> {
        let mut writer = self.writer.lock().expect("index writer mutex poisoned");
        writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    fn build_document(&self, obj: &Map<String, Value>) -> Result<TantivyDocument> {
        let mut doc = TantivyDocument::default();
        for field_config in &self.wf_schema.fields {
            let Some(value) = obj.get(&field_config.name) else {
                if field_config.required {
                    return Err(anyhow!(
                        "document is missing required field `{}`",
                        field_config.name
                    ));
                }
                continue;
            };
            let kind = self
                .wf_schema
                .value_kind(&field_config.name)
                .expect("a declared field always has a value kind");
            self.add_value(&mut doc, &field_config.name, kind, value)?;

            // copy_fields: index-time only, so a copy destination sees the
            // source's raw value and analyzes it with its own field type.
            for dest in self.wf_schema.copy_dests(&field_config.name) {
                let dest_kind = self
                    .wf_schema
                    .value_kind(dest)
                    .expect("copy_fields destinations are validated at load time");
                self.add_value(&mut doc, dest, dest_kind, value)?;
            }
        }

        // Everything left over is either a dynamic-field match or an error.
        let mut dynamic: Map<String, Value> = Map::new();
        let mut dynamic_text: Map<String, Value> = Map::new();
        for (name, value) in obj {
            if self.wf_schema.is_static(name) {
                continue;
            }
            let Some(rule) = self.wf_schema.match_dynamic(name) else {
                // Matches strict Solr (`-Dupdate.autoCreateFields=false`):
                // `ERROR: [doc=<id>] unknown field '<name>'`. The `_default`
                // configset's schemaless auto-add is deliberately not copied —
                // PRD §3 rules out runtime schema mutation.
                let id = obj
                    .get(&self.wf_schema.core.unique_key)
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                return Err(anyhow!("ERROR: [doc={id}] unknown field '{name}'"));
            };
            let kind = schema::dynamic_value_kind(rule, &self.wf_schema.field_types)?;
            let target = self.wf_schema.dynamic_target(rule);
            let coerced = coerce_json(name, kind, value)?;
            if target == schema::DYNAMIC_TEXT_FIELD {
                dynamic_text.insert(name.clone(), coerced);
            } else {
                dynamic.insert(name.clone(), coerced);
            }
        }
        for (field_name, map) in [
            (schema::DYNAMIC_FIELD, dynamic),
            (schema::DYNAMIC_TEXT_FIELD, dynamic_text),
        ] {
            if map.is_empty() {
                continue;
            }
            let field = self
                .wf_schema
                .field(field_name)
                .expect("the catch-all json fields exist whenever a dynamic rule matched");
            let object: std::collections::BTreeMap<String, OwnedValue> = map
                .into_iter()
                .map(|(k, v)| (k, OwnedValue::from(v)))
                .collect();
            doc.add_object(field, object);
        }

        Ok(doc)
    }

    /// Adds `value` (scalar or array) to `field_name`, coerced to `kind`.
    fn add_value(
        &self,
        doc: &mut TantivyDocument,
        field_name: &str,
        kind: ValueKind,
        value: &Value,
    ) -> Result<()> {
        let field = self
            .wf_schema
            .field(field_name)
            .expect("caller passes a declared field name");
        let values: Vec<&Value> = match value {
            Value::Array(vs) => vs.iter().collect(),
            Value::Null => return Ok(()),
            single => vec![single],
        };
        for v in values {
            if v.is_null() {
                continue;
            }
            match kind {
                ValueKind::Text => doc.add_text(field, as_text(field_name, v)?),
                ValueKind::I64 => doc.add_i64(field, as_i64(field_name, v)?),
                ValueKind::F64 => doc.add_f64(field, as_f64(field_name, v)?),
                ValueKind::Date => doc.add_date(field, as_date(field_name, v)?),
            }
        }
        Ok(())
    }

    /// Parses a Solr `q` (or `fq`) query string into a Tantivy query.
    /// `*:*` is special-cased to `AllQuery`, matching Solr's match-all idiom;
    /// everything else goes through Tantivy's own query parser using
    /// `default_field` for bare (non-`field:value`) terms.
    pub fn parse_query(&self, query_str: &str, default_field_name: &str) -> Result<Box<dyn Query>> {
        if query_str.trim() == "*:*" {
            return Ok(Box::new(AllQuery));
        }
        let default_field = self
            .wf_schema
            .field(default_field_name)
            .ok_or_else(|| anyhow!("unknown default field `{default_field_name}`"))?;
        let parser = QueryParser::for_index(&self.index, vec![default_field]);
        let rewritten = self.rewrite_dynamic_fields(query_str);
        parser
            .parse_query(&rewritten)
            .map_err(|e| anyhow!("could not parse query `{query_str}`: {e}"))
    }

    /// Rewrites `name:value` to `_dynamic.name:value` (or `_dynamic_text.`) for
    /// every `name` that is not a declared field but does match a
    /// `[[dynamic_fields]]` pattern, since that is where its values are indexed.
    ///
    /// ponytail: a scan for `<ident>:` rather than a real query-syntax parser.
    /// Ceiling: a field name appearing inside a quoted phrase would also be
    /// rewritten. Revisit if/when the query layer grows its own parser (#8).
    fn rewrite_dynamic_fields(&self, query_str: &str) -> String {
        if self.wf_schema.dynamic_fields.is_empty() {
            return query_str.to_string();
        }
        let mut out = String::with_capacity(query_str.len());
        let mut ident = String::new();
        for ch in query_str.chars() {
            if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                ident.push(ch);
                continue;
            }
            if ch == ':'
                && !ident.is_empty()
                && !self.wf_schema.is_static(&ident)
                && let Some(rule) = self.wf_schema.match_dynamic(&ident)
            {
                out.push_str(self.wf_schema.dynamic_target(rule));
                out.push('.');
            }
            out.push_str(&ident);
            ident.clear();
            out.push(ch);
        }
        out.push_str(&ident);
        out
    }

    /// Runs `query`, intersects with every `filter_queries` match set, and
    /// returns the full match list (all docs, unpaginated) ordered per
    /// `AllScoredHits`: by `sort`'s clauses, then always by ascending doc order.
    /// An empty `sort` is the no-`sort` default — score descending, then
    /// ascending doc order.
    pub fn search(
        &self,
        query: &dyn Query,
        filter_queries: &[Box<dyn Query>],
        sort: &[SortClause],
    ) -> Result<Vec<(Score, DocAddress)>> {
        let searcher = self.reader.searcher();
        let mut hits = searcher.search(query, &AllScoredHits::new(sort.to_vec()))?;

        for fq in filter_queries {
            let allowed = searcher.search(fq.as_ref(), &DocSetCollector)?;
            hits.retain(|(_, addr)| allowed.contains(addr));
        }

        Ok(hits)
    }

    /// Renders the stored fields of `addr` as a Solr-shaped doc JSON object,
    /// restricted to `fl` (schema field names) if given. Unknown `fl` fields
    /// are silently dropped (findings fact 6); fields with no stored value
    /// are omitted entirely, never emitted as `null`/`[]`.
    pub fn render_doc(
        &self,
        addr: DocAddress,
        fl: Option<&[String]>,
        score: Option<Score>,
    ) -> Result<Value> {
        let searcher = self.reader.searcher();
        let doc: TantivyDocument = searcher.doc(addr)?;

        let wanted: Vec<&schema::FieldConfig> = self
            .wf_schema
            .fields
            .iter()
            .filter(|f| f.stored)
            .filter(|f| fl.is_none_or(|fl| fl.iter().any(|name| name == &f.name)))
            .collect();

        let mut out = Map::new();
        for field_config in wanted {
            let field = self.wf_schema.field(&field_config.name).unwrap();
            let values: Vec<Value> = doc.get_all(field).map(render_value).collect();
            if values.is_empty() {
                continue;
            }
            if field_config.multi_valued {
                out.insert(field_config.name.clone(), Value::Array(values));
            } else {
                out.insert(
                    field_config.name.clone(),
                    values.into_iter().next().expect("checked non-empty"),
                );
            }
        }

        // `score` only appears when `fl` explicitly names it, matching Solr:
        // requesting `fl=score` is what turns scoring output on at all, so a
        // caller passing a `Some(score)` without asking for it must still see
        // it omitted.
        //
        // ponytail: positioned here — after the schema-declared stored fields,
        // before the dynamic fields below — on an unverified assumption that
        // this matches Solr's own key order. No captured fixture actually
        // discriminates score-before-dynamic-fields from score-appended-last,
        // since no scored fixture (`select_term_scored.json`,
        // `select_quick_scored.json`) has a dynamic field. Finding 24
        // (`docs/solr-ref-findings.md`) is evidence pointing the other way:
        // Solr appends its own pseudo-fields (`_version_`, `_root_`) at the
        // end in `select_all.json`, and `score` is itself a pseudo-field, so
        // "appended last" may be the more faithful placement. Revisit if a
        // fixture with `fl=score,<dynamic field>` ever gets captured.
        if let Some(score) = score.filter(|_| fl.is_some_and(|fl| fl.iter().any(|f| f == "score")))
        {
            out.insert("score".to_string(), json!(score));
        }

        // Stored dynamic fields come back as top-level keys, the way Solr
        // returns its own dynamic fields — the `_dynamic*` containers are an
        // implementation detail and never appear in a response.
        for container in [schema::DYNAMIC_FIELD, schema::DYNAMIC_TEXT_FIELD] {
            let Some(field) = self.wf_schema.field(container) else {
                continue;
            };
            for value in doc.get_all(field) {
                let OwnedValue::Object(entries) = OwnedValue::from(value) else {
                    continue;
                };
                for (name, v) in entries {
                    let stored = self
                        .wf_schema
                        .match_dynamic(&name)
                        .is_some_and(|rule| rule.stored);
                    if !stored || fl.is_some_and(|fl| !fl.iter().any(|want| want == &name)) {
                        continue;
                    }
                    out.insert(name, serde_json::to_value(&v)?);
                }
            }
        }
        Ok(Value::Object(out))
    }

    /// Number of documents matching `query`. The counting primitive behind
    /// `facet.query`, `facet.missing` and each `facet.range` bucket, all of
    /// which are "how many docs also match this extra constraint?".
    pub fn count(&self, query: &dyn Query) -> Result<usize> {
        Ok(self.reader.searcher().search(query, &Count)?)
    }

    /// Every term in a **string** field's term dictionary, with how many of
    /// `query`'s matches carry it — Solr's `facet.field`.
    ///
    /// For a string field the counts do not come from the hit set: Solr
    /// enumerates the whole term dictionary, so a query matching one document
    /// still reports every other term at 0 (`facet_zero.json`,
    /// `facet_subset.json`). Tantivy does that with `min_doc_count: 0`,
    /// documented in tantivy 0.26.1's `aggregation/bucket/term_agg.rs:229` as
    /// *"When set to 0, this will return all terms in the field"* — which is
    /// also why this needs a `fast` (docValues) field and why a non-`fast` one is
    /// refused by the caller rather than silently answered with an empty array.
    ///
    /// For a numeric or date field this does **not** happen, and issue #24
    /// established that this is a match for Solr rather than a Wayfinder gap:
    /// Solr itself does not enumerate a term dictionary for a Points-based
    /// field (`facet_field_numeric_all.json`, `facet_field_date_all.json` list
    /// only hit-set values, and `facet_field_string_control_subset.json` on the
    /// same core/corpus/hit-set proves the difference is field-type-driven, not
    /// a capture artifact — Solr's own `responseHeader.warnings` even says why:
    /// *"...because field views is Points-based."*). In tantivy 0.26.1 the
    /// `min_doc_count == 0` term-dictionary stream that inserts the zero-count
    /// buckets sits inside the `ColumnType::Str` branch
    /// (`term_agg.rs:1024-1053`); the numeric/date/bool branches
    /// (`:1054-1112`) only map the entries the hit set actually produced, which
    /// is exactly Solr's own behaviour.
    ///
    /// Returned unsorted and unfiltered: `facet.mincount` / `facet.sort` /
    /// `facet.limit` are response-shaping concerns and live in `crate::facet`,
    /// which also needs an order-preserving sort key alongside the rendered
    /// term — see `FacetOrderKey`.
    pub fn term_facet(
        &self,
        field_name: &str,
        query: &dyn Query,
    ) -> Result<Vec<(String, FacetOrderKey, u64)>> {
        const AGG_NAME: &str = "wf_terms";

        let mut aggs = Aggregations::default();
        aggs.insert(
            AGG_NAME.to_string(),
            Aggregation {
                agg: AggregationVariants::Terms(TermsAggregation {
                    field: field_name.to_string(),
                    // `size` trims the final bucket list and `segment_size`
                    // caps the dictionary walk that fills in the zero-count
                    // terms, so both have to be at least the dictionary size
                    // for the enumeration to be complete.
                    //
                    // ponytail: a field with more distinct values than
                    // `MAX_FACET_TERMS` truncates (and trips Tantivy's own
                    // bucket limit). Solr has no such ceiling. Revisit with
                    // streaming/`facet.prefix` paging if a real corpus needs it.
                    size: Some(MAX_FACET_TERMS),
                    segment_size: Some(MAX_FACET_TERMS),
                    min_doc_count: Some(0),
                    ..TermsAggregation::default()
                }),
                sub_aggregation: Aggregations::default(),
            },
        );

        let collector = AggregationCollector::from_aggs(
            aggs,
            AggContextParams::new(
                AggregationLimitsGuard::default(),
                self.index.tokenizers().clone(),
            ),
        );
        let results = self.reader.searcher().search(query, &collector)?;

        let Some(AggregationResult::BucketResult(BucketResult::Terms { buckets, .. })) =
            results.0.get(AGG_NAME)
        else {
            return Err(anyhow!(
                "could not facet on field `{field_name}`: unexpected aggregation result"
            ));
        };

        let kind = self.wf_schema.value_kind(field_name);

        Ok(buckets
            .iter()
            .map(|bucket| {
                // The rendered term is exactly what shipped before: Tantivy's
                // own `key_as_string` for a `Bool` column (the only variant
                // `into_final_result` in tantivy 0.26.1's
                // `intermediate_agg_result.rs:728-734` ever sets it for — not
                // reachable today since `ValueKind` has no `Bool`, kept
                // defensively rather than as a live path), otherwise the raw
                // key. A **date** column's terms bucket is *not* `key_as_string`
                // at all: `term_agg.rs:1054-1060` inserts
                // `IntermediateKey::Str(format_date(val))` directly, so the key
                // Tantivy hands back is already `Key::Str(rfc3339)` and falls
                // into the plain `Key::Str` arm below.
                // A `pdouble`/`pfloat` column renders Java `Double.toString`
                // (finding 39): an integral double is `"5.0"`, never `"5"`.
                // Tantivy's own aggregation *normalises* an exactly-integral
                // double to a `U64`/`I64` key variant
                // (`NumericalValue::normalize`, `term_agg.rs:1096-1109`), so
                // this decision cannot be driven by the bucket key's variant
                // or value — only the schema's own declared `ValueKind::F64`
                // says "this column is a double/float", regardless of which
                // key variant a particular bucket happened to normalise to.
                // `views` (an `I64` column) must keep rendering `"5"` for the
                // exact same underlying value, which is exactly why this is
                // schema-driven and not variant- or value-sniffed.
                let term = if kind == Some(ValueKind::F64) {
                    let v = match &bucket.key {
                        Key::F64(v) => *v,
                        Key::I64(v) => *v as f64,
                        Key::U64(v) => *v as f64,
                        // Genuinely unreachable, not just quiet-fallback
                        // unreachable: the terms aggregation's `Key` variant
                        // is decided by the underlying Tantivy column's own
                        // type (`term_agg.rs`'s numeric vs. `ColumnType::Str`
                        // branches), and `kind == Some(ValueKind::F64)` here
                        // means this field was declared (and therefore
                        // added, via `add_f64_field`) as an `f64` column — a
                        // `Str` key can only come from a string/text column.
                        // Loud per the sibling guard in `facet.rs`'s
                        // `echo_range_end`, rather than a silent `0.0`.
                        Key::Str(_) => {
                            unreachable!("an F64-kind field's aggregation key was Key::Str")
                        }
                    };
                    render_double(v)
                } else {
                    match (&bucket.key_as_string, &bucket.key) {
                        (Some(s), _) => s.clone(),
                        (None, Key::Str(s)) => s.clone(),
                        (None, Key::I64(v)) => v.to_string(),
                        (None, Key::U64(v)) => v.to_string(),
                        (None, Key::F64(v)) => v.to_string(),
                    }
                };
                // The sort key is separate from the rendered term because the
                // string is lossy: `"15"` sorts before `"5"` lexically but
                // after it by value (issue #24). For a date field the term
                // *is* the rendered RFC3339 string (see above), so ordering by
                // it naively is ordering lexically, not chronologically —
                // which happens to coincide with chronological order for
                // fixed-width same-precision keys, but is still the wrong
                // thing to order by in general (e.g. a fraction rendered with
                // a different number of digits, or two precisions mixed).
                // Parsing the term back into an exact instant and sorting by
                // that removes the dependency on rendering shape entirely —
                // it does not merely "remove the dependency on precision":
                // this issue's own millisecond-precision fixtures resolve
                // well inside the ~200ns an `f64`-seconds key (the previous
                // carrier) can distinguish near a 2020s epoch, so the
                // ms-ordering fixtures alone would not have caught a
                // precision-loss regression there — nanoseconds-since-epoch
                // in an `i128` carrier is exact instead of merely "precise
                // enough for the corpus captured so far".
                let order = if kind == Some(ValueKind::Date) {
                    match OffsetDateTime::parse(&term, &Rfc3339) {
                        Ok(dt) => FacetOrderKey::Nanos(dt.unix_timestamp_nanos()),
                        // Should not happen — `term` came from Tantivy's own
                        // `format_date`, which always emits RFC3339 — but fall
                        // back to the (still correct for this corpus) lexical
                        // order rather than panicking or dropping the bucket.
                        Err(_) => FacetOrderKey::Str(term.clone()),
                    }
                } else {
                    FacetOrderKey::from(&bucket.key)
                };
                (term, order, bucket.doc_count)
            })
            .collect())
    }
}

/// An order-preserving sort key for one facet-term bucket, carried alongside
/// the rendered `String` term so `facet::facet_fields` can order numeric/date
/// terms by *value* (issue #24) while string terms keep byte-lexical order.
/// The rendered string is lossy — `"15"` sorts before `"5"` — so the fix has
/// to happen while the key still has its type, which is here, not after
/// `term_facet` has already collapsed it.
#[derive(Clone)]
pub enum FacetOrderKey {
    Str(String),
    I64(i64),
    U64(u64),
    F64(f64),
    /// A date bucket's sort key: nanoseconds since the Unix epoch,
    /// reconstructed by parsing the bucket's rendered RFC3339 term back into
    /// an instant (`term_facet`). `i128` because `OffsetDateTime::
    /// unix_timestamp_nanos` is (year range aside) exact; an earlier version
    /// of this carried seconds-plus-fractional-remainder as an `f64` instead,
    /// which is lossy far below one nanosecond once the seconds component is
    /// a real epoch value — see `term_facet`'s comment for why this issue's
    /// millisecond fixtures do not by themselves prove that lossiness away.
    Nanos(i128),
}

impl From<&Key> for FacetOrderKey {
    fn from(key: &Key) -> Self {
        match key {
            Key::Str(s) => FacetOrderKey::Str(s.clone()),
            Key::I64(v) => FacetOrderKey::I64(*v),
            Key::U64(v) => FacetOrderKey::U64(*v),
            Key::F64(v) => FacetOrderKey::F64(*v),
        }
    }
}

impl FacetOrderKey {
    /// A mismatched-variant pair is not just a theoretical guard: tantivy
    /// 0.26.1 calls `NumericalValue::normalize()` per bucket for an `f64`
    /// column (`term_agg.rs:1096-1109`), so one aggregation over a
    /// `float`/`double` field can freely mix `U64`/`I64` (exactly-integral
    /// values) and `F64` (fractional ones) across its own buckets. The
    /// fallback below is total and lossless for that case: `normalize()` only
    /// substitutes `U64`/`I64` when the value is exactly representable as one,
    /// so `as_f64` loses nothing converting back, and the comparator can never
    /// panic on `NaN` — `serde_json` numbers cannot represent `NaN`, so it
    /// never reaches a bucket key in the first place.
    pub fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use FacetOrderKey::*;
        match (self, other) {
            (Str(a), Str(b)) => a.cmp(b),
            (I64(a), I64(b)) => a.cmp(b),
            (U64(a), U64(b)) => a.cmp(b),
            (F64(a), F64(b)) => a.total_cmp(b),
            // `term_facet` only ever produces `Nanos` for a date column and
            // always for every bucket of it, so this is the exact comparison
            // that matters for dates — never the lossy `as_f64` fallback
            // below.
            (Nanos(a), Nanos(b)) => a.cmp(b),
            _ => self
                .as_f64()
                .partial_cmp(&other.as_f64())
                .unwrap_or(std::cmp::Ordering::Equal),
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            FacetOrderKey::Str(_) => 0.0,
            FacetOrderKey::I64(v) => *v as f64,
            FacetOrderKey::U64(v) => *v as f64,
            FacetOrderKey::F64(v) => *v,
            // Only reachable through a mismatched-variant pair, which
            // `term_facet` never actually produces for a date column (see
            // `cmp`'s `Nanos` arm) — kept total rather than panicking.
            FacetOrderKey::Nanos(v) => *v as f64,
        }
    }
}

/// Issue #34 (`fl=score`): unit-level pin for the `render_doc` contract
/// stage 2 must implement, independent of the hermetic differential
/// harness's fixture comparison. Solr renders each doc's BM25 score as a
/// float `score` key when (and only when) `fl` explicitly names it
/// (`select_term_scored.json`, `select_quick_scored.json`), positioned
/// immediately after the schema-declared stored fields and before any
/// dynamic-field keys (findings fact 6's ordering, extended).
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const SCHEMA_TOML: &str = r#"
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

[[dynamic_fields]]
pattern = "*_s"
type = "string"
stored = true
"#;

    /// Opens a fresh `CoreIndex` against `SCHEMA_TOML` in a throwaway temp
    /// dir. The `TempDir` guard must outlive the returned `CoreIndex`.
    fn open_test_index() -> (TempDir, CoreIndex) {
        let dir = TempDir::new().expect("create temp dir");
        let schema_path = dir.path().join("schema.toml");
        std::fs::write(&schema_path, SCHEMA_TOML).expect("write schema.toml");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let index = CoreIndex::open(&schema_path, &data_dir, &ServerConfig::default())
            .expect("open test index");
        (dir, index)
    }

    /// Indexes one doc, commits, and returns the doc's `(Score, DocAddress)`
    /// for a `body:quick` query — a real BM25 score, not a placeholder.
    fn indexed_scored_hit(index: &CoreIndex) -> (Score, DocAddress) {
        index
            .add_documents(&[
                json!({"id": "doc1", "body": "the quick brown fox", "extra_s": "tag"}),
            ])
            .expect("add_documents");
        index.commit().expect("commit");
        let query = index.parse_query("quick", "body").expect("parse_query");
        let hits = index
            .search(query.as_ref(), &[], &[])
            .expect("search should not fail");
        hits.into_iter()
            .next()
            .expect("the indexed doc must match `quick`")
    }

    #[test]
    fn render_doc_includes_score_when_fl_requests_it() {
        let (_dir, index) = open_test_index();
        let (score, addr) = indexed_scored_hit(&index);

        let fl = vec!["id".to_string(), "score".to_string()];
        let rendered = index
            .render_doc(addr, Some(&fl), Some(score))
            .expect("render_doc");

        let obj = rendered.as_object().expect("doc is a JSON object");
        assert_eq!(
            obj.get("score").and_then(Value::as_f64),
            Some(score as f64),
            "`score` must be the BM25 score passed in, rendered as a float"
        );
    }

    #[test]
    fn render_doc_omits_score_when_fl_does_not_request_it() {
        let (_dir, index) = open_test_index();
        let (score, addr) = indexed_scored_hit(&index);

        let fl = vec!["id".to_string()];
        let rendered = index
            .render_doc(addr, Some(&fl), Some(score))
            .expect("render_doc");

        let obj = rendered.as_object().expect("doc is a JSON object");
        assert!(
            !obj.contains_key("score"),
            "`score` must not appear unless `fl` names it explicitly, even when a score is available"
        );
    }

    #[test]
    fn render_doc_omits_score_when_fl_is_absent() {
        let (_dir, index) = open_test_index();
        let (score, addr) = indexed_scored_hit(&index);

        let rendered = index
            .render_doc(addr, None, Some(score))
            .expect("render_doc");

        let obj = rendered.as_object().expect("doc is a JSON object");
        assert!(
            !obj.contains_key("score"),
            "no `fl` means the default projection, which never includes `score`"
        );
    }

    #[test]
    fn render_doc_orders_score_after_stored_fields_and_before_dynamic_fields() {
        let (_dir, index) = open_test_index();
        let (score, addr) = indexed_scored_hit(&index);

        // `fl` lists `extra_s` (dynamic) and `score` before the schema-declared
        // stored fields, deliberately out of the expected output order, to pin
        // that key order is driven by schema position, not by `fl`'s order.
        let fl = vec![
            "extra_s".to_string(),
            "score".to_string(),
            "body".to_string(),
            "id".to_string(),
        ];
        let rendered = index
            .render_doc(addr, Some(&fl), Some(score))
            .expect("render_doc");

        let obj = rendered.as_object().expect("doc is a JSON object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["id", "body", "score", "extra_s"],
            "`score` must sit immediately after the schema-declared stored \
             fields and before any dynamic-field keys"
        );
    }
}
