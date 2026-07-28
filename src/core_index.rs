//! The Tantivy-backed core: build, index, and search a single Wayfinder
//! core (PRD open question 1 — single-core-per-process, so there's exactly
//! one of these per running `app()`).

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use tantivy::collector::DocSetCollector;
use tantivy::query::{AllQuery, Query, QueryParser};
use tantivy::schema::OwnedValue;
use tantivy::schema::Value as _;
use tantivy::time::OffsetDateTime;
use tantivy::time::format_description::well_known::Rfc3339;
use tantivy::{
    DateTime, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument,
};

use crate::collector::AllScoredHits;
use crate::config::ServerConfig;
use crate::schema::{self, ValueKind, WayfinderSchema};

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
    /// returns the full match list (all docs, unpaginated) sorted per
    /// `AllScoredHits` — score descending, then ascending doc order.
    pub fn search(
        &self,
        query: &dyn Query,
        filter_queries: &[Box<dyn Query>],
    ) -> Result<Vec<(Score, DocAddress)>> {
        let searcher = self.reader.searcher();
        let mut hits = searcher.search(query, &AllScoredHits)?;

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
    pub fn render_doc(&self, addr: DocAddress, fl: Option<&[String]>) -> Result<Value> {
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

    /// Counts occurrences of each value of `field_name` across `hits`
    /// (Solr's `facet.field`, default sort — count descending, term
    /// ascending on ties). `field_name` must be a stored field; the tracer
    /// bullet reads facet counts from stored values rather than the fast
    /// field, which is an implementation simplification (see handoff notes).
    pub fn facet_counts(
        &self,
        field_name: &str,
        hits: &[(Score, DocAddress)],
    ) -> Result<Vec<(String, i64)>> {
        let field = self
            .wf_schema
            .field(field_name)
            .ok_or_else(|| anyhow!("unknown facet field `{field_name}`"))?;
        let searcher = self.reader.searcher();

        let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for (_, addr) in hits {
            let doc: TantivyDocument = searcher.doc(*addr)?;
            for value in doc.get_all(field).filter_map(|v| v.as_str()) {
                *counts.entry(value.to_string()).or_insert(0) += 1;
            }
        }

        let mut counted: Vec<(String, i64)> = counts.into_iter().collect();
        counted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(counted)
    }
}
