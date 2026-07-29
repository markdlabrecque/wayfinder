//! The Tantivy-backed core: build, index, and search a single Wayfinder
//! core (PRD open question 1 — single-core-per-process, so there's exactly
//! one of these per running `app()`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use tantivy::aggregation::agg_req::{Aggregation, AggregationVariants, Aggregations};
use tantivy::aggregation::agg_result::{AggregationResult, BucketResult, MetricResult};
use tantivy::aggregation::bucket::TermsAggregation;
use tantivy::aggregation::metric::{ExtendedStats, ExtendedStatsAggregation};
use tantivy::aggregation::{
    AggContextParams, AggregationCollector, AggregationLimitsGuard, DEFAULT_BUCKET_LIMIT, Key,
};
use tantivy::collector::{Count, DocSetCollector};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, EmptyQuery, ExistsQuery, Occur, Query, QueryParser,
    RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, OwnedValue};
use tantivy::snippet::SnippetGenerator;
use tantivy::time::OffsetDateTime;
use tantivy::time::format_description::well_known::Rfc3339;
use tantivy::{
    DateTime, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument,
    Term,
};

use crate::collector::{AllScoredHits, SortClause};
use crate::config::ServerConfig;
use crate::query::{self, QueryError};
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

/// Flattens an incoming JSON field value (scalar, `null`, or array) into a
/// list of non-null owned values, for `build_document`'s combined
/// own-value-plus-copy-field single-valued check (finding 48e). `null` (bare
/// or inside an array) contributes nothing — mirrors the pre-issue-#9
/// `add_value`'s null handling exactly, just applied before combination
/// rather than during the write.
/// The effective string value of `obj`'s `unique_key` field, for the
/// overwrite-delete step in `add_documents` — a bare string, or a
/// one-element array of one (mirroring `flatten_values`'/finding 48e's
/// single-element-array unwrap, since `id: ["u1"]` is exactly as valid a
/// uniqueKey value as `id: "u1"`). Anything else (no value, a multi-element
/// array, a non-string scalar) returns `None`: schema load time guarantees
/// `unique_key` is string-typed (review round 1, five-minute item), so a doc
/// whose value doesn't resolve to a single string here will fail the same
/// way in `build_document`'s own single-valued/type checks a few lines
/// later — skipping the overwrite delete for it is harmless because the add
/// itself is about to be rejected.
fn unique_key_value<'a>(obj: &'a Map<String, Value>, unique_key: &str) -> Option<&'a str> {
    match obj.get(unique_key)? {
        Value::String(s) => Some(s.as_str()),
        Value::Array(vs) => match vs.as_slice() {
            [Value::String(s)] => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn flatten_values(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(vs) => vs.iter().filter(|v| !v.is_null()).cloned().collect(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
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

/// The writer/reader machinery shared between `CoreIndex` and its background
/// commit-scheduler thread (issue #9). Holding both behind one `Arc` is what
/// lets the scheduler fire a commit — through the same writer `Mutex`, then a
/// reader reload — without `CoreIndex` itself needing a `&mut self` anywhere.
struct CommitState {
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    /// Uncommitted docs added since the last commit (of any kind — manual,
    /// scheduled, or autocommit). Drives `autocommit_max_docs`; deletes are
    /// not counted, since Solr's own `autoCommit` thresholds are documented
    /// in terms of added docs, and no fixture pins delete-driven autocommit.
    pending_docs: AtomicU64,
    /// The next scheduled commit deadline, if any — set by `commitWithin` and
    /// `autocommit_max_time`, cleared by any commit (manual or fired). `None`
    /// means "no pending schedule".
    deadline: Mutex<Option<Instant>>,
    condvar: Condvar,
}

impl CommitState {
    /// Hard-commits pending writes, reloads the reader, resets the pending-doc
    /// counter, and absorbs (clears) any pending scheduled deadline — a manual
    /// commit makes a later scheduled one redundant, per the task spec's
    /// "a manual commit cancels/absorbs pending schedules".
    fn commit(&self) -> Result<()> {
        *self
            .deadline
            .lock()
            .expect("commit deadline mutex poisoned") = None;
        let mut writer = self.writer.lock().expect("index writer mutex poisoned");
        writer.commit()?;
        self.reader.reload()?;
        self.pending_docs.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Arms (or tightens) the scheduled-commit deadline. Per the task spec, a
    /// pending deadline is only ever moved EARLIER — a later request asking
    /// for a longer `commitWithin` must not push an already-armed sooner
    /// commit back out.
    fn schedule(&self, at: Instant) {
        let mut deadline = self
            .deadline
            .lock()
            .expect("commit deadline mutex poisoned");
        if deadline.is_none_or(|current| at < current) {
            *deadline = Some(at);
            self.condvar.notify_one();
        }
    }
}

/// How long the background scheduler thread sleeps with nothing due, and how
/// often it re-checks whether `CoreIndex` (and so this `CommitState`) has been
/// dropped. Bounds shutdown latency without a busy loop: the thread blocks on
/// the condvar for at most this long per iteration, woken early by
/// `CommitState::schedule`'s `notify_one` whenever a deadline moves earlier.
const SCHEDULER_IDLE_POLL: Duration = Duration::from_millis(200);

/// The commit-scheduler background thread body (issue #9): one mechanism
/// serving both `commitWithin` and config autocommit's `autocommit_max_time`,
/// since both are just "commit at this deadline". Holds only a `Weak` handle
/// so it exits (within `SCHEDULER_IDLE_POLL`) once the owning `CoreIndex`
/// drops, rather than leaking a thread per test app.
///
/// Not a busy loop: every iteration either fires a due commit or blocks on
/// the condvar for the lesser of "time to the deadline" and
/// `SCHEDULER_IDLE_POLL`, waking early on `notify_one` when a deadline moves
/// earlier.
fn run_scheduler(weak: std::sync::Weak<CommitState>) {
    loop {
        let Some(state) = weak.upgrade() else { return };
        let mut deadline = state
            .deadline
            .lock()
            .expect("commit deadline mutex poisoned");
        let now = Instant::now();
        if matches!(*deadline, Some(d) if d <= now) {
            *deadline = None;
            drop(deadline);
            // Best-effort: a background autocommit has no request to report a
            // failure to. A poisoned writer mutex would already have panicked
            // elsewhere; a transient commit error here just leaves the next
            // scheduled/manual commit to retry.
            let _ = state.commit();
            continue;
        }
        let wait = match *deadline {
            Some(d) => (d - now).min(SCHEDULER_IDLE_POLL),
            None => SCHEDULER_IDLE_POLL,
        };
        let _ = state
            .condvar
            .wait_timeout(deadline, wait)
            .expect("commit deadline condvar poisoned");
    }
}

/// One `Should`-joined `TermQuery` per entry in `terms` — an `EmptyQuery`
/// when there are none, never an empty (and therefore ill-defined)
/// `BooleanQuery`.
fn terms_to_should_query(field: Field, terms: Vec<String>) -> Box<dyn Query> {
    if terms.is_empty() {
        return Box::new(EmptyQuery);
    }
    let clauses: Vec<(Occur, Box<dyn Query>)> = terms
        .into_iter()
        .map(|text| {
            let term = Term::from_field_text(field, &text);
            let query: Box<dyn Query> = Box::new(TermQuery::new(
                term,
                IndexRecordOption::WithFreqsAndPositions,
            ));
            (Occur::Should, query)
        })
        .collect();
    Box::new(BooleanQuery::new(clauses))
}

/// The field name a `UserInputLeaf` targets, if any (`All` never does — a
/// bare `*` alone, distinct from the fielded `Exists` idiom).
fn leaf_field_name(leaf: &tantivy::query_grammar::UserInputLeaf) -> Option<&str> {
    use tantivy::query_grammar::UserInputLeaf;
    match leaf {
        UserInputLeaf::Literal(l) => l.field_name.as_deref(),
        UserInputLeaf::Exists { field } => Some(field.as_str()),
        UserInputLeaf::Range { field, .. } => field.as_deref(),
        UserInputLeaf::Set { field, .. } => field.as_deref(),
        UserInputLeaf::Regex { field, .. } => field.as_deref(),
        UserInputLeaf::All => None,
    }
}

/// True when `field_name` is a `[[dynamic_fields]]` catch-all JSON path
/// (`_dynamic.<name>`/`_dynamic_text.<name>`) rather than a declared field —
/// exactly and only what `rewrite_dynamic_fields` produces, so this is a
/// reliable signal without needing the pre-rewrite identifier (which the
/// grammar has already discarded by the time a leaf reaches `build_leaf`).
fn is_dynamic_container_field(field_name: &str) -> bool {
    [schema::DYNAMIC_FIELD, schema::DYNAMIC_TEXT_FIELD]
        .iter()
        .any(|container| {
            field_name.starts_with(*container) && field_name[container.len()..].starts_with('.')
        })
}

pub struct CoreIndex {
    pub wf_schema: WayfinderSchema,
    index: Index,
    state: Arc<CommitState>,
    reader: IndexReader,
    /// From `ServerConfig.commit` (parsed since #12, consumed here since #9).
    /// The Nth uncommitted doc triggers a commit.
    autocommit_max_docs: Option<u64>,
    /// The first uncommitted doc since the last commit arms a deadline this
    /// many ms out.
    autocommit_max_time_ms: Option<u64>,
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

        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("creating index reader")?;

        let state = Arc::new(CommitState {
            writer: Mutex::new(writer),
            reader: reader.clone(),
            pending_docs: AtomicU64::new(0),
            deadline: Mutex::new(None),
            condvar: Condvar::new(),
        });
        // A `Weak` handle, not a strong one: the scheduler thread must not be
        // the thing keeping `CommitState` (and so the writer/reader) alive —
        // it should exit once `CoreIndex` itself drops, not leak forever.
        let weak = Arc::downgrade(&state);
        thread::spawn(move || run_scheduler(weak));

        Ok(CoreIndex {
            wf_schema,
            index,
            state,
            reader,
            autocommit_max_docs: config.commit.autocommit_max_docs,
            autocommit_max_time_ms: config.commit.autocommit_max_time,
        })
    }

    /// Adds documents from a Solr-style JSON array-of-docs body. Returns the
    /// number of documents added.
    ///
    /// `overwrite` mirrors Solr's own default: replace-by-uniqueKey before
    /// each add unless the caller passed `overwrite=false` (finding 48a/b).
    /// The uniqueKey is `wf_schema.core.unique_key` — schema load time now
    /// rejects a non-string-typed `core.unique_key` outright (review round 1,
    /// five-minute item), so resolving it as a text term below is a real
    /// guarantee, not just "a declared field".
    ///
    /// Each successfully-added doc bumps the shared pending-doc counter (and,
    /// on crossing `autocommit_max_docs` or arming the first
    /// `autocommit_max_time` deadline, triggers the corresponding action)
    /// immediately after `writer.add_document` — not batched to the end of
    /// the loop. The counter update and its autocommit/deadline
    /// consequences run even when a LATER doc in the same batch fails
    /// validation (review round 1 must-fix): the loop below records its
    /// `Result` instead of returning `?` straight out of the function, so
    /// `arm_deadline`/`should_autocommit` are always acted on for whatever
    /// prefix of the batch actually reached the writer before the error is
    /// propagated to the caller. Previously the early `?` return skipped
    /// that follow-through entirely — the doc that landed stayed pending
    /// with `autocommit_max_time` never arming a deadline for it, and
    /// because the pending counter was already nonzero, no LATER add ever
    /// saw `prior == 0` again either, so `autocommit_max_time` alone could
    /// never fire again until a manual commit.
    pub fn add_documents(&self, docs: &[Value], overwrite: bool) -> Result<usize> {
        let unique_key_field = self
            .wf_schema
            .field(&self.wf_schema.core.unique_key)
            .expect("core.unique_key is validated to be a declared, string-typed field at schema load time");

        let mut arm_deadline = false;
        let mut should_autocommit = false;
        let added: Result<usize> = (|| {
            let writer = self
                .state
                .writer
                .lock()
                .expect("index writer mutex poisoned");
            let mut added = 0usize;
            for doc in docs {
                let obj = doc.as_object().ok_or_else(|| {
                    anyhow!("each document in the update body must be a JSON object")
                })?;
                if overwrite
                    && let Some(id) = unique_key_value(obj, &self.wf_schema.core.unique_key)
                {
                    writer.delete_term(Term::from_field_text(unique_key_field, id));
                }
                let tantivy_doc = self.build_document(obj)?;
                writer.add_document(tantivy_doc)?;
                added += 1;

                let prior = self.state.pending_docs.fetch_add(1, Ordering::SeqCst);
                let now_pending = prior + 1;
                if prior == 0 {
                    arm_deadline = true;
                }
                if self
                    .autocommit_max_docs
                    .is_some_and(|max| now_pending >= max)
                {
                    should_autocommit = true;
                }
            }
            Ok(added)
        })();

        // Run on BOTH the `Ok` and `Err` paths, before propagating any
        // error: every doc that reached `writer.add_document` above is
        // genuinely pending regardless of whether a later doc in the same
        // batch failed, and the writer lock is already released by now, so
        // `commit()` (which locks the same mutex) cannot deadlock here.
        if arm_deadline && let Some(ms) = self.autocommit_max_time_ms {
            self.schedule_commit(ms);
        }
        if should_autocommit {
            self.commit()?;
        }
        added
    }

    /// Deletes every document sharing any of `ids` on the uniqueKey term —
    /// Solr's delete-by-id, which removes ALL docs with that key, including
    /// `overwrite=false` duplicates (finding 48c). A delete of an id matching
    /// nothing is not an error (finding 46's `update_delete_id_missing`).
    pub fn delete_by_ids(&self, ids: &[String]) -> Result<()> {
        let unique_key_field = self.wf_schema.field(&self.wf_schema.core.unique_key).expect(
            "core.unique_key is validated to be a declared, string-typed field at schema load time",
        );
        let writer = self
            .state
            .writer
            .lock()
            .expect("index writer mutex poisoned");
        for id in ids {
            writer.delete_term(Term::from_field_text(unique_key_field, id));
        }
        Ok(())
    }

    /// Deletes every document matching `query`, parsed through the SAME
    /// `parse_query` `/select` uses — finding 48d pins that delete-by-query is
    /// analyzed identically (`body:lazy` on a `text_en` field matches multiple
    /// docs the same way a `/select?q=body:lazy` would).
    pub fn delete_by_query(&self, query: &str, default_field_name: &str) -> Result<()> {
        let parsed = self.parse_query(query, default_field_name)?;
        let writer = self
            .state
            .writer
            .lock()
            .expect("index writer mutex poisoned");
        writer.delete_query(parsed)?;
        Ok(())
    }

    /// Schedules a commit at most `within_ms` out (`commitWithin`), through
    /// the same background scheduler `autocommit_max_time` uses. Per the task
    /// spec, `commitWithin` is a HARD commit + reader reload in Wayfinder —
    /// Tantivy has no in-memory-searchable segment for a "soft" commit to
    /// leave uncommitted-but-visible, so there is no less-durable state to
    /// schedule into. Wire-visible behaviour (the doc becomes searchable once
    /// the window elapses) matches Solr; only the internal durability differs
    /// (stronger here, never weaker).
    pub fn schedule_commit(&self, within_ms: u64) {
        self.state
            .schedule(Instant::now() + Duration::from_millis(within_ms));
    }

    /// Hard-commits pending writes and reloads the reader, making them
    /// visible to subsequent searches. Used for the explicit `commit=true`
    /// param and — per the task spec's honest divergence note — also for
    /// `softCommit=true`: Tantivy has no in-memory-searchable segment, so a
    /// reader reload alone cannot reproduce Solr's soft-commit visibility
    /// (`update_select_softcommit_visible.json`); a real (hard) commit is the
    /// only way Wayfinder can make wire-visible behaviour match. The
    /// difference is durability only (stronger, never weaker) — never fewer
    /// documents visible than Solr would show.
    pub fn commit(&self) -> Result<()> {
        self.state.commit()
    }

    fn build_document(&self, obj: &Map<String, Value>) -> Result<TantivyDocument> {
        let mut doc = TantivyDocument::default();
        // Pass 1: collect every value destined for each declared field —
        // its own JSON value AND whatever `[[copy_fields]]` copies in from a
        // source field — before writing anything. Single-valued enforcement
        // (pass 2 below) needs the COMBINED count: a copy-field landing a
        // second value in a single-valued destination is a 400 (finding 48e)
        // even though neither the destination's own value nor the copied one
        // is individually more than one value.
        let mut pending: HashMap<&str, Vec<Value>> = HashMap::new();
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
            pending
                .entry(field_config.name.as_str())
                .or_default()
                .extend(flatten_values(value));

            // copy_fields: index-time only, so a copy destination sees the
            // source's raw value and analyzes it with its own field type.
            for dest in self.wf_schema.copy_dests(&field_config.name) {
                pending
                    .entry(dest)
                    .or_default()
                    .extend(flatten_values(value));
            }
        }

        // Pass 2: enforce single-valuedness on the combined list, then write.
        // A JSON array with exactly one element unwraps to a scalar rather
        // than erroring (finding 48e) — `flatten_values` already produced a
        // one-element `Vec` for that case, so there is nothing extra to do
        // for it here; the write path below is identical either way.
        for field_config in &self.wf_schema.fields {
            let Some(values) = pending.get(field_config.name.as_str()) else {
                continue;
            };
            if values.is_empty() {
                continue;
            }
            if !field_config.multi_valued && values.len() > 1 {
                return Err(anyhow!(
                    "multiple values encountered for non multiValued field {}: {:?}",
                    field_config.name,
                    values
                ));
            }
            let kind = self
                .wf_schema
                .value_kind(&field_config.name)
                .expect("a declared field always has a value kind");
            self.add_values(&mut doc, &field_config.name, kind, values)?;
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

    /// Writes an already-validated, already-flattened value list to
    /// `field_name`, coerced to `kind`. Single-valuedness is enforced by the
    /// caller (`build_document`'s pass 2) before this is reached.
    fn add_values(
        &self,
        doc: &mut TantivyDocument,
        field_name: &str,
        kind: ValueKind,
        values: &[Value],
    ) -> Result<()> {
        let field = self
            .wf_schema
            .field(field_name)
            .expect("caller passes a declared field name");
        for v in values {
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
    /// `*:*` is special-cased to `AllQuery`, matching Solr's match-all idiom.
    ///
    /// Everything else is parsed by `tantivy::query_grammar::parse_query` —
    /// the exact same grammar entry point Tantivy's own `QueryParser` uses
    /// internally — and then walked leaf by leaf via `build_ast`/
    /// `build_leaf`: fuzzy (`term~`, `term~N`), wildcard/prefix (`te?t`,
    /// `test*`, `*mals`, `field:*`) and regex (`/pattern/`) are constructs
    /// Tantivy's own `QueryParser` does not implement at all (see
    /// `crate::query`'s module doc for exactly how it silently mangles each
    /// one instead of erroring), so those leaves are built directly; every
    /// other leaf (plain terms/phrases, ranges, sets) is delegated to
    /// Tantivy's own per-leaf conversion, and the boolean `Clause`/`Boost`
    /// structure joining them is walked, not reparsed — round-1 review
    /// established that a whole-query-string special case cannot tell a
    /// compound query (`category:animals OR body:laz*`) apart from a bare
    /// atomic one, so detection now happens per leaf, after the grammar has
    /// already done the hard part of splitting the query into leaves.
    pub fn parse_query(&self, query_str: &str, default_field_name: &str) -> Result<Box<dyn Query>> {
        if query_str.trim() == "*:*" {
            return Ok(Box::new(AllQuery));
        }
        let default_field = self
            .wf_schema
            .field(default_field_name)
            .ok_or_else(|| anyhow!("unknown default field `{default_field_name}`"))?;
        let rewritten = self.rewrite_dynamic_fields(&self.rewrite_wildcard_subclause(query_str));
        let user_ast = tantivy::query_grammar::parse_query(&rewritten)
            .map_err(|_| anyhow!("could not parse query `{query_str}`"))?;
        let parser = QueryParser::for_index(&self.index, vec![default_field]);
        self.build_ast(user_ast, &parser, default_field_name)
            .map_err(anyhow::Error::from)
    }

    /// Rewrites an embedded `*:*` sub-clause (e.g. `*:* AND lazy`) to a bare
    /// `*`, which tantivy's own query grammar parses as `UserInputLeaf::All`
    /// (the same "match everything" leaf its own bare-`*` support produces),
    /// rather than as an `Exists { field: "*" }` leaf.
    ///
    /// The whole-string `*:*` case is already handled by the `AllQuery`
    /// special-case above and never reaches this function.
    ///
    /// Escalation note (resolved): the spec's original proposal was to
    /// rewrite to `<uniqueKey>:*` (e.g. `id:*`). That does not work: tantivy
    /// 0.26's own grammar parses any `field:*` as `UserInputLeaf::Exists`,
    /// and `QueryParser::compute_logical_ast_for_leaf` in
    /// `tantivy-0.26.1/src/query/query_parser/query_parser.rs` unconditionally
    /// rejects `Exists` leaves with `QueryParserError::UnsupportedQuery`
    /// ("Range query need to target a specific field.") regardless of
    /// whether a field is present — `Exists` support was apparently never
    /// wired up in `QueryParser`, so `id:*` alone fails to parse (verified
    /// empirically: `index.parse_query("id:*", "body")` errors). A bare `*`
    /// sub-clause, by contrast, is handled by the grammar's `leaf()`
    /// combinator (checked before falling into field:value parsing) and
    /// resolves through `UserInputLeaf::All` -> `LogicalLiteral::All`, which
    /// `QueryParser` does support, with no field required — this is Wayfinder's
    /// own real match-everything leaf resolving through the exact same path
    /// its own whole-string `*:*` fixtures were captured against, just
    /// reached via a sub-clause instead of the whole string.
    ///
    /// ponytail: a substring replace, not a syntax-aware rewrite (same
    /// ceiling as `rewrite_dynamic_fields` below) — it would also fire inside
    /// a quoted phrase that literally contains the text `*:*`. Acceptable for
    /// now; no fixture exercises that case. Revisit if/when the query layer
    /// grows its own parser (#8).
    fn rewrite_wildcard_subclause(&self, query_str: &str) -> String {
        query_str.replace("*:*", "*")
    }

    /// Recursively converts a `tantivy::query_grammar::UserInputAst` into a
    /// real `Query`, handling `Clause`/`Boost` composition itself and
    /// deferring each `Leaf` to `build_leaf`.
    ///
    /// Round-2 review's regression: an all-`MustNot` `Clause` (`-lazy`,
    /// `NOT lazy`, `-lazy -dog`) must answer Solr's implicit complement —
    /// every doc except the excluded ones — not a silent 200/0. A plain
    /// `BooleanQuery` of only exclusions has nothing to exclude *from*, so
    /// once every clause at *this* level is confirmed `MustNot`, an extra
    /// `(Should, AllQuery)` clause is pushed to supply that "everything"
    /// (mirroring tantivy's own `QueryParser::make_non_negative`, which does
    /// the same for its `LogicalAst`). This check is per-`Clause`-node, not
    /// global: `lazy AND NOT dog` parses as `(+lazy +(-dog))` — an *inner*
    /// single-clause `(-dog)` that is all-negative on its own, nested inside
    /// an *outer* clause that also carries the positive `+lazy` and so is
    /// never all-negative itself. Recursion alone gets this right: the
    /// inner `(-dog)` clause earns the `AllQuery` companion when *it* is
    /// built (becoming "every doc except dog"), and that combines with
    /// `+lazy` at the outer `Must`/`Must` level exactly as a normal AND
    /// would — no special-casing needed at the outer level, and nothing here
    /// ever injects `AllQuery` into a clause that already carries a
    /// non-`MustNot` sibling.
    fn build_ast(
        &self,
        ast: tantivy::query_grammar::UserInputAst,
        parser: &QueryParser,
        default_field_name: &str,
    ) -> Result<Box<dyn Query>, QueryError> {
        use tantivy::query_grammar::UserInputAst;
        match ast {
            UserInputAst::Clause(subqueries) => {
                let mut clauses = Vec::with_capacity(subqueries.len());
                for (occur_opt, sub) in subqueries {
                    let occur = occur_opt.unwrap_or(Occur::Should);
                    clauses.push((occur, self.build_ast(sub, parser, default_field_name)?));
                }
                if clauses.is_empty() {
                    return Ok(Box::new(EmptyQuery));
                }
                if clauses.iter().all(|(occur, _)| *occur == Occur::MustNot) {
                    clauses.push((Occur::Should, Box::new(AllQuery)));
                }
                Ok(Box::new(BooleanQuery::new(clauses)))
            }
            UserInputAst::Boost(inner, boost) => {
                let built = self.build_ast(*inner, parser, default_field_name)?;
                Ok(Box::new(BoostQuery::new(built, boost.into_inner() as f32)))
            }
            UserInputAst::Leaf(leaf) => self.build_leaf(*leaf, parser, default_field_name),
        }
    }

    /// Builds one grammar leaf. A bare (`Delimiter::None`, no slop/prefix)
    /// literal is classified by `query::classify_literal` for fuzzy/wildcard/
    /// unclosed-regex; `UserInputLeaf::Exists` and an all-`Unbounded`
    /// `UserInputLeaf::Range` (`[* TO *]`) both become the field-exists idiom
    /// (finding 43/44); `UserInputLeaf::Regex` (a *closed* `/pattern/`,
    /// which the grammar itself already parsed out of a literal) becomes a
    /// `RegexQuery`. A leaf targeting a `[[dynamic_fields]]`-backed catch-all
    /// path (its field starts with `_dynamic.`/`_dynamic_text.` —
    /// `rewrite_dynamic_fields` already ran, so that prefix is exactly and
    /// only how a dynamic-field reference looks by the time the grammar
    /// sees it) is never special-cased here: this module's field lookups
    /// only know declared fields, not a JSON sub-path within a catch-all
    /// container, so those go straight to Tantivy's own per-leaf conversion,
    /// which already resolves JSON paths correctly (round-1 review item 3 —
    /// `count_i:*`/`count_i:1*`/`count_i:7~1` on a `*_i` dynamic rule must
    /// not hard-error just because "count_i" is not itself a declared
    /// field). Everything else delegates to `parser.
    /// build_query_from_user_input_ast` on a single-leaf sub-AST, reusing
    /// Tantivy's own (already-correct-against-the-fixtures) conversion for
    /// plain terms/phrases/ranges/sets.
    fn build_leaf(
        &self,
        leaf: tantivy::query_grammar::UserInputLeaf,
        parser: &QueryParser,
        default_field_name: &str,
    ) -> Result<Box<dyn Query>, QueryError> {
        use tantivy::query_grammar::{UserInputAst, UserInputBound, UserInputLeaf};

        if leaf_field_name(&leaf).is_some_and(is_dynamic_container_field) {
            return parser
                .build_query_from_user_input_ast(UserInputAst::Leaf(Box::new(leaf)))
                .map(|q| q as Box<dyn Query>)
                .map_err(|e| QueryError::Syntax(e.to_string()));
        }

        if query::leaf_is_special_literal(&leaf) {
            let UserInputLeaf::Literal(literal) = &leaf else {
                unreachable!("leaf_is_special_literal only returns true for a Literal")
            };
            let field_name = literal.field_name.as_deref().unwrap_or(default_field_name);
            match query::classify_literal(&literal.phrase) {
                query::LiteralKind::Fuzzy { term, distance_raw } => {
                    return self.build_fuzzy(field_name, &term, &distance_raw);
                }
                query::LiteralKind::Wildcard { glob } => {
                    return self.build_wildcard(field_name, &glob);
                }
                query::LiteralKind::RegexUnclosed => {
                    return Err(QueryError::Syntax(
                        "unclosed regex literal: expected a matching `/`".to_string(),
                    ));
                }
                query::LiteralKind::Plain => {} // fall through to delegate below
            }
        }

        match &leaf {
            UserInputLeaf::Exists { field } => return self.build_field_exists(field),
            UserInputLeaf::Range {
                field: Some(field),
                lower: UserInputBound::Unbounded,
                upper: UserInputBound::Unbounded,
            } => return self.build_field_exists(field),
            UserInputLeaf::Regex {
                field: Some(field),
                pattern,
            } => return self.build_regex(field, pattern),
            UserInputLeaf::Regex { field: None, .. } => {
                return Err(QueryError::Syntax(
                    "regex query needs a specific field".to_string(),
                ));
            }
            _ => {}
        }

        parser
            .build_query_from_user_input_ast(UserInputAst::Leaf(Box::new(leaf)))
            .map(|q| q as Box<dyn Query>)
            .map_err(|e| QueryError::Syntax(e.to_string()))
    }

    fn field_or_err(&self, field_name: &str) -> Result<Field, QueryError> {
        self.wf_schema
            .field(field_name)
            .ok_or_else(|| QueryError::Syntax(format!("undefined field \"{field_name}\"")))
    }

    /// `field:*` / `field:[* TO *]` — every doc carrying any value for
    /// `field` (finding 43's field-exists idiom; also the range-syntax
    /// equivalent, finding 44's `range_str_star_both`). `ExistsQuery` needs
    /// a fast (docValues) column, but Solr answers `field:*` from the
    /// postings on a plain indexed field with none (round-1 review's
    /// `exists_non_docvalues.json`: `body:*` = all five docs even though
    /// `body` is not `fast`) — so a non-fast field falls back to a
    /// constant-score `RegexQuery` matching every term in the field's own
    /// dictionary, which is exactly "this doc has at least one term here".
    fn build_field_exists(&self, field_name: &str) -> Result<Box<dyn Query>, QueryError> {
        let field = self.field_or_err(field_name)?;
        let is_fast = self
            .wf_schema
            .field_config(field_name)
            .is_some_and(|f| f.fast);
        if is_fast {
            return Ok(Box::new(ExistsQuery::new(field_name.to_string(), false)));
        }
        if self.wf_schema.value_kind(field_name) == Some(ValueKind::Text) {
            // ponytail: `.*` walks and matches every entry in the field's
            // term dictionary via the automaton machinery `RegexQuery`
            // already has (`AutomatonWeight`) — correct, but with no upper
            // bound on dictionary size the way a real "does this doc have a
            // fast column value" check would have. Fine for the corpora
            // this issue's fixtures cover; revisit if a non-fast text field
            // with a very large distinct-term count ever needs this path.
            return RegexQuery::from_pattern(".*", field)
                .map(|q| Box::new(q) as Box<dyn Query>)
                .map_err(|e| QueryError::RegexCompile(e.to_string()));
        }
        // A non-fast, non-text (numeric/date) field: no fixture exercises
        // this combination — `ExistsQuery` at least gives a clear (if
        // unfortunate) runtime error rather than a silently wrong answer.
        Ok(Box::new(ExistsQuery::new(field_name.to_string(), false)))
    }

    /// `field:term~[N]` — finding 42. Lowercased (never stemmed) on an
    /// analyzed text field, left alone on an unanalyzed `string`/`keyword`
    /// one; on a numeric/date field, always a 200 with 0 hits (fuzzy syntax
    /// is accepted everywhere, it just never matches there — no term
    /// dictionary makes edit-distance meaningful). Scored, not
    /// constant-score: every distinct term within the resolved edit distance
    /// becomes its own `TermQuery`, `Should`-joined, so BM25 tf/idf/length
    /// norm apply exactly as they would to an ordinary term query landing on
    /// the same term.
    fn build_fuzzy(
        &self,
        field_name: &str,
        term_raw: &str,
        distance_raw: &str,
    ) -> Result<Box<dyn Query>, QueryError> {
        let field = self.field_or_err(field_name)?;
        if self.wf_schema.value_kind(field_name) != Some(ValueKind::Text) {
            return Ok(Box::new(EmptyQuery));
        }
        let distance = query::resolve_fuzzy_distance(distance_raw);
        let term_text = if self.wf_schema.is_raw_string(field_name) {
            term_raw.to_string()
        } else {
            term_raw.to_lowercase()
        };
        let matches = self
            .matching_terms(field, &term_text, distance)
            .map_err(|e| QueryError::Internal(e.to_string()))?;
        Ok(terms_to_should_query(field, matches))
    }

    /// `[field:]glob` — finding 43. Lowercased (never stemmed) on an
    /// analyzed text field, left alone on `string`/`keyword`; a numeric/date
    /// field 400s (`qwild_int.json`'s "Can't run prefix queries on numeric
    /// fields" — there is no term dictionary to walk there). Constant-score,
    /// matching Lucene's own multi-term rewrite (finding 43).
    fn build_wildcard(&self, field_name: &str, glob: &str) -> Result<Box<dyn Query>, QueryError> {
        let field = self.field_or_err(field_name)?;
        if self.wf_schema.value_kind(field_name) != Some(ValueKind::Text) {
            return Err(QueryError::Syntax(format!(
                "can't run prefix queries on numeric fields: \"{field_name}\""
            )));
        }
        let normalized = if self.wf_schema.is_raw_string(field_name) {
            glob.to_string()
        } else {
            glob.to_lowercase()
        };
        let pattern = query::glob_to_regex(&normalized);
        RegexQuery::from_pattern(&pattern, field)
            .map(|q| Box::new(q) as Box<dyn Query>)
            .map_err(|e| QueryError::Syntax(e.to_string()))
    }

    /// `field:/pattern/` — finding 43/45. Anchored whole-term, case-sensitive,
    /// no analysis at all, over the *indexed* (post-analysis, e.g. stemmed)
    /// terms; constant-score. A pattern that fails automaton compilation
    /// (e.g. an unbalanced character class) is finding 45's one 500, not a
    /// 400 — this is the only place that error can come from, since a
    /// `/pattern` with no closing `/` never reaches this: the grammar's own
    /// `regex()` combinator only ever produces `UserInputLeaf::Regex` for a
    /// *closed* delimiter pair; an unclosed one is a `Literal` instead,
    /// caught by `query::classify_literal`'s `RegexUnclosed` (a 400) before
    /// `build_leaf` gets here.
    fn build_regex(&self, field_name: &str, pattern: &str) -> Result<Box<dyn Query>, QueryError> {
        let field = self.field_or_err(field_name)?;
        RegexQuery::from_pattern(pattern, field)
            .map(|q| Box::new(q) as Box<dyn Query>)
            .map_err(|e| QueryError::RegexCompile(e.to_string()))
    }

    /// Every distinct term in `field`'s term dictionary (across every
    /// segment) within `distance` of `term_text` — the scored-fuzzy
    /// building block `build_fuzzy` turns into a `Should`-joined
    /// `BooleanQuery` of `TermQuery`s.
    fn matching_terms(&self, field: Field, term_text: &str, distance: u8) -> Result<Vec<String>> {
        let searcher = self.reader.searcher();
        let mut out = std::collections::BTreeSet::new();
        for segment_reader in searcher.segment_readers() {
            let inverted_index = segment_reader.inverted_index(field)?;
            let mut stream = inverted_index.terms().stream()?;
            while stream.advance() {
                let Ok(text) = std::str::from_utf8(stream.key()) else {
                    continue;
                };
                if query::levenshtein(term_text, text) <= distance as usize {
                    out.insert(text.to_string());
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Rewrites `name:value` to `_dynamic.name:value` (or `_dynamic_text.`) for
    /// every `name` that is not a declared field but does match a
    /// `[[dynamic_fields]]` pattern, since that is where its values are indexed.
    ///
    /// ponytail: a scan for `<ident>:` rather than a real query-syntax parser,
    /// with one exception the scan does track: a double- or single-quoted
    /// span is copied through verbatim, `<ident>:` and all, never rewritten —
    /// a colon inside a quoted phrase is a literal phrase character, not a
    /// field query (finding 45's `phrase_with_colon`; the regression test is
    /// `tests/query_types.rs::dynamic_field_rewrite_must_not_apply_inside_a_quoted_phrase`).
    /// Everything else about this being a scan rather than a real parser
    /// still stands — revisit if/when the query layer needs more than that
    /// (#8).
    fn rewrite_dynamic_fields(&self, query_str: &str) -> String {
        if self.wf_schema.dynamic_fields.is_empty() {
            return query_str.to_string();
        }
        let mut out = String::with_capacity(query_str.len());
        let mut ident = String::new();
        let mut in_quote: Option<char> = None;
        let mut chars = query_str.chars();
        while let Some(ch) = chars.next() {
            if let Some(quote) = in_quote {
                out.push(ch);
                if ch == '\\' {
                    // Copy an escaped character through untouched too —
                    // this scan never rewrites inside quotes at all, so the
                    // exact escape semantics do not matter here, only that
                    // the quote's own closing delimiter is not mistaken for
                    // an escaped one.
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if ch == quote {
                    in_quote = None;
                }
                continue;
            }
            if ch == '"' || ch == '\'' {
                out.push_str(&ident);
                ident.clear();
                out.push(ch);
                in_quote = Some(ch);
                continue;
            }
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

    /// Generates up to one highlighted HTML snippet for `field_name` in the
    /// doc at `addr`, against `query`'s terms in that field (Solr's
    /// `hl`/`hl.fl`). Returns an empty `Vec` -- never a single empty-string
    /// entry -- when the field carries no term overlap for this doc (finding
    /// 51, `docs/solr-ref-findings.md`), or when the field is not stored
    /// (silently, mirroring `render_doc`'s own omit-rather-than-null
    /// treatment of a missing stored value -- unfixture-backed, the
    /// conservative choice for a case no captured response exercises).
    ///
    /// ponytail: Tantivy's public `SnippetGenerator` only exposes the single
    /// best-scoring fragment (`select_best_fragment_combination` is a private
    /// fn, `tantivy-0.26.1/src/snippet/mod.rs`), so `hl.snippets > 1` is a cap
    /// this can never actually fill past 1 -- at most one snippet comes back
    /// regardless of how many times a term repeats in the field. No captured
    /// fixture needs a second real snippet (`hl_snippets_two.json`'s query
    /// has exactly one hit per doc per field), so this is left as the
    /// ceiling rather than hand-rolling multi-fragment selection against a
    /// private algorithm.
    pub fn highlight_field(
        &self,
        query: &dyn Query,
        addr: DocAddress,
        field_name: &str,
        max_num_chars: usize,
        pre: &str,
        post: &str,
    ) -> Result<Vec<String>> {
        let field = self
            .wf_schema
            .field(field_name)
            .ok_or_else(|| anyhow!("can not highlight undefined field: {field_name}"))?;
        let searcher = self.reader.searcher();
        let mut generator = SnippetGenerator::create(&searcher, query, field)?;
        generator.set_max_num_chars(max_num_chars);
        let doc: TantivyDocument = searcher.doc(addr)?;
        let mut snippet = generator.snippet_from_doc(&doc);
        if snippet.is_empty() {
            return Ok(Vec::new());
        }
        snippet.set_snippet_prefix_postfix(pre, post);
        Ok(vec![snippet.to_html()])
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

    /// Solr's `stats.field`: min/max/count/sum/sumOfSquares/mean/stddev over a
    /// numeric or date fast field, computed only over docs that actually have
    /// a value in that field. Backed by Tantivy's `ExtendedStats` metric
    /// aggregation (`tantivy::aggregation::metric`), which already ignores
    /// docs missing the field rather than treating them as 0 — exactly the
    /// "computed only over present docs" contract issue #5 needs; `missing`
    /// itself is a separate `ExistsQuery` count, not part of this result.
    ///
    /// Solr's stddev is the *sample* standard deviation (dividing by `n - 1`),
    /// confirmed against `stats_views.json`/`stats_multi_fields.json`: e.g.
    /// `views`' five present values (10/20/30/40/50) have sample variance
    /// 1000/4 = 250, `sqrt(250) = 15.811388300841896`, matching the fixture
    /// exactly, while the population variance (1000/5 = 200) would not — so
    /// this reads `std_deviation_sampling`, not `std_deviation`/
    /// `std_deviation_population`.
    pub fn field_stats(&self, field_name: &str, query: &dyn Query) -> Result<ExtendedStats> {
        const AGG_NAME: &str = "wf_stats";

        let mut aggs = Aggregations::default();
        aggs.insert(
            AGG_NAME.to_string(),
            Aggregation {
                agg: AggregationVariants::ExtendedStats(ExtendedStatsAggregation::from_field_name(
                    field_name.to_string(),
                )),
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

        let Some(AggregationResult::MetricResult(MetricResult::ExtendedStats(stats))) =
            results.0.get(AGG_NAME)
        else {
            return Err(anyhow!(
                "could not compute stats on field `{field_name}`: unexpected aggregation result"
            ));
        };
        Ok((**stats).clone())
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
            .add_documents(
                &[json!({"id": "doc1", "body": "the quick brown fox", "extra_s": "tag"})],
                true,
            )
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

    /// A `*:*` sub-clause of a larger boolean query (e.g. `*:* AND lazy`)
    /// must parse without panicking or erroring — issue #39. Correctness of
    /// the resulting doc set/order is covered end-to-end against real Solr
    /// fixtures by `tests/differential.rs`'s
    /// `hermetic_whole_query_set_matches_committed_fixtures`; this test only
    /// pins that `parse_query` itself succeeds for the three shapes the
    /// panic was originally reported against.
    #[test]
    fn wildcard_subclause_parses_without_panicking() {
        let (_dir, index) = open_test_index();
        index
            .parse_query("*:* AND lazy", "body")
            .expect("*:* AND lazy must parse");
        index
            .parse_query("lazy OR *:*", "body")
            .expect("lazy OR *:* must parse");
        index
            .parse_query("*:* -lazy", "body")
            .expect("*:* -lazy must parse");
    }
}
