//! The Tantivy-backed core: build, index, and search a single Wayfinder
//! core (PRD open question 1 — single-core-per-process, so there's exactly
//! one of these per running `app()`).

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use tantivy::collector::DocSetCollector;
use tantivy::query::{AllQuery, Query, QueryParser};
use tantivy::schema::Value as _;
use tantivy::{DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument};

use crate::collector::AllScoredHits;
use crate::schema::{self, WayfinderSchema};

pub struct CoreIndex {
    pub wf_schema: WayfinderSchema,
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
}

impl CoreIndex {
    pub fn open(schema_path: &Path, data_dir: &Path) -> Result<CoreIndex> {
        let wf_schema = schema::load(schema_path)?;
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let index = Index::create_in_dir(data_dir, wf_schema.tantivy_schema.clone())
            .or_else(|_| Index::open_in_dir(data_dir))
            .context("opening/creating Tantivy index")?;

        // Single-threaded writer: with a small, single-commit corpus this
        // gives deterministic ascending doc-id allocation, which is what the
        // tie-break in `AllScoredHits` relies on to match Solr's observed
        // (insertion) order on equally-scored matches.
        let writer: IndexWriter = index
            .writer_with_num_threads(1, 32_000_000)
            .context("creating index writer")?;

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
            let field = self
                .wf_schema
                .field(&field_config.name)
                .expect("field_config name always has a matching schema field");
            match value {
                Value::String(s) => doc.add_text(field, s),
                Value::Array(values) => {
                    for v in values {
                        let s = v
                            .as_str()
                            .ok_or_else(|| anyhow!("field `{}` array values must be strings", field_config.name))?;
                        doc.add_text(field, s);
                    }
                }
                Value::Null => {}
                other => {
                    return Err(anyhow!(
                        "field `{}` has unsupported JSON value {other}",
                        field_config.name
                    ));
                }
            }
        }
        Ok(doc)
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
        parser
            .parse_query(query_str)
            .map_err(|e| anyhow!("could not parse query `{query_str}`: {e}"))
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
            let values: Vec<&str> = doc.get_all(field).filter_map(|v| v.as_str()).collect();
            if values.is_empty() {
                continue;
            }
            if field_config.multi_valued {
                out.insert(field_config.name.clone(), json!(values));
            } else {
                out.insert(field_config.name.clone(), json!(values[0]));
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
