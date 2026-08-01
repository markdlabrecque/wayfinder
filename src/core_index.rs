//! The Tantivy-backed core: build, index, and search a single Wayfinder
//! core (PRD open question 1 — single-core-per-process, so there's exactly
//! one of these per running `app()`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
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
    AllQuery, BooleanQuery, BoostQuery, DisjunctionMaxQuery, EmptyQuery, ExistsQuery, Occur,
    PhraseQuery, Query, QueryClone, QueryParser, RegexQuery, TermQuery,
};
use tantivy::schema::document::Value as _;
use tantivy::schema::{Field, IndexRecordOption, OwnedValue};
use tantivy::snippet::SnippetGenerator;
use tantivy::time::OffsetDateTime;
use tantivy::time::format_description::well_known::Rfc3339;
use tantivy::tokenizer::TokenStream;
use tantivy::{
    DateTime, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument,
    Term,
};

use crate::collector::{AllScoredHits, SortClause};
use crate::config::ServerConfig;
use crate::edismax;
use crate::local_params;
use crate::query::{self, QueryError};
use crate::schema::{self, ValueKind, WayfinderSchema};

/// Ceiling on how many distinct terms one `facet.field` enumerates. Tantivy's
/// own aggregation bucket limit is the binding constraint, so the request asks
/// for exactly that many and no more.
const MAX_FACET_TERMS: u32 = DEFAULT_BUCKET_LIMIT;

/// `highlight_field`'s sentinel `max_num_chars` meaning "do not fragment at
/// all -- return the whole field as one snippet". Solr's `hl.fragsize=0`
/// (`docs/solr-ref-findings.md` finding 81, resolved in `crate::highlight`)
/// is the only thing that produces it.
///
/// `usize::MAX` is safe to hand to `SnippetGenerator::set_max_num_chars`:
/// Tantivy only ever compares it against a text-offset difference
/// (`(next.offset_to - fragment.start_offset) > max_num_chars` in
/// `tantivy-0.26.1/src/snippet/mod.rs::search_fragments`), so nothing is
/// allocated or sized by it and the split simply never fires.
pub(crate) const WHOLE_FIELD_MAX_CHARS: usize = usize::MAX;

/// One `hl.method=original` text fragment, modeled after Lucene's
/// `TextFragment`: raw byte bounds into the stored text, query-term ranges,
/// and the score used to choose `hl.snippets` candidates.
#[derive(Clone)]
struct OriginalHighlightFragment {
    range: Range<usize>,
    highlights: Vec<Range<usize>>,
    score: Score,
}

/// The same minimal HTML entity encoding `tantivy::snippet::Snippet::to_html`
/// applies to the non-highlighted parts of a fragment
/// (`htmlescape::encode_minimal`, whose `MINIMAL_ENTITIES` table is exactly
/// these five characters). Reimplemented rather than depended on directly so
/// the whole-field path in `highlight_field` -- which has to encode the
/// leading/trailing text Tantivy's token-bounded fragment leaves out -- encodes
/// it identically to the fragment Tantivy encoded itself.
fn encode_minimal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("&quot;"),
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#x27;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Renders an original-highlighter fragment with Tantivy's existing minimal
/// HTML escaping and the request's marker pair.
fn render_original_highlight_fragment(
    text: &str,
    fragment: OriginalHighlightFragment,
    pre: &str,
    post: &str,
) -> String {
    let mut highlights = fragment.highlights;
    highlights.sort_by_key(|range| (range.start, range.end));
    highlights.dedup();

    let mut out = String::new();
    let mut cursor = fragment.range.start;
    for range in highlights {
        let start = range.start.max(fragment.range.start);
        let end = range.end.min(fragment.range.end);
        if start >= end || start < cursor {
            continue;
        }
        out.push_str(&encode_minimal(&text[cursor..start]));
        out.push_str(pre);
        out.push_str(&encode_minimal(&text[start..end]));
        out.push_str(post);
        cursor = end;
    }
    out.push_str(&encode_minimal(&text[cursor..fragment.range.end]));
    out
}

/// Seeds each core's in-process `_version_` source from wall-clock time. A
/// restart therefore starts later than ordinary pre-restart writes without
/// persisting write-side version semantics; unusually fast restart/write
/// cycles remain outside this narrow stats-only compatibility scope.
fn version_seed() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// Lucene's classic English stopword list (`StandardAnalyzer`'s default,
/// also Solr's own `text_en` field type) — see `CoreIndex::mlt_query`'s doc
/// comment for why `/mlt` injects this explicitly rather than relying on
/// built-in `text_en`'s index-time stopword removal.
const ENGLISH_STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

/// Tuning knobs for `CoreIndex::mlt_query`, one field per `/mlt` request
/// param `src/lib.rs`'s handler parses. Solr's own defaults (`mlt.mintf=2`,
/// `mlt.mindf=5`, `mlt.maxqt=25`, `mlt.boost=false`, no word-length/max-doc-
/// frequency gate) are the caller's job to supply when a param is absent —
/// this struct carries only the resolved values, no further defaulting.
pub struct MltOptions {
    pub min_doc_frequency: Option<u64>,
    pub max_doc_frequency: Option<u64>,
    pub min_term_frequency: Option<usize>,
    pub max_query_terms: Option<usize>,
    pub min_word_length: Option<usize>,
    pub max_word_length: Option<usize>,
    pub boost_factor: Option<f32>,
}

/// `/mlt` term-mining noise filter (mirrors Tantivy's private
/// `MoreLikeThis::is_noise_word`): a word is dropped if it fails the
/// min/max length gate or is one of `ENGLISH_STOPWORDS`.
fn mlt_is_noise_word(
    word: &str,
    min_word_length: Option<usize>,
    max_word_length: Option<usize>,
) -> bool {
    let len = word.len();
    if len == 0 {
        return true;
    }
    if min_word_length.is_some_and(|min| len < min) {
        return true;
    }
    if max_word_length.is_some_and(|max| len > max) {
        return true;
    }
    ENGLISH_STOPWORDS.contains(&word)
}

/// `/mlt` term scoring weight (mirrors Tantivy's private, `pub(crate)`
/// `tantivy::query::bm25::idf`, unreachable from outside the crate).
fn mlt_idf(doc_freq: u64, doc_count: u64) -> Score {
    // `doc_count` (sum of `segment_reader.num_docs()`, alive docs only) can be
    // less than `doc_freq` (`Searcher::doc_freq`, which counts the raw term
    // dictionary and so still includes docs deleted by an overwrite —
    // `add_documents` deletes-then-reinserts on every unique-key collision)
    // once a doc has been overwritten. Tantivy's own private `bm25::idf`
    // asserts `doc_count >= doc_freq`; `saturating_sub` is this file's
    // equivalent guard against the subtract-with-overflow/garbage-idf that
    // would otherwise follow.
    let x = ((doc_count - doc_freq) as Score + 0.5) / (doc_freq as Score + 0.5);
    (1.0 + x).ln()
}

/// Recursively sums the size of every regular file under `dir`.
///
/// Unreadable entries are skipped rather than propagated: this backs a
/// display-only figure (see `CoreIndex::disk_size_bytes`), and a transient
/// stat failure on one segment file mid-merge must not turn the admin page
/// into a 500.
fn dir_size_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size_bytes(&entry.path()));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

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

/// Flattens a `tantivy::query_grammar::UserInputAst` (the same grammar
/// entry point `CoreIndex::parse_query` walks — finding 74) into a flat list
/// of `(Occur, boost, leaf)` triples for edismax's `q`, whose scope is
/// exactly the flat "words, quoted phrases, `+`/`-`" grammar this produces
/// for every query this issue's fixtures exercise. Nested clauses (parens,
/// `AND`/`OR`) are supported by recursing rather than assumed away,
/// combining an outer clause's `Occur` with each descendant leaf's own via
/// `combine_occur` — once any ancestor is `MustNot` the leaf is excluded,
/// once any ancestor (with no `MustNot` ancestor) is `Must` the leaf is
/// required, and only a leaf with every ancestor `Should` stays optional
/// and counts toward `mm`. `boost` (issue #109: `q=rocket^5`) multiplies
/// down through nested `Boost` nodes rather than being discarded, matching
/// the plain-parser `build_ast` path's own `UserInputAst::Boost` handling.
fn flatten_edismax_clauses(
    ast: tantivy::query_grammar::UserInputAst,
) -> Vec<(Occur, f32, tantivy::query_grammar::UserInputLeaf)> {
    use tantivy::query_grammar::UserInputAst;
    match ast {
        UserInputAst::Leaf(leaf) => vec![(Occur::Should, 1.0, *leaf)],
        UserInputAst::Boost(inner, boost) => flatten_edismax_clauses(*inner)
            .into_iter()
            .map(|(occur, weight, leaf)| (occur, weight * boost.into_inner() as f32, leaf))
            .collect(),
        UserInputAst::Clause(subqueries) => {
            let mut out = Vec::with_capacity(subqueries.len());
            for (occur_opt, sub) in subqueries {
                let occur = occur_opt.unwrap_or(Occur::Should);
                for (sub_occur, weight, leaf) in flatten_edismax_clauses(sub) {
                    out.push((combine_occur(occur, sub_occur), weight, leaf));
                }
            }
            out
        }
    }
}

/// Combines an outer clause's `Occur` with a nested leaf's own: `MustNot`
/// wins over everything (excluding always excludes), `Must` wins over
/// `Should` (a required clause nested inside another required clause is
/// still required), and only `Should`-under-`Should` stays optional.
fn combine_occur(outer: Occur, inner: Occur) -> Occur {
    if outer == Occur::MustNot || inner == Occur::MustNot {
        Occur::MustNot
    } else if outer == Occur::Must || inner == Occur::Must {
        Occur::Must
    } else {
        Occur::Should
    }
}

/// The already-built inline nested queries (issue #137) a query string's
/// sentinel literals stand for, together with the sentinel prefix
/// `local_params::extract_nested_queries` actually keyed them with.
///
/// The prefix travels with the queries rather than being a constant, because
/// `extract_nested_queries` re-keys it whenever the user's own query text
/// already contains the base prefix. Resolving against a constant instead would
/// let user-supplied text be mistaken for a placeholder and silently answer a
/// different query (round-2 review item 1).
struct NestedQueries<'a> {
    sentinel_prefix: &'a str,
    built: Vec<Box<dyn Query>>,
}

impl NestedQueries<'_> {
    /// No local-params block was present, so no literal resolves to anything.
    const NONE: Self = Self {
        sentinel_prefix: "",
        built: Vec::new(),
    };

    /// The nested query `phrase` stands for, if it is one of *this* rewrite's
    /// sentinels. An out-of-range index falls through to `None` and so is
    /// parsed as ordinary text.
    fn resolve(&self, phrase: &str) -> Option<Box<dyn Query>> {
        let i = local_params::sentinel_index(self.sentinel_prefix, phrase)?;
        self.built.get(i).map(|q| q.box_clone())
    }
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

/// What one `qf`/`pf` entry actually addresses in the Tantivy index: a
/// declared `[[fields]]` handle, or — for a name that only matches a
/// `[[dynamic_fields]]` pattern (issue #84) — a JSON sub-path of the
/// catch-all container that dynamic rule writes into. Terms differ between
/// the two (`Term::from_field_text` versus a JSON-path term), which is why
/// this cannot just be a `Field`.
#[derive(Clone, Debug)]
enum FieldTarget {
    Static(Field),
    Dynamic { container: Field, path: String },
}

impl FieldTarget {
    /// The Tantivy field the target's terms live in — itself for a static
    /// field, the catch-all container for a dynamic one.
    fn field(&self) -> Field {
        match self {
            FieldTarget::Static(field) => *field,
            FieldTarget::Dynamic { container, .. } => *container,
        }
    }
}

pub struct CoreIndex {
    pub wf_schema: WayfinderSchema,
    /// The directory this core's segments live in. Kept so read-only
    /// introspection (the admin UI's on-disk size, issue #94) can measure the
    /// same directory the writer/reader are using, rather than re-deriving it
    /// from a Tantivy `Directory` that does not expose a path.
    data_dir: PathBuf,
    index: Index,
    state: Arc<CommitState>,
    reader: IndexReader,
    /// From `ServerConfig.commit` (parsed since #12, consumed here since #9).
    /// The Nth uncommitted doc triggers a commit.
    autocommit_max_docs: Option<u64>,
    /// The first uncommitted doc since the last commit arms a deadline this
    /// many ms out.
    autocommit_max_time_ms: Option<u64>,
    /// One source per core. Versions are assigned after validation, directly
    /// before writer insertion, and intentionally are not update-response or
    /// optimistic-concurrency semantics.
    version_source: AtomicI64,
    /// Process-lifetime delete counters, for
    /// `UPDATE.updateHandler.deletesById` / `.deletesByQuery` on
    /// `/admin/mbeans` (issue #158). Deliberately not persisted: Solr's own
    /// figures reset on core reload too, so a fresh process starting at 0 is
    /// the matching behaviour, not a gap.
    ///
    /// `deletes_by_id` counts *ids*, not calls -- Solr's JSON update loader
    /// turns `{"delete": ["a","b"]}` into one `DeleteUpdateCommand` per id, so
    /// a two-id body moves the real counter by two.
    ///
    /// `Relaxed`: these are display-only monotonic counters read by a
    /// different request than the one that bumped them, with no other memory
    /// ordered against them -- so the cost on the update path is a bare
    /// `lock xadd`, no fences.
    deletes_by_id: AtomicU64,
    deletes_by_query: AtomicU64,
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
        // A snapshot is Wayfinder's durable proof that an existing index
        // predates this open. `app_with_schema` legitimately pre-creates an
        // empty data directory, so directory existence alone must never turn
        // a fresh index into a legacy one.
        let has_snapshot = snapshot.exists();
        let (previous_uses_changed_analyzer_path, previous_has_dynamic_fields) = if has_snapshot {
            let previous = std::fs::read_to_string(&snapshot)
                .with_context(|| format!("reading stored schema {}", snapshot.display()))?;
            schema::check_compatible(&previous, &schema_toml).with_context(|| {
                format!(
                    "the index in {} was built with an incompatible schema",
                    data_dir.display()
                )
            })?;
            let previous_schema = schema::parse(&previous)
                .context("parsing the index's stored schema for its analyzer contract")?;
            (
                previous_schema.uses_changed_analyzer_path(),
                previous_schema.has_dynamic_fields(),
            )
        } else {
            (false, false)
        };

        // `text_en` changed its index-time semantics in analyzer contract v1.
        // A pre-marker index can safely be adopted only when neither its
        // configured nor persisted schema could have written through a changed
        // path: static `text_en`, or any analyzed dynamic rule sharing
        // `_dynamic_text`'s versioned tokenizer.
        let analyzer_contract = schema::analyzer_contract_path(data_dir);
        if analyzer_contract.exists() {
            let persisted = std::fs::read_to_string(&analyzer_contract).with_context(|| {
                format!(
                    "reading analyzer contract marker {}",
                    analyzer_contract.display()
                )
            })?;
            match persisted.trim() {
                schema::ANALYZER_CONTRACT => {}
                // A raw-only dynamic pre-v1 index is safe to open, but its
                // unused `_dynamic_text` schema still names `en_stem`. Do not
                // let a compatible rule edit begin using that old catch-all.
                schema::ANALYZER_CONTRACT_LEGACY_DYNAMIC_TEXT
                    if wf_schema.uses_changed_analyzer_path()
                        || previous_uses_changed_analyzer_path =>
                {
                    bail!(
                        "the index in {} predates the Solr-compatible text_en/_dynamic_text analyzer contract; reindex into a fresh data directory",
                        data_dir.display()
                    );
                }
                schema::ANALYZER_CONTRACT_LEGACY_DYNAMIC_TEXT => {}
                other => {
                    bail!(
                        "the index in {} has unsupported analyzer contract `{other}`; reindex into a fresh data directory",
                        data_dir.display()
                    );
                }
            }
        } else if has_snapshot
            && (wf_schema.uses_changed_analyzer_path() || previous_uses_changed_analyzer_path)
        {
            bail!(
                "the index in {} predates the Solr-compatible text_en/_dynamic_text analyzer contract; reindex into a fresh data directory",
                data_dir.display()
            );
        }

        // Write the marker before opening or creating the Tantivy index. A
        // marker-write failure now leaves no newly-created versioned index
        // behind, so a retry cannot mistake it for a pre-contract index. A
        // real legacy index has a snapshot and was rejected above before any
        // marker write; an unaffected legacy index is safe to adopt. A legacy
        // dynamic schema retains a distinct state because its unused
        // `_dynamic_text` catch-all still carries the old `en_stem` identity.
        if !analyzer_contract.exists() {
            let marker = if has_snapshot && previous_has_dynamic_fields {
                schema::ANALYZER_CONTRACT_LEGACY_DYNAMIC_TEXT
            } else {
                schema::ANALYZER_CONTRACT
            };
            std::fs::write(&analyzer_contract, marker).with_context(|| {
                format!(
                    "writing analyzer contract marker {}",
                    analyzer_contract.display()
                )
            })?;
        }

        // `settings` only apply to a newly created index; re-opening an
        // existing one keeps the doc-store settings it was built with, which
        // is Tantivy's own rule and worth knowing before tuning them.
        let mut index = Index::builder()
            .schema(wf_schema.tantivy_schema.clone())
            .tokenizers(wf_schema.tokenizers.clone())
            .fast_field_tokenizers(wf_schema.tokenizers.clone())
            .settings(config.index_settings()?)
            .create_in_dir(data_dir)
            .or_else(|_| Index::open_in_dir(data_dir))
            .context("opening/creating Tantivy index")?;
        if index.schema().get_field(schema::VERSION_FIELD).is_err() {
            bail!(
                "the index in {} predates internal field `{}`; reindex into a fresh data directory",
                data_dir.display(),
                schema::VERSION_FIELD
            );
        }
        index.set_tokenizers(wf_schema.tokenizers.clone());
        index.set_fast_field_tokenizers(wf_schema.tokenizers.clone());

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

        std::fs::write(&snapshot, &schema_toml)
            .with_context(|| format!("writing stored schema {}", snapshot.display()))?;

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
            data_dir: data_dir.to_path_buf(),
            index,
            state,
            reader,
            autocommit_max_docs: config.commit.autocommit_max_docs,
            autocommit_max_time_ms: config.commit.autocommit_max_time,
            version_source: AtomicI64::new(version_seed()),
            deletes_by_id: AtomicU64::new(0),
            deletes_by_query: AtomicU64::new(0),
        })
    }

    /// Uncommitted docs added since the last commit -- the same counter
    /// `autocommit_max_docs` is driven by, exposed rather than duplicated, so
    /// `UPDATE.updateHandler.docsPending` (`/admin/mbeans`, issue #158) can
    /// never disagree with the threshold the core actually acts on.
    pub fn pending_docs(&self) -> u64 {
        self.state.pending_docs.load(Ordering::Relaxed)
    }

    /// Ids deleted by `delete_by_ids` over this process's lifetime
    /// (`UPDATE.updateHandler.deletesById`).
    pub fn deletes_by_id(&self) -> u64 {
        self.deletes_by_id.load(Ordering::Relaxed)
    }

    /// Delete-by-query calls over this process's lifetime
    /// (`UPDATE.updateHandler.deletesByQuery`).
    pub fn deletes_by_query(&self) -> u64 {
        self.deletes_by_query.load(Ordering::Relaxed)
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
                let mut tantivy_doc = self.build_document(obj)?;
                let version_field = self
                    .index
                    .schema()
                    .get_field(schema::VERSION_FIELD)
                    .expect("the internal _version_ field exists in every Wayfinder schema");
                // Validation above completed before consuming a version; this
                // is the last operation before the document reaches Tantivy.
                tantivy_doc.add_i64(
                    version_field,
                    self.version_source.fetch_add(1, Ordering::SeqCst),
                );
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
        // One atomic add for the whole batch, not one per id: the counter is
        // per-id in value (matching Solr, whose loader raises one command per
        // id) without paying a per-id atomic on the update path.
        self.deletes_by_id
            .fetch_add(ids.len() as u64, Ordering::Relaxed);
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
        // After the writer accepted it, not before: a query that failed to
        // parse (the `?` above) never became a delete, so it must not count.
        // Mutation-tested by
        // `tests/admin_mbeans.rs::mbeans_deletes_by_query_does_not_count_a_query_that_failed_to_parse`
        // -- hoisting this line above the parse makes that test fail with 1.
        self.deletes_by_query.fetch_add(1, Ordering::Relaxed);
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

        // Inline `{!edismax qf=...}` nested queries (issue #137) are lifted out
        // *before* the rewrites and the grammar, because neither can see them:
        // `{`/`!`/a quoted `qf` value are not lucene query syntax to Tantivy's
        // grammar, which 400s on the raw string. Each block plus its bound
        // token is replaced by a sentinel literal, so the outer parser still
        // decides the nested clause's `+`/`-`/paren context itself — see
        // `crate::local_params`.
        let lifted = local_params::extract_nested_queries(query_str)
            .map_err(|msg| anyhow::Error::from(QueryError::Syntax(msg)))?;
        let (source, nested) = match &lifted {
            None => (query_str, NestedQueries::NONE),
            Some(rewritten) => {
                let mut built: Vec<Box<dyn Query>> = Vec::with_capacity(rewritten.nested.len());
                for nq in &rewritten.nested {
                    built.push(self.build_nested_query(nq, default_field_name)?);
                }
                (
                    rewritten.outer.as_str(),
                    NestedQueries {
                        // The prefix the rewrite actually used, which is not the
                        // base constant when `query_str` already contained it.
                        sentinel_prefix: &rewritten.sentinel_prefix,
                        built,
                    },
                )
            }
        };

        let rewritten = self.rewrite_dynamic_fields(&self.rewrite_wildcard_subclause(source));
        let user_ast = tantivy::query_grammar::parse_query(&rewritten)
            .map_err(|_| anyhow!("could not parse query `{query_str}`"))?;
        let parser = QueryParser::for_index(&self.index, vec![default_field]);
        self.build_ast(user_ast, &parser, default_field_name, &nested)
            .map_err(anyhow::Error::from)
    }

    /// One inline nested query: `{!edismax qf='...'}<bound token>` becomes the
    /// same `parse_edismax_query` composition a `defType=edismax` request
    /// would get, over just the block's own `qf` — the nested parser's local
    /// params are its whole configuration, so no request-level `mm`/`pf`/`bq`/
    /// `boost`/`tie` leaks in (Solr's nested parsers do inherit request params
    /// it does not name, but no capture varies any of those alongside a nested
    /// query, so inheriting nothing is the smaller claim).
    fn build_nested_query(
        &self,
        nq: &local_params::NestedQuery,
        default_field_name: &str,
    ) -> Result<Box<dyn Query>> {
        let qf = nq.local.get("qf").unwrap_or("");
        self.parse_edismax_query(&nq.text, default_field_name, qf, None, None, 0.0, &[], None)
            .map_err(anyhow::Error::from)
    }

    /// Builds a `defType=edismax` query (issue #7, PRD §5 v1 exception):
    /// `q` walked into one `Should`/`Must`/`MustNot`-tagged clause per
    /// top-level term/phrase (finding 74 — the same `+`/`-`/quote grammar as
    /// the plain parser, `tantivy::query_grammar::parse_query` again), each
    /// clause a `DisjunctionMaxQuery` (with `tie`) over every `qf` field's
    /// own per-field query, `mm` applied as `BooleanQuery`'s own
    /// `minimum_number_should_match` over just the `Should` clauses (finding
    /// 68 — a `None` `mm` leaves Tantivy's own built-in "at least one
    /// optional clause must match when there's no `Must`" default alone,
    /// which already reproduces Solr's own no-`mm`-param default of
    /// effectively `0%`/OR without this needing to special-case it), then
    /// wrapped as the sole `Must` clause of an outer `BooleanQuery` whose
    /// other clauses are `pf`'s phrase-boost (finding 70) and each `bq`
    /// (finding 73) — both score-only `Should` clauses that can never block
    /// a match once a `Must` clause is present (Tantivy's own
    /// `RequiredOptionalScorer` path). `boost` (finding 72) is a final
    /// `BoostQuery` multiplying the whole thing.
    ///
    /// `qf`/`pf` field lists (and their `^boost` suffixes) come from
    /// `edismax::parse_field_weights`. An unknown field name in `qf` is a 400
    /// (finding 84, extended to `q=*:*` by finding 88); in `pf` it is still
    /// silently dropped, a Wayfinder leniency choice with no fixture pinning
    /// stricter behaviour either way. A leaf this function does not
    /// specially handle (a fielded literal, a range, a set, `bf`'s function-
    /// query syntax) falls through to `build_ast`/`build_leaf` against just
    /// the default field, the same machinery `parse_query` itself uses —
    /// out of `qf`'s per-field weighting, but never a hard error, which is
    /// what finding 75 requires for `bf`.
    #[allow(clippy::too_many_arguments)]
    pub fn parse_edismax_query(
        &self,
        q: &str,
        default_field_name: &str,
        qf: &str,
        pf: Option<&str>,
        mm: Option<&str>,
        tie: f32,
        bq: &[String],
        boost: Option<f32>,
    ) -> Result<Box<dyn Query>, QueryError> {
        // Real Solr 400s if *any* named `qf` field is undefined, even when
        // other fields in the same `qf` are valid (issue #111) -- unlike
        // `pf`'s own unknown-field leniency (a Wayfinder choice), which this does not
        // touch. Checked up front against the raw parsed names rather than
        // relying on `resolve_field_weights`'s drop-unknown filtering, which
        // exists for `pf` and for `qf`'s empty-spec default-field fallback.
        // Must use `field_target` (static-before-dynamic), not a raw
        // `wf_schema.field` lookup -- a `qf` naming only a dynamic field
        // (issue #84) is valid and must not 400 here.
        //
        // This runs *before* the `*:*` short-circuit below (issue #112):
        // Solr validates `qf` regardless of the query shape, so
        // `q=*:*&qf=nosuchfield` 400s just as a term query would. Returning
        // `AllQuery` first made an invalid `qf` silently 200.
        if !qf.trim().is_empty() {
            for (name, _) in edismax::parse_field_weights(qf) {
                if self.field_target(&name).is_none() {
                    return Err(QueryError::Syntax(format!(
                        "edismax `qf` names an undefined field: `{name}`"
                    )));
                }
            }
        }

        // `*:*` is special-cased exactly like the plain parser (`parse_query`
        // above): Tantivy 0.26's own grammar parses it as
        // `UserInputLeaf::Exists { field: "*" }`, which without this
        // short-circuit falls into this function's per-leaf `_` arm and 400s
        // as an undefined field (`*` is not a real field). Real Solr returns
        // every doc for `q=*:*&defType=edismax`, same as under the lucene
        // parser. A valid (or absent) `qf` is irrelevant to the result here —
        // every doc matches — so nothing below this point needs to run.
        if q.trim() == "*:*" {
            return Ok(Box::new(AllQuery));
        }

        let default_field = self.wf_schema.field(default_field_name).ok_or_else(|| {
            QueryError::Internal(format!("unknown default field `{default_field_name}`"))
        })?;

        let qf_fields = self.resolve_field_weights(qf, default_field_name);
        if qf_fields.is_empty() {
            return Err(QueryError::Syntax(format!(
                "edismax `qf` names no field this core has: `{qf}`"
            )));
        }

        // Same rewrite prologue `parse_query` runs before handing off to the
        // grammar (wildcard subclause + dynamic-field rewriting) — omitting
        // this was a real bug: a dynamic-field name in an edismax `q` would
        // otherwise reach the grammar unrewritten and 400 the same way a
        // missing `*:*` short-circuit did above.
        let rewritten = self.rewrite_dynamic_fields(&self.rewrite_wildcard_subclause(q));
        let user_ast = tantivy::query_grammar::parse_query(&rewritten)
            .map_err(|_| QueryError::Syntax(format!("could not parse query `{q}`")))?;
        let flat = flatten_edismax_clauses(user_ast);

        let parser = QueryParser::for_index(&self.index, vec![default_field]);
        let mut literal_texts: Vec<String> = Vec::new();
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(flat.len());
        for (occur, leaf_boost, leaf) in flat {
            let built: Box<dyn Query> = match &leaf {
                tantivy::query_grammar::UserInputLeaf::Literal(lit) if lit.field_name.is_none() => {
                    literal_texts.push(lit.phrase.clone());
                    let quoted = lit.delimiter != tantivy::query_grammar::Delimiter::None;
                    self.build_field_disjunction(&lit.phrase, quoted, &qf_fields, tie)
                }
                _ => self.build_ast(
                    tantivy::query_grammar::UserInputAst::Leaf(Box::new(leaf)),
                    &parser,
                    default_field_name,
                    &NestedQueries::NONE,
                )?,
            };
            clauses.push((occur, Box::new(BoostQuery::new(built, leaf_boost))));
        }

        if clauses.is_empty() {
            return Ok(Box::new(EmptyQuery));
        }

        // `mm` *present but empty* (`mm=`, or a bare `mm` with no `=`, or a
        // whitespace-only `mm=%20` -- all three arrive here as `Some("")`/
        // `Some(" ")`) is a malformed spec, not an absent one: real Solr 400s
        // with `Invalid 'mm' spec. Expecting an integer.` (a
        // `NumberFormatException` -- fixture
        // `solr-ref/responses/edismax_mm_empty_string.json`, finding 89).
        // Issue #113's own premise, that Solr ignores an empty `mm`, is wrong.
        // `mm` entirely absent (`None`) is the untouched case: no
        // `minimum_number_should_match` is set at all and the normal OR
        // default stands (`edismax_mm_absent.json`). `Params::get`
        // distinguishes the two -- `mm=` parses to `Some("")`, an absent `mm`
        // to `None`.
        //
        // Placement is load-bearing and capture-derived, not a guess (finding
        // 89): Solr only *parses* `mm` when it has a multi-clause boolean
        // query to apply it to, so an empty `mm` alongside a `q` that yields
        // fewer than two clauses is never reached and 200s. Captured
        // one-clause 200s: `q=*:*` (short-circuited above), `q=` (the empty-
        // `clauses` return above), `q=alpha`, `q=title:rocket`,
        // `q="alpha beta"`, `q=-mission`. Captured multi-clause 400s:
        // `q=alpha beta`, `q=+alpha +beta`, `q=alpha -mission` -- occur kind
        // is irrelevant, only the count. Hence: after the `*:*` and empty-
        // `clauses` early returns, after `qf` validation (a bad `qf` alongside
        // `mm=` 400s on the `qf` name, captured), and on the raw clause count
        // *before* the all-`MustNot` `AllQuery` augmentation below, which is
        // Wayfinder-internal and would otherwise turn a captured-200
        // `q=-mission` into a 400.
        if clauses.len() >= 2
            && let Some(spec) = mm
            && spec.trim().is_empty()
        {
            return Err(QueryError::Syntax(
                "Invalid 'mm' spec. Expecting an integer.".to_string(),
            ));
        }

        // Same all-`MustNot` guard as `build_ast`: an edismax `q=-mission`
        // alone must mean "every doc except one with mission", not zero
        // results from an ill-defined all-exclusion `BooleanQuery`.
        if clauses.iter().all(|(occur, _)| *occur == Occur::MustNot) {
            clauses.push((Occur::Should, Box::new(AllQuery)));
        }

        let should_count = clauses
            .iter()
            .filter(|(occur, _)| *occur == Occur::Should)
            .count();
        let mut main_query = BooleanQuery::new(clauses);
        if let Some(mm_spec) = mm {
            main_query
                .set_minimum_number_should_match(edismax::min_should_match(mm_spec, should_count));
        }

        let mut outer_clauses: Vec<(Occur, Box<dyn Query>)> =
            vec![(Occur::Must, Box::new(main_query))];

        if let Some(pf_spec) = pf
            && let Some(pf_query) =
                self.build_pf_query(pf_spec, default_field_name, &literal_texts, tie)
        {
            outer_clauses.push((Occur::Should, pf_query));
        }

        for bq_str in bq {
            let bq_query = self
                .parse_query(bq_str, default_field_name)
                .map_err(|e| QueryError::Syntax(e.to_string()))?;
            outer_clauses.push((Occur::Should, bq_query));
        }

        let mut composed: Box<dyn Query> = if outer_clauses.len() == 1 {
            outer_clauses
                .into_iter()
                .next()
                .expect("checked len == 1")
                .1
        } else {
            Box::new(BooleanQuery::new(outer_clauses))
        };

        if let Some(boost_factor) = boost {
            composed = Box::new(BoostQuery::new(composed, boost_factor));
        }

        Ok(composed)
    }

    /// `qf`/`pf`'s `field^boost` list, resolved to query targets (dropping
    /// any name this core neither declares nor matches with a
    /// `[[dynamic_fields]]` pattern) — an empty `spec` falls back to
    /// `default_field_name` at weight 1.0, matching Solr's own behaviour when
    /// `qf` is absent (`df` alone drives the query, just as it does for the
    /// plain parser).
    ///
    /// A name that only matches a dynamic rule (issue #84) resolves to the
    /// catch-all container's JSON sub-path (`_dynamic[_text].<name>`) — the
    /// same addressing `WayfinderSchema::resolved_fast_column` uses for fast
    /// fields and `rewrite_dynamic_fields` splices into a query string —
    /// rather than being silently dropped for want of a literal `Field`
    /// handle.
    fn resolve_field_weights(
        &self,
        spec: &str,
        default_field_name: &str,
    ) -> Vec<(FieldTarget, f32)> {
        let weights = if spec.trim().is_empty() {
            vec![(default_field_name.to_string(), 1.0)]
        } else {
            edismax::parse_field_weights(spec)
        };
        weights
            .into_iter()
            .filter_map(|(name, boost)| self.field_target(&name).map(|target| (target, boost)))
            .collect()
    }

    /// The query target backing `name`, with the same static-before-dynamic
    /// precedence as indexing (`is_static`/`match_dynamic`): a declared field
    /// wins, otherwise the catch-all JSON container plus `name` as the path.
    ///
    /// ponytail: a dynamic name resolves to a *string*-typed JSON term, which
    /// is exactly right for the `_dynamic_text` container (`qf`/`pf` are text
    /// relevance params) but means a `qf` naming a non-text dynamic rule
    /// (`*_i` in the `_dynamic` container, whose values index as typed JSON
    /// numbers) contributes a clause that cannot match rather than a numeric
    /// term. Tantivy's own numeric coercion for JSON paths
    /// (`convert_to_fast_value_and_append_to_json_term`) is private to its
    /// query parser, so matching that would mean reimplementing it.
    ///
    /// Note the failure mode this trades: before #84 such a name was dropped
    /// outright, so a `qf` naming *only* a numeric dynamic field left
    /// `qf_fields` empty and 400d with "names no field this core has". Now it
    /// resolves, the list is non-empty, and the request 200s with
    /// `numFound: 0` — a loud wrong answer became a quiet one for the numeric
    /// case, which is the price of the text case (the one `qf`/`pf` exist
    /// for) working at all. Raising this ceiling means encoding numeric JSON
    /// terms here, not restoring the 400.
    fn field_target(&self, name: &str) -> Option<FieldTarget> {
        if let Some(field) = self.wf_schema.field(name) {
            return Some(FieldTarget::Static(field));
        }
        let rule = self.wf_schema.match_dynamic(name)?;
        let container_name = self.wf_schema.dynamic_target(rule);
        let container = self.wf_schema.field(container_name)?;
        Some(FieldTarget::Dynamic {
            container,
            path: name.to_string(),
        })
    }

    /// Whether this core can address `name` at all — the public face of
    /// `field_target`, so callers outside this module (`lib.rs`'s
    /// `check_terms_field`) test existence through the *same* static-before-
    /// dynamic resolution the query path uses instead of growing a second,
    /// drift-prone copy of the rule. True for a declared `[[fields]]` entry
    /// and for a name only a `[[dynamic_fields]]` pattern matches; false for a
    /// name matching neither.
    pub fn resolves_field_name(&self, name: &str) -> bool {
        self.field_target(name).is_some()
    }

    /// One edismax clause's `qf`-wide query: `phrase_text` tokenized with
    /// each `qf` field's own indexing analyzer (a bare word normally
    /// tokenizes to exactly one term; a quoted multi-word phrase to more
    /// than one, becoming a `PhraseQuery` rather than a `TermQuery` — see
    /// finding 74), each wrapped in that field's `BoostQuery`, combined via
    /// `DisjunctionMaxQuery::with_tie_breaker` (finding 69's reordering,
    /// finding 71's tie behaviour). A field whose analyzer drops every token
    /// (for example, an all-stopword `text_en` phrase) is simply absent from
    /// the disjunction rather than contributing an
    /// ill-defined empty clause.
    ///
    /// `quoted` says whether the clause was written as a quoted phrase in `q`.
    /// It decides what a *bare* string that nonetheless analyzes to several
    /// tokens (`quick+rocket` — `+` is an ordinary term character mid-token in
    /// Lucene, so this is one clause, not two) becomes: an unquoted
    /// multi-token clause is an *optional boolean* over its tokens, and only
    /// an explicitly quoted clause becomes a `PhraseQuery`.
    ///
    /// **That split is settled by capture, not by documentation** (issue
    /// #147). `solr-ref/responses/edismax_unquoted_multitoken.json` — manifest
    /// row `edismax_unquoted_multitoken`,
    /// `q=quick%2Brocket&defType=edismax&qf=title+body&sort=id+asc`, taken
    /// against a real `solr:9` with `capture.sh`'s edismax block schema and
    /// 10-doc corpus — answers `numFound=6` (`eA eB eC eD pA pB`): every
    /// document carrying *either* token, and no document in that corpus
    /// carries the two adjacent. A `PhraseQuery` reading would have matched 0,
    /// so Solr's answer is the boolean-OR reading this function implements.
    /// Asserted by
    /// `tests/edismax.rs::unquoted_multitoken_clause_matches_committed_capture`,
    /// which reads both `numFound` and the id list out of that fixture.
    ///
    /// The capture replaces the reasoning this comment used to rest on — Solr's
    /// *documented* `autoGeneratePhraseQueries` default (off for schema
    /// `version >= 1.4`, with `solr-ref/search-api/configset/schema.xml:52`
    /// declaring `version="1.6"` and the attribute set nowhere in the
    /// configset). That inference is now corroborated rather than load-bearing;
    /// finding 92 records it, and findings 90/91 the related binding rule.
    /// Note for anyone re-deriving it: `schema.xml:63` is **inside an XML
    /// comment** documenting the `version` attribute's history and establishes
    /// nothing on its own.
    ///
    /// The `select.q.local-params-edismax.and` coverage probe's expectation is
    /// derived from this fixture too, having been a speculative placeholder
    /// from `bb44cc4` (#105) until issue #147.
    fn build_field_disjunction(
        &self,
        phrase_text: &str,
        quoted: bool,
        qf_fields: &[(FieldTarget, f32)],
        tie: f32,
    ) -> Box<dyn Query> {
        let mut disjuncts: Vec<Box<dyn Query>> = Vec::with_capacity(qf_fields.len());
        for (target, field_boost) in qf_fields {
            let tokens = self.tokenize_for_target(target, phrase_text);
            let base: Box<dyn Query> = match tokens.as_slice() {
                [] => continue,
                [only] => Box::new(TermQuery::new(
                    self.term_for_target(target, only),
                    IndexRecordOption::WithFreqsAndPositions,
                )),
                _ if quoted => {
                    let terms: Vec<Term> = tokens
                        .iter()
                        .map(|t| self.term_for_target(target, t))
                        .collect();
                    Box::new(PhraseQuery::new(terms))
                }
                _ => {
                    let clauses: Vec<(Occur, Box<dyn Query>)> = tokens
                        .iter()
                        .map(|t| {
                            let q: Box<dyn Query> = Box::new(TermQuery::new(
                                self.term_for_target(target, t),
                                IndexRecordOption::WithFreqsAndPositions,
                            ));
                            (Occur::Should, q)
                        })
                        .collect();
                    Box::new(BooleanQuery::new(clauses))
                }
            };
            disjuncts.push(Box::new(BoostQuery::new(base, *field_boost)));
        }
        if disjuncts.is_empty() {
            Box::new(EmptyQuery)
        } else {
            Box::new(DisjunctionMaxQuery::with_tie_breaker(disjuncts, tie))
        }
    }

    /// `pf`'s phrase-boost query (finding 70): every literal clause's text
    /// from `q` (in original order, `+`/`-`/quoting stripped away — exactly
    /// the free-text terms) joined with a space and re-tokenized per `pf`
    /// field, then built into a `PhraseQuery` if that field's analyzer
    /// produces more than one token (a single-token "phrase" is just the
    /// `qf` clause again, so it is skipped rather than duplicated — also
    /// what keeps a one-word `q` from producing a `pf` clause at all, since
    /// `PhraseQuery::new` itself requires more than one term). `None` when
    /// no `pf` field survives that filter, so callers never need to inspect
    /// an empty `DisjunctionMaxQuery`.
    fn build_pf_query(
        &self,
        pf_spec: &str,
        default_field_name: &str,
        literal_texts: &[String],
        tie: f32,
    ) -> Option<Box<dyn Query>> {
        let pf_fields = self.resolve_field_weights(pf_spec, default_field_name);
        let joined = literal_texts.join(" ");
        if pf_fields.is_empty() || joined.trim().is_empty() {
            return None;
        }
        let mut disjuncts: Vec<Box<dyn Query>> = Vec::new();
        for (target, field_boost) in &pf_fields {
            let tokens = self.tokenize_for_target(target, &joined);
            if tokens.len() < 2 {
                continue;
            }
            let terms: Vec<Term> = tokens
                .iter()
                .map(|t| self.term_for_target(target, t))
                .collect();
            let phrase: Box<dyn Query> = Box::new(PhraseQuery::new(terms));
            disjuncts.push(Box::new(BoostQuery::new(phrase, *field_boost)));
        }
        if disjuncts.is_empty() {
            None
        } else {
            Some(Box::new(DisjunctionMaxQuery::with_tie_breaker(
                disjuncts, tie,
            )))
        }
    }

    /// One analyzed token as an index term for `target`: a plain field term
    /// for a declared field, or the JSON-path term shape Tantivy's own
    /// `generate_literals_for_json_object` builds
    /// (`Term::from_field_json_path` + `append_type_and_str`) for a dynamic
    /// name inside a catch-all container — the same encoding the indexing
    /// path produced via `add_object`, so the two match.
    fn term_for_target(&self, target: &FieldTarget, token: &str) -> Term {
        match target {
            FieldTarget::Static(field) => Term::from_field_text(*field, token),
            FieldTarget::Dynamic { container, path } => {
                let schema = self.index.schema();
                let expand_dots = match schema.get_field_entry(*container).field_type() {
                    tantivy::schema::FieldType::JsonObject(opts) => opts.is_expand_dots_enabled(),
                    _ => false,
                };
                let mut term = Term::from_field_json_path(*container, path, expand_dots);
                term.append_type_and_str(token);
                term
            }
        }
    }

    /// Tokenizes free-standing `text` (not a stored doc value — a `q`/`pf`
    /// clause's own literal text) with `target`'s own indexing analyzer, the
    /// same tokenizer chain `mlt_query` mines stored values with. For a
    /// dynamic target that analyzer is the catch-all container's, not a
    /// declared field's (see the `JsonObject` arm below). A non-text target
    /// (or one with no configured tokenizer) falls back to the raw text as a
    /// single "token" — edismax's `qf`/`pf` are only ever pointed at text
    /// fields per this issue's scope, so this is a defensive fallback, not a
    /// path any fixture exercises.
    fn tokenize_for_target(&self, target: &FieldTarget, text: &str) -> Vec<String> {
        let schema = self.index.schema();
        let field_entry = schema.get_field_entry(target.field());
        let tokenizer_name = match field_entry.field_type() {
            tantivy::schema::FieldType::Str(text_options) => {
                text_options.get_indexing_options().map(|o| o.tokenizer())
            }
            // A dynamic `qf`/`pf` name lives inside a catch-all JSON
            // container, whose analyzer is declared on the container's own
            // `JsonObjectOptions` (`_dynamic_text` = `text_en`, `_dynamic` =
            // `raw`) — the same chain `generate_literals_for_json_object`
            // uses when a dynamic name reaches Tantivy's parser via the
            // query-text path.
            tantivy::schema::FieldType::JsonObject(json_options) => json_options
                .get_text_indexing_options()
                .map(|o| o.tokenizer()),
            _ => None,
        };
        let Some(mut tokenizer) = tokenizer_name.and_then(|name| self.index.tokenizers().get(name))
        else {
            return vec![text.to_string()];
        };
        let mut tokens = Vec::new();
        let mut token_stream = tokenizer.token_stream(text);
        token_stream.process(&mut |token: &tantivy::tokenizer::Token| {
            tokens.push(token.text.clone());
        });
        tokens
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
    ///
    /// `nested` carries the already-built inline nested queries (issue #137)
    /// that `local_params::extract_nested_queries` lifted out of the query
    /// string, indexed by sentinel; it is `NestedQueries::NONE` for every query
    /// with no local-params block, which is every caller other than
    /// `parse_query`.
    fn build_ast(
        &self,
        ast: tantivy::query_grammar::UserInputAst,
        parser: &QueryParser,
        default_field_name: &str,
        nested: &NestedQueries<'_>,
    ) -> Result<Box<dyn Query>, QueryError> {
        use tantivy::query_grammar::UserInputAst;
        match ast {
            UserInputAst::Clause(subqueries) => {
                let mut clauses = Vec::with_capacity(subqueries.len());
                for (occur_opt, sub) in subqueries {
                    let occur = occur_opt.unwrap_or(Occur::Should);
                    clauses.push((
                        occur,
                        self.build_ast(sub, parser, default_field_name, nested)?,
                    ));
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
                let built = self.build_ast(*inner, parser, default_field_name, nested)?;
                Ok(Box::new(BoostQuery::new(built, boost.into_inner() as f32)))
            }
            UserInputAst::Leaf(leaf) => self.build_leaf(*leaf, parser, default_field_name, nested),
        }
    }

    /// Builds one grammar leaf. A bare (`Delimiter::None`, no slop/prefix)
    /// literal is classified by `query::classify_literal` for fuzzy/wildcard/
    /// unclosed-regex; `UserInputLeaf::Exists` and an all-`Unbounded`
    /// `UserInputLeaf::Range` (`[* TO *]`) both become the field-exists idiom
    /// (finding 57/58); `UserInputLeaf::Regex` (a *closed* `/pattern/`,
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
    ///
    /// A leaf that is one of *this rewrite's* sentinel literals (issue #137 —
    /// see `NestedQueries`, which carries the possibly-re-keyed prefix)
    /// resolves to the already-built inline nested query it stands for,
    /// *before* any other classification: the sentinel occupies the exact
    /// clause position the `{!edismax ...}` block did, so the surrounding
    /// `+`/`-`/paren structure the outer parser derived applies to the nested
    /// query unchanged. A sentinel-shaped literal that is *not* one of this
    /// rewrite's own falls through and is parsed as ordinary text.
    fn build_leaf(
        &self,
        leaf: tantivy::query_grammar::UserInputLeaf,
        parser: &QueryParser,
        default_field_name: &str,
        nested: &NestedQueries<'_>,
    ) -> Result<Box<dyn Query>, QueryError> {
        use tantivy::query_grammar::{UserInputAst, UserInputBound, UserInputLeaf};

        if let UserInputLeaf::Literal(literal) = &leaf
            && literal.field_name.is_none()
            && let Some(query) = nested.resolve(&literal.phrase)
        {
            return Ok(query);
        }

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
    /// `field` (finding 57's field-exists idiom; also the range-syntax
    /// equivalent, finding 58's `range_str_star_both`). `ExistsQuery` needs
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

    /// `field:term~[N]` — finding 56. Lowercased (never stemmed) on an
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

    /// `[field:]glob` — finding 57. Lowercased (never stemmed) on an
    /// analyzed text field, left alone on `string`/`keyword`; a numeric/date
    /// field 400s (`qwild_int.json`'s "Can't run prefix queries on numeric
    /// fields" — there is no term dictionary to walk there). Constant-score,
    /// matching Lucene's own multi-term rewrite (finding 57).
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

    /// `field:/pattern/` — finding 57/59. Anchored whole-term, case-sensitive,
    /// no analysis at all, over the *indexed* (post-analysis, e.g. stemmed)
    /// terms; constant-score. A pattern that fails automaton compilation
    /// (e.g. an unbalanced character class) is finding 59's one 500, not a
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
    /// field query (finding 59's `phrase_with_colon`; the regression test is
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
    ///
    /// `*` in `fl` is a wildcard over every *stored* field — declared and
    /// dynamic alike — not a literal field name (issue #188;
    /// `solr-ref/responses/mlt_fl_wildcard_score.json`,
    /// `solr-ref/search-api/trace/00010.json`). It composes with named fields
    /// (naming a field the wildcard already covers is a no-op) and with
    /// `score`, and never widens the set beyond what an absent `fl` returns.
    ///
    /// Key order is schema order — declared `[[fields]]` first, then stored
    /// dynamic fields — with `score` appended *last*, after the dynamic fields
    /// (`solr-ref/search-api/trace/00010.json`). `fl`'s own member order never
    /// drives it (`solr-ref/responses/select_fl_reversed.json`).
    ///
    /// ponytail: `*` is the only glob understood. Solr also accepts a partial
    /// pattern (`fl=ss_*`, `fl=*_txt`) and the wildcard is per-`fl`-member
    /// there; here anything other than a bare `*` stays a literal name. No
    /// captured fixture sends a partial pattern.
    pub fn render_doc(
        &self,
        addr: DocAddress,
        fl: Option<&[String]>,
        score: Option<Score>,
    ) -> Result<Value> {
        let searcher = self.reader.searcher();
        let doc: TantivyDocument = searcher.doc(addr)?;

        // An absent `fl` and an `fl` containing `*` want the same field set, so
        // both loops below ask this rather than matching literal names.
        let wants =
            |name: &str| fl.is_none_or(|fl| fl.iter().any(|want| want == "*" || want == name));

        let wanted: Vec<&schema::FieldConfig> = self
            .wf_schema
            .fields
            .iter()
            .filter(|f| f.stored)
            .filter(|f| wants(&f.name))
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
                    if !stored || !wants(&name) {
                        continue;
                    }
                    out.insert(name, serde_json::to_value(&v)?);
                }
            }
        }

        // `score` only appears when `fl` explicitly names it, matching Solr:
        // requesting `fl=score` is what turns scoring output on at all, so a
        // caller passing a `Some(score)` without asking for it must still see
        // it omitted.
        //
        // Positioned last — after the dynamic-field loop above, not between it
        // and the schema-declared fields — because that is what captured Solr
        // does. `solr-ref/search-api/trace/00010.json` is a real `fl=*,score`
        // `/select` response over a corpus full of dynamic fields, and its doc
        // key order ends `..., "sm_field_keywords", "hash", "timestamp",
        // "ss_search_api_language", "score"`: `score` after every dynamic
        // field. That agrees with finding 24 (`docs/solr-ref-findings.md`) —
        // Solr appends its pseudo-fields (`_version_`, `_root_`, and `score`
        // itself) at the end — and with `mlt_fl_wildcard_score.json`, where
        // `score` follows `_version_`/`_root_`.
        if let Some(score) = score.filter(|_| fl.is_some_and(|fl| fl.iter().any(|f| f == "score")))
        {
            out.insert("score".to_string(), json!(score));
        }

        Ok(Value::Object(out))
    }

    /// Extracts the query's textual terms for `field`. With
    /// `cross_field_query_terms`, each text term is retargeted to the field
    /// being highlighted; otherwise only terms the query already targets at
    /// that field are retained.
    fn highlight_terms(
        &self,
        query: &dyn Query,
        field: Field,
        cross_field_query_terms: bool,
    ) -> Result<BTreeMap<String, Score>> {
        let mut term_texts = BTreeSet::new();
        query.query_terms(&mut |term, _| {
            if !cross_field_query_terms && term.field() != field {
                return;
            }
            if let Some(text) = term.value().as_str() {
                term_texts.insert(text.to_string());
            }
        });

        let searcher = self.reader.searcher();
        let mut terms = BTreeMap::new();
        for text in term_texts {
            let doc_freq = searcher.doc_freq(&Term::from_field_text(field, &text))?;
            if doc_freq > 0 {
                terms.insert(text, 1.0 / (1.0 + doc_freq as Score));
            }
        }
        Ok(terms)
    }

    /// Reproduces Solr original highlighter's default `LuceneGapFragmenter`
    /// plus Lucene `Highlighter::getBestTextFragments` selection. In
    /// particular, the queue keeps zero-score fragments: when
    /// `hl.mergeContiguous=true`, those selected bridges are what let the
    /// original highlighter coalesce the first two matching fragments without
    /// also coalescing a later one.
    #[allow(clippy::too_many_arguments)]
    fn original_highlight_fragments(
        &self,
        field: Field,
        text: &str,
        terms: &BTreeMap<String, Score>,
        max_num_chars: usize,
        pre: &str,
        post: &str,
        snippets_cap: usize,
        merge_contiguous: bool,
    ) -> Result<Vec<String>> {
        const MAX_SNIPPETS_PER_FIELD: usize = 100;

        let mut tokenizer = self.index.tokenizer_for_field(field)?;
        let mut stream = tokenizer.token_stream(text);
        let mut fragments = Vec::new();
        let mut current = OriginalHighlightFragment {
            range: 0..0,
            highlights: Vec::new(),
            score: 0.0,
        };
        let mut previous_end = 0;
        let mut fragment_offset = 0usize;
        let mut has_previous_token = false;
        let mut current_terms = BTreeSet::new();

        while stream.advance() {
            let token = stream.token();
            // Solr's default original fragmenter is `LuceneGapFragmenter`:
            // it tests the *current* token's end offset against the prior
            // break's current-token end offset. Lucene calls it only after
            // flushing the preceding token group, so a boundary belongs after
            // that preceding token and retains leading whitespace here.
            if has_previous_token
                && token.offset_to >= fragment_offset.saturating_add(max_num_chars)
            {
                current.range.end = previous_end;
                fragments.push(current);
                current = OriginalHighlightFragment {
                    range: previous_end..previous_end,
                    highlights: Vec::new(),
                    score: 0.0,
                };
                current_terms.clear();
                fragment_offset = token.offset_to;
            }

            // The analyzer already decides case normalization. Preserve its
            // token text so custom case-sensitive chains remain highlightable.
            let term_text = token.text.clone();
            if let Some(score) = terms.get(&term_text) {
                // Lucene QueryScorer scores a term only on its first
                // occurrence in a fragment, while marking every occurrence.
                if current_terms.insert(term_text) {
                    current.score += score;
                }
                current.highlights.push(token.offset_from..token.offset_to);
            }
            previous_end = token.offset_to;
            has_previous_token = true;
        }
        current.range.end = text.len();
        fragments.push(current);

        let wanted = snippets_cap.min(MAX_SNIPPETS_PER_FIELD);
        let mut ranked: Vec<(usize, OriginalHighlightFragment)> =
            fragments.into_iter().enumerate().collect();
        ranked.sort_by(|(left_number, left), (right_number, right)| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left_number.cmp(right_number))
        });
        let mut selected: Vec<Option<OriginalHighlightFragment>> = ranked
            .into_iter()
            .take(wanted)
            .map(|(_, fragment)| Some(fragment))
            .collect();

        if merge_contiguous {
            // This is `Highlighter::mergeContiguousFragments` in document
            // terms. It deliberately runs over the selected zero-score
            // bridges too; they are removed only after all merges finish.
            loop {
                let mut merged = false;
                'search: for i in 0..selected.len() {
                    let Some(current) = selected[i].as_ref() else {
                        continue;
                    };
                    for x in 0..selected.len() {
                        if i == x {
                            continue;
                        }
                        let Some(other) = selected[x].as_ref() else {
                            continue;
                        };
                        let pair = if current.range.end == other.range.start {
                            Some((i, x))
                        } else if other.range.end == current.range.start {
                            Some((x, i))
                        } else {
                            None
                        };
                        let Some((first, second)) = pair else {
                            continue;
                        };

                        let first_fragment = selected[first]
                            .take()
                            .expect("selected original fragment is present");
                        let second_fragment = selected[second]
                            .take()
                            .expect("selected original fragment is present");
                        let winner = if first_fragment.score > second_fragment.score {
                            first
                        } else {
                            second
                        };
                        let mut highlights = first_fragment.highlights;
                        highlights.extend(second_fragment.highlights);
                        selected[winner] = Some(OriginalHighlightFragment {
                            range: first_fragment.range.start..second_fragment.range.end,
                            highlights,
                            score: first_fragment.score + second_fragment.score,
                        });
                        merged = true;
                        break 'search;
                    }
                }
                if !merged {
                    break;
                }
            }
        }

        Ok(selected
            .into_iter()
            .flatten()
            .filter(|fragment| fragment.score > 0.0)
            .map(|fragment| render_original_highlight_fragment(text, fragment, pre, post))
            .collect())
    }

    /// Generates up to `snippets_cap` distinct highlighted HTML snippets for
    /// `field_name` in the doc at `addr`, against `query`'s terms in that
    /// field (Solr's `hl`/`hl.fl`). `snippets_cap` is Solr's `hl.snippets`:
    /// it caps and never pads (finding 53, `docs/solr-ref-findings.md`), so
    /// a field with fewer matches than the cap simply returns fewer. It is a
    /// parameter rather than the caller's `take()` because each snippet costs
    /// a full `SnippetGenerator` pass over the field text (see the ponytail
    /// below) -- extracting past what the request asked for is wasted work on
    /// every highlighted doc. Returns an empty `Vec` -- never a single
    /// empty-string entry -- when the field carries no term overlap for this
    /// doc (finding 52), when the field is not stored (silently, mirroring
    /// `render_doc`'s own omit-rather-than-null treatment of a missing stored
    /// value -- unfixture-backed, the conservative choice for a case no
    /// captured response exercises), or when `snippets_cap` is 0.
    ///
    /// Tantivy's public `SnippetGenerator` only ever hands back the single
    /// best-scoring fragment (`select_best_fragment_combination` is a private
    /// fn, `tantivy-0.26.1/src/snippet/mod.rs`), so more than one fragment is
    /// obtained by mask-and-resnippet: take the best fragment, blank the
    /// matched byte ranges it consumed out of a mutable copy of the field
    /// text (with spaces, so every other byte offset -- and UTF-8 validity --
    /// stays put), and re-run the generator over the remainder. Each pass
    /// therefore retires at least one occurrence, which is what bounds the
    /// loop; masked spans can never be re-matched, so no fragment repeats.
    ///
    /// ponytail: mask-and-resnippet is not a real multi-fragment scorer.
    /// Four known gaps, all accepted because no captured fixture
    /// discriminates them (`hl_snippets_two.json`'s query has exactly one hit
    /// per doc per field):
    ///
    /// 1. Fragment *selection* is greedy, one Tantivy call per fragment, and
    ///    ordering falls out of Tantivy's own tie-break (best score, then
    ///    earliest offset) applied to a shrinking text -- not Solr's
    ///    multi-fragment scoring or ordering, and with no minimum-gap-between-
    ///    fragments notion at all. Occurrences closer together than
    ///    `max_num_chars` land in one fragment and come back as a single
    ///    multi-highlight snippet, where Solr might split them.
    /// 2. Cost is one `SnippetGenerator` pass over the whole field text per
    ///    snippet returned. `snippets_cap` bounds that, so the ordinary
    ///    `hl.snippets=1` request costs exactly the single pass it did before
    ///    multi-snippet support existed; only a request that asks for more
    ///    pays for more. A cap-sized request over a long field is still
    ///    O(cap * field bytes), which a real fragmenter would do in one pass.
    /// 3. `MAX_SNIPPETS_PER_FIELD` is a defensive outer ceiling on that loop
    ///    regardless of `snippets_cap`, so an `hl.snippets` above it silently
    ///    caps there rather than at the true occurrence count.
    /// 4. Divergence, not just a simplification: two *distinct* occurrences
    ///    whose fragments happen to render byte-identically (repeated
    ///    boilerplate, far enough apart to fragment separately) are collapsed
    ///    into one entry here, where Solr returns both. Unfixture-backed --
    ///    no captured response has a field with duplicate surroundings -- and
    ///    de-duplicating was judged the more useful answer, but it is a real
    ///    difference in returned snippet count, not a wash.
    // Same call as `parse_edismax_query` above: these are Solr request
    // parameters arriving one-per-`hl.*`-param, and bundling them into a
    // struct would only move the arity somewhere else.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn highlight_field(
        &self,
        query: &dyn Query,
        addr: DocAddress,
        field_name: &str,
        max_num_chars: usize,
        pre: &str,
        post: &str,
        snippets_cap: usize,
    ) -> Result<Vec<String>> {
        self.highlight_field_with_options(
            query,
            addr,
            field_name,
            max_num_chars,
            pre,
            post,
            snippets_cap,
            false,
            false,
            false,
        )
    }

    /// The parameter-aware highlighting primitive. An explicit
    /// `hl.requireFieldMatch=false` supplies query terms from every queried
    /// field; otherwise terms stay scoped to `field_name`. `hl.method=original`
    /// selects Lucene original-highlighter fragment selection, including its
    /// selected zero-score bridges for `hl.mergeContiguous=true`.
    #[allow(clippy::too_many_arguments)]
    pub fn highlight_field_with_options(
        &self,
        query: &dyn Query,
        addr: DocAddress,
        field_name: &str,
        max_num_chars: usize,
        pre: &str,
        post: &str,
        snippets_cap: usize,
        cross_field_query_terms: bool,
        original_fragments: bool,
        merge_contiguous: bool,
    ) -> Result<Vec<String>> {
        /// Defensive outer ceiling on the mask-and-resnippet loop, so a
        /// pathological request (`hl.snippets=100000`) over a field repeating
        /// the same term costs a bounded number of full-text
        /// `SnippetGenerator` passes. Chosen well above any plausible
        /// `hl.snippets` -- Solr's own default is 1, and the captured Search
        /// API traffic asks for 3 -- so ordinary requests are bounded by
        /// `snippets_cap`, never by this.
        const MAX_SNIPPETS_PER_FIELD: usize = 100;

        let field = self
            .wf_schema
            .field(field_name)
            .ok_or_else(|| anyhow!("can not highlight undefined field: {field_name}"))?;
        let terms = self.highlight_terms(query, field, cross_field_query_terms)?;
        let searcher = self.reader.searcher();
        let mut generator = SnippetGenerator::new(
            terms.clone(),
            self.index.tokenizer_for_field(field)?,
            field,
            max_num_chars,
        );
        generator.set_max_num_chars(max_num_chars);
        let doc: TantivyDocument = searcher.doc(addr)?;

        // The same text `SnippetGenerator::snippet_from_doc` would assemble
        // for this field: every stored string value, space-joined and
        // trimmed. Reproduced here (rather than calling `snippet_from_doc`)
        // because masking needs to own the text across passes.
        let mut text = String::new();
        for value in doc.get_all(field) {
            if let Some(s) = value.as_str() {
                text.push(' ');
                text.push_str(s);
            }
        }
        let mut text = text.trim().to_string();

        // Whole-field mode (Solr's `hl.fragsize=0`, finding 81): the entire
        // field, unfragmented, as one snippet -- so the mask-and-resnippet
        // loop below is not entered. `WHOLE_FIELD_MAX_CHARS` already makes
        // Tantivy build a single fragment candidate spanning every token, so
        // one pass carries every matching occurrence's range. What it does
        // *not* carry is text outside the first/last token boundary: the
        // fragment stops at the last token's `offset_to`, dropping a field's
        // trailing "." (real Solr keeps it --
        // `solr-ref/responses/hl_fragsize_zero_whole_field*` end in one). So
        // the fragment's own HTML is re-seated inside the untouched field
        // text, with the leading/trailing remainder encoded the same way
        // `Snippet::to_html` encoded the rest.
        //
        // ponytail: returning exactly one snippet here -- ignoring
        // `snippets_cap` rather than bounding anything with it -- is an
        // *inference*, not a captured fact. It follows from "the whole field
        // is the fragment", so there is no second fragment to return, but none
        // of the three issue-#104 fixtures sends `hl.snippets`, so real Solr's
        // answer to `hl.fragsize=0&hl.snippets=3` is uncaptured (finding 81
        // records this same caveat). Revisit if that combination is ever
        // captured.
        if max_num_chars == WHOLE_FIELD_MAX_CHARS {
            let mut snippet = generator.snippet(&text);
            if snippet.is_empty() {
                return Ok(Vec::new());
            }
            // A slice of `text` by construction (same reasoning as the loop
            // below), so `find` succeeds; returning no snippet rather than
            // panicking keeps a future Tantivy change from taking the process
            // down.
            let Some(base) = text.find(snippet.fragment()) else {
                return Ok(Vec::new());
            };
            let end = base + snippet.fragment().len();
            snippet.set_snippet_prefix_postfix(pre, post);
            // `base` is 0 for every input reachable today -- the fragment
            // splitter never fires under this sentinel, so the single
            // candidate starts at the first token, and `text` is already
            // trimmed, so the head slice is always empty. It is still handled
            // rather than assumed away because the tail is *not* always empty
            // (that trailing "." is the whole reason this branch exists), so
            // the fragment is not the full text in general -- kept symmetric
            // with the tail in case a future Tantivy version ever produces a
            // non-zero `base` here.
            let mut html = encode_minimal(&text[..base]);
            html.push_str(&snippet.to_html());
            html.push_str(&encode_minimal(&text[end..]));
            return Ok(vec![html]);
        }

        if original_fragments {
            return self.original_highlight_fragments(
                field,
                &text,
                &terms,
                max_num_chars,
                pre,
                post,
                snippets_cap,
                merge_contiguous,
            );
        }

        // `snippets_cap` is the real bound; the iteration count is capped
        // separately because a de-duplicated pass (gap 4) consumes a pass
        // without filling a slot, so passes are not one-to-one with snippets.
        let wanted = snippets_cap.min(MAX_SNIPPETS_PER_FIELD);
        let mut snippets: Vec<String> = Vec::new();
        for _ in 0..MAX_SNIPPETS_PER_FIELD {
            if snippets.len() >= wanted {
                break;
            }
            let mut snippet = generator.snippet(&text);
            if snippet.is_empty() {
                break;
            }
            // `Snippet` exposes its highlights relative to the fragment, not
            // to the field text, so the fragment has to be located again.
            // It is a slice of `text` by construction, so `find` succeeds;
            // bailing out instead of masking nothing keeps the loop
            // guaranteed to terminate even if that ever stops holding.
            let Some(base) = text.find(snippet.fragment()) else {
                break;
            };
            let masked: Vec<Range<usize>> = snippet
                .highlighted()
                .iter()
                .map(|r| base + r.start..base + r.end)
                .collect();
            snippet.set_snippet_prefix_postfix(pre, post);
            let html = snippet.to_html();
            // Masking already stops the *same* occurrence coming back twice,
            // so this only fires for two genuinely different occurrences whose
            // fragments render identically -- and dropping the second is the
            // documented divergence from Solr in gap 4 above, which returns
            // both. No corpus in the suite hits it (deleting this guard leaves
            // every test green).
            if !snippets.contains(&html) {
                snippets.push(html);
            }
            let mut bytes = text.into_bytes();
            for range in masked {
                // Same-length ASCII blanking: the range is a token boundary,
                // so overwriting it with spaces leaves the rest of the text
                // byte-identical and still valid UTF-8. The range came from a
                // fragment located inside this very text, so it is in bounds
                // by construction -- skipping it silently would burn the
                // remaining passes re-finding the same match.
                bytes
                    .get_mut(range)
                    .expect("a highlight range from the located fragment lies inside the text")
                    .fill(b' ');
            }
            text = String::from_utf8(bytes)
                .context("masking a highlighted range produced invalid UTF-8")?;
        }
        Ok(snippets)
    }

    /// Builds the `/mlt` similarity query for `addr` — mines terms from
    /// `field_names`'s stored values (every declared field if absent, from
    /// `mlt.fl`), tuned by `opts`.
    ///
    /// ponytail: `MoreLikeThis::stop_words` is filled with a fixed Lucene
    /// English stopword list unconditionally, rather than deriving one from
    /// the field's own analyzer. Built-in `text_en` now removes these words at
    /// index time, but `/mlt` can mine custom analyzers and other language
    /// presets that intentionally retain them. Keeping the MLT noise list
    /// independent preserves its existing term-selection behavior without
    /// changing those analyzer contracts.
    ///
    /// ponytail: reimplements (rather than calls) Tantivy's own
    /// `MoreLikeThis`/`MoreLikeThisQuery` algorithm. That type's containing
    /// module (`tantivy::query::more_like_this`) is private — only
    /// `MoreLikeThisQuery`/`MoreLikeThisQueryBuilder` are re-exported — and
    /// the builder offers no way to get `boost_factor: None` (Solr's
    /// `mlt.boost=false` default; the builder's own default is always
    /// `Some(1.0)`, since `MoreLikeThis::boost_factor` itself is private and
    /// `with_boost_factor(f32)` can only ever produce `Some`). That is the
    /// one real gap in the public builder API; `with_stop_words` *is* public,
    /// so the stop-word list alone would not have forced a reimplementation.
    /// This mirrors Tantivy 0.26.1's private algorithm term-for-term
    /// (tokenize each mined field with its own indexing analyzer, drop noise
    /// words, threshold by term/doc frequency, score by `tf * idf` with the
    /// identical BM25-style `idf` formula, keep the top `max_query_terms`),
    /// so it should track upstream's actual output; revisit if that
    /// algorithm becomes public or gains a `with_boost_factor(None)` escape
    /// hatch.
    ///
    /// Returns both the composed query and the scored terms it was built
    /// from (highest-scored first, already truncated to `max_query_terms`)
    /// so a caller rendering `mlt.interestingTerms` has real weighted-term
    /// data to work with rather than needing to re-derive it.
    pub fn mlt_query(
        &self,
        addr: DocAddress,
        field_names: Option<&[String]>,
        opts: MltOptions,
    ) -> Result<(BooleanQuery, Vec<(Term, Score)>)> {
        let searcher = self.reader.searcher();
        let doc: TantivyDocument = searcher.doc(addr)?;
        let schema = searcher.schema();
        let tokenizer_manager = searcher.index().tokenizers();

        let mut term_frequencies: HashMap<Term, usize> = HashMap::new();
        for field_config in &self.wf_schema.fields {
            if field_names.is_some_and(|names| !names.iter().any(|n| n == &field_config.name)) {
                continue;
            }
            let Some(field) = self.wf_schema.field(&field_config.name) else {
                continue;
            };
            let field_entry = schema.get_field_entry(field);
            if !field_entry.is_indexed() {
                continue;
            }
            let tantivy::schema::FieldType::Str(text_options) = field_entry.field_type() else {
                continue;
            };
            let Some(mut tokenizer) = text_options
                .get_indexing_options()
                .map(|o| o.tokenizer())
                .and_then(|name| tokenizer_manager.get(name))
            else {
                continue;
            };
            for value in doc.get_all(field) {
                let Some(text) = value.as_str() else {
                    continue;
                };
                let mut token_stream = tokenizer.token_stream(text);
                token_stream.process(&mut |token: &tantivy::tokenizer::Token| {
                    if mlt_is_noise_word(&token.text, opts.min_word_length, opts.max_word_length) {
                        return;
                    }
                    let term = Term::from_field_text(field, &token.text);
                    *term_frequencies.entry(term).or_insert(0) += 1;
                });
            }
        }

        let num_docs: u64 = searcher
            .segment_readers()
            .iter()
            .map(|r| r.num_docs() as u64)
            .sum();

        let mut score_terms: Vec<(Term, Score)> = Vec::new();
        for (term, term_frequency) in term_frequencies {
            if opts
                .min_term_frequency
                .is_some_and(|min| term_frequency < min)
            {
                continue;
            }
            let doc_freq = searcher.doc_freq(&term)?;
            if opts.min_doc_frequency.is_some_and(|min| doc_freq < min) {
                continue;
            }
            if opts.max_doc_frequency.is_some_and(|max| doc_freq > max) {
                continue;
            }
            if doc_freq == 0 {
                continue;
            }
            let idf = mlt_idf(doc_freq, num_docs);
            score_terms.push((term, term_frequency as Score * idf));
        }
        score_terms.sort_by(|(_, left), (_, right)| {
            right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(limit) = opts.max_query_terms {
            score_terms.truncate(limit);
        }

        let best_score = score_terms.first().map_or(1.0, |(_, score)| *score);
        let mut clauses: Vec<(tantivy::query::Occur, Box<dyn Query>)> = Vec::new();
        for (term, score) in &score_terms {
            let mut clause: Box<dyn Query> = Box::new(tantivy::query::TermQuery::new(
                term.clone(),
                tantivy::schema::IndexRecordOption::Basic,
            ));
            if let Some(factor) = opts.boost_factor {
                clause = Box::new(tantivy::query::BoostQuery::new(
                    clause,
                    score * factor / best_score,
                ));
            }
            clauses.push((tantivy::query::Occur::Should, clause));
        }
        Ok((BooleanQuery::from(clauses), score_terms))
    }

    /// Number of documents matching `query`. The counting primitive behind
    /// `facet.query`, `facet.missing` and each `facet.range` bucket, all of
    /// which are "how many docs also match this extra constraint?".
    pub fn count(&self, query: &dyn Query) -> Result<usize> {
        Ok(self.reader.searcher().search(query, &Count)?)
    }

    /// Live document count as of the last commit — the same searcher the
    /// query pipeline reads from, so the admin UI (issue #94) cannot drift
    /// from what `/select` reports. Deleted/overwritten docs are excluded:
    /// `Searcher::num_docs` sums each segment's alive docs.
    ///
    /// Read-only: taking a searcher neither commits nor reloads.
    pub fn doc_count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Number of searchable segments backing the current searcher.
    ///
    /// Read off the same searcher `doc_count` uses, so the admin UI's segment
    /// count is the one the query pipeline is actually reading from — not a
    /// fresh `meta.json` read that could disagree with the open reader. The
    /// reader is `ReloadPolicy::Manual` and reloaded on commit (see `open`),
    /// so this changes only when a commit does, exactly as the doc count does.
    ///
    /// Read-only: taking a searcher neither commits nor reloads.
    pub fn segment_count(&self) -> usize {
        self.reader.searcher().segment_readers().len()
    }

    /// Deleted-but-not-yet-reclaimed documents behind the current searcher —
    /// Lucene's `deletedDocs`, and the difference between `maxDoc` and
    /// `numDocs` (`/admin/luke`, issue #157).
    ///
    /// Summed off the same searcher's segment readers `doc_count` and
    /// `segment_count` use, so all three agree about which commit they
    /// describe. A tantivy delete tombstones the doc in its segment's alive
    /// bitset; the row itself goes away only when a merge rewrites the
    /// segment, which is exactly Lucene's semantics for this figure — so this
    /// is a read of real bookkeeping, not a counter Wayfinder maintains.
    ///
    /// Read-only: taking a searcher neither commits nor reloads.
    pub fn deleted_doc_count(&self) -> u64 {
        self.reader
            .searcher()
            .segment_readers()
            .iter()
            .map(|reader| u64::from(reader.num_deleted_docs()))
            .sum()
    }

    /// Every term in `field_name`'s **inverted index** term dictionary, with
    /// its document frequency — Solr's TermsComponent (`/terms`, issue #155).
    ///
    /// Deliberately *not* `term_facet`: that runs a docValues
    /// `TermsAggregation` over a fast column, which never sees the analyzed
    /// tokens of a `text_en` field (`lazy` -> `lazi`, `archived` -> `archiv`).
    /// The trace this endpoint matches
    /// (`solr-ref/search-api/trace/00028.json`) reports stemmed terms, so the
    /// term dictionary is the only source that can answer it.
    ///
    /// Two properties the ticket calls out, both encoded here:
    ///
    /// - **Doc frequencies sum across segments.** One term generally lives in
    ///   several segments' dictionaries, each with its own local `doc_freq`;
    ///   Solr reports the total, so the `BTreeMap` accumulates rather than
    ///   overwrites.
    /// - **Deleted documents still count.** `TermInfo::doc_freq` is the raw
    ///   Lucene `docFreq`: deleting a document tombstones it in the segment's
    ///   alive-docs bitmap without rewriting the postings, so the frequency
    ///   only drops when a merge physically purges it. Solr's TermsComponent
    ///   behaves the same way (it reads `docFreq`, not a live-docs-filtered
    ///   count), so intersecting with `SegmentReader::alive_bitset` here would
    ///   be a divergence, not a fix. `tests/terms.rs`'s
    ///   `terms_doc_frequency_includes_deleted_docs_without_a_merge` guards it.
    ///
    /// Returned as a `BTreeMap` — unlimited and term-ascending. `terms.limit`
    /// and `terms.sort` are response-shaping concerns and live in the handler,
    /// which relies on the term-ascending iteration order for the
    /// count-descending sort's tie-break.
    ///
    /// **Text fields only, enforced.** A term dictionary holds whatever bytes
    /// the field's indexing wrote, and only a `string`/`text_*` field writes
    /// UTF-8 there: a numeric or date field's terms are Tantivy's
    /// order-preserving fixed-width encoding, and a JSON catch-all's are
    /// path-prefixed and type-tagged. Decoding those lossily does not merely
    /// render badly — two distinct encoded terms can collapse onto the same
    /// replacement-character string and have their unrelated document
    /// frequencies summed into one `BTreeMap` key, which is a wrong answer, not
    /// an ugly one. So this errors on non-UTF-8 bytes rather than substituting
    /// `U+FFFD`, and `lib.rs`'s `terms` handler refuses a non-text `terms.fl`
    /// with a 400 before it ever gets here (`check_terms_field`, following
    /// `stats::check_statable`'s precedent). The check here is the backstop that
    /// makes the ceiling real for any other future caller.
    ///
    /// **Dynamic names resolve too**, through the same `field_target` the
    /// `/select` path uses (issue #155's follow-up: `terms.fl=tm_X3b_en_title`
    /// used to 400 as an undefined field even though `q=tm_X3b_en_title:lazy`
    /// worked on the same core). A dynamic name has no term dictionary of its
    /// own — every name matching a `[[dynamic_fields]]` rule shares the rule's
    /// catch-all JSON container (`_dynamic_text`/`_dynamic`), and the *whole*
    /// address lives inside each dictionary entry. Verified against
    /// `tantivy` 0.26.1's own encoding, not assumed: a JSON field's dictionary
    /// key is `Term::serialized_value_bytes()`, which for
    /// `[type code=JSON][JSON path][JSON_END_OF_PATH][ValueBytes]`
    /// (`schema/term.rs:298`) minus the leading type tag
    /// (`TERM_TYPE_TAG_LEN = 1`) is
    /// `<path><JSON_END_OF_PATH=0x00><Type::Str=b's'><term utf-8>`. So
    /// `tm_X3b_en_title`'s `lazi` is stored as `tm_X3b_en_title\0slazi`.
    ///
    /// The prefix is therefore built by `term_for_target(target, "")` — the
    /// exact same constructor the query path uses to look a term up, so the two
    /// cannot drift — and matched **byte-for-byte including the terminating
    /// `0x00` and `b's'`**. That anchoring is what keeps two names under one
    /// rule apart: no other JSON path can produce those bytes at that offset,
    /// since `0x00` terminates the path and cannot occur inside it. A prefix of
    /// just the name would also match `tm_X3b_en_title_extra`; a split on the
    /// first `0x00` without checking the path would match every field in the
    /// container. `tests/terms.rs`'s
    /// `terms_dynamic_fields_do_not_leak_across_the_shared_catch_all_container`
    /// guards it.
    ///
    /// Dictionary keys are byte-ordered, so all of one path's entries are
    /// contiguous: the scan seeks to `>= prefix` and stops at the first key
    /// that is not prefixed, rather than walking the shared container's whole
    /// vocabulary.
    ///
    /// ponytail: the whole dictionary is materialised in memory before the
    /// handler truncates it to `terms.limit`. Ceiling: a field with a very
    /// large vocabulary pays for every term on every request. Solr streams and
    /// stops early. Revisit if `/terms` ever serves a real autocomplete load
    /// (PRD v3's suggester work) rather than the module's handshake-sized
    /// requests.
    pub fn field_terms(&self, field_name: &str) -> Result<BTreeMap<String, u64>> {
        let target = self
            .field_target(field_name)
            .ok_or_else(|| anyhow!("undefined field \"{field_name}\""))?;
        let searcher = self.reader.searcher();
        let mut totals: BTreeMap<String, u64> = BTreeMap::new();
        // Empty for a static field, `<path>\0s` for a dynamic one — so the
        // static case strips nothing and the dynamic case strips exactly the
        // address Tantivy prepended.
        let prefix: Vec<u8> = match &target {
            FieldTarget::Static(_) => Vec::new(),
            FieldTarget::Dynamic { .. } => self
                .term_for_target(&target, "")
                .serialized_value_bytes()
                .to_vec(),
        };
        for segment_reader in searcher.segment_readers() {
            let inverted = segment_reader.inverted_index(target.field())?;
            let mut stream = inverted.terms().range().ge(&prefix).into_stream()?;
            while stream.advance() {
                let Some(rest) = stream.key().strip_prefix(prefix.as_slice()) else {
                    // Keys are byte-ordered, so the prefixed run is over.
                    break;
                };
                let term = std::str::from_utf8(rest).with_context(|| {
                    format!(
                        "field `{field_name}` has a term dictionary entry that is not UTF-8: \
                         /terms can only enumerate a text field"
                    )
                })?;
                *totals.entry(term.to_string()).or_insert(0) += u64::from(stream.value().doc_freq);
            }
        }
        Ok(totals)
    }

    /// Total bytes of this core's index directory.
    ///
    /// ponytail: a plain recursive `std::fs` walk of the data dir, summing
    /// file lengths. Ceiling: it is O(files) per call with no caching, counts
    /// apparent (not allocated) size, follows nothing but the directory tree,
    /// and silently skips entries it cannot stat.
    ///
    /// Decision, at the index-stats milestone this comment used to defer to
    /// (PRD §5 v2.5, issue #129): **keep it uncached**. Both callers are
    /// human-paced admin pages (`GET /ui`, `GET /ui/stats`) on a single-core
    /// process — there is no metrics endpoint, no auto-refresh, and nothing
    /// on a `/select` path calls this, so the walk is bounded by one operator
    /// clicking a link. A cache would buy nothing measurable and would cost
    /// an invalidation rule (the directory changes on merges and background
    /// segment deletes, not just on commits), which is a staleness bug
    /// waiting to happen: a size figure that silently lags reality is worse
    /// than a walk nobody is timing. Revisit when a *polled* consumer appears
    /// — a Prometheus/metrics endpoint or a self-refreshing page — since that
    /// is the change that makes the per-call cost real.
    pub fn disk_size_bytes(&self) -> u64 {
        dir_size_bytes(&self.data_dir)
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
    ///
    /// `kind` is the caller-resolved `ValueKind` backing `field_name` (issue
    /// #66: `field_name` may be a dynamic-only field's catch-all JSON column,
    /// e.g. `_dynamic.ss_lang`, which carries no schema entry of its own to
    /// look `value_kind` up from — so the caller resolves it via
    /// `WayfinderSchema::resolved_value_kind` against the *original* field
    /// name and passes the result in, rather than this fn re-deriving it from
    /// `field_name` directly).
    pub fn term_facet(
        &self,
        field_name: &str,
        kind: Option<ValueKind>,
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
    use std::collections::HashSet;

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

    /// `score` goes last, after the stored *dynamic* fields.
    ///
    /// History: this was
    /// `render_doc_orders_score_after_stored_fields_and_before_dynamic_fields`,
    /// added by `1511137 feat(schema): complete the v1 schema layer` as a
    /// characterization test pinning the pre-#188 order — `score` inserted
    /// between the declared-field loop and the dynamic-field loop. That
    /// placement was explicitly flagged as an unverified assumption (the
    /// `ponytail:` comment that used to sit at the insertion point said no
    /// captured fixture discriminated the two placements, since no scored
    /// fixture had a dynamic field).
    ///
    /// `solr-ref/search-api/trace/00010.json` overturns it: a real `fl=*,score`
    /// `/select` response whose doc keys end `..., "sm_field_keywords", "hash",
    /// "timestamp", "ss_search_api_language", "score"` — `score` after every
    /// dynamic field. #188 is what first makes `fl=*` reach the dynamic loop,
    /// so it is the first change for which the position is observable. The
    /// assertion is inverted rather than deleted, so the decision the earlier
    /// commit made on purpose stays visible and stays pinned in its corrected
    /// form.
    #[test]
    fn render_doc_orders_score_last_after_dynamic_fields() {
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
            vec!["id", "body", "extra_s", "score"],
            "`score` must be appended last, after every dynamic-field key \
             (`solr-ref/search-api/trace/00010.json`)"
        );
    }

    /// `segment_count` must track the index's *real* segment count, not the
    /// `1` that a single-commit corpus happens to have —
    /// `tests/admin_ui_index_stats.rs`'s black-box oracle is real, but the
    /// 5-doc fixture it runs against commits once, so a hardcoded `1` would
    /// survive it. This builds a deliberately multi-segment index
    /// (`merge_policy = "no_merge"`, one commit per doc) and compares against
    /// a fresh, independent `tantivy::Index::open_in_dir` read of the same
    /// directory — never against the searcher the implementation itself uses.
    #[test]
    fn segment_count_tracks_a_multi_segment_index() {
        let dir = TempDir::new().expect("create temp dir");
        let schema_path = dir.path().join("schema.toml");
        std::fs::write(&schema_path, SCHEMA_TOML).expect("write schema.toml");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let config = ServerConfig::parse("[indexing]\nmerge_policy = \"no_merge\"\n")
            .expect("no_merge config is valid");
        let index = CoreIndex::open(&schema_path, &data_dir, &config).expect("open test index");

        assert_eq!(
            index.segment_count(),
            0,
            "a core with nothing committed has no searchable segments"
        );

        for i in 0..3 {
            index
                .add_documents(&[json!({"id": format!("doc{i}"), "body": "quick"})], true)
                .expect("add_documents");
            index.commit().expect("commit");

            let expected = Index::open_in_dir(&data_dir)
                .expect("independent oracle opens the committed directory")
                .searchable_segment_metas()
                .expect("independent oracle lists searchable segment metas")
                .len();
            assert_eq!(
                index.segment_count(),
                expected,
                "after {} commit(s) the reported segment count must equal the \
                 real one",
                i + 1
            );
        }

        assert!(
            index.segment_count() > 1,
            "with `no_merge` and one commit per doc the index must be \
             genuinely multi-segment, or this test cannot catch a hardcoded \
             count"
        );
    }

    /// `field_terms`'s own UTF-8 backstop, tested directly rather than through
    /// the handler.
    ///
    /// `lib.rs::check_terms_field` 400s a non-text `terms.fl` before
    /// `field_terms` is ever called, so nothing on the HTTP path can reach
    /// these bytes — which is exactly why this needs a unit test. Without one
    /// the backstop is untested code that a later caller (a `/terms` that
    /// grows `terms.raw`, a suggester, an admin page) could quietly reintroduce
    /// the lossy decode behind. A numeric field's dictionary holds Tantivy's
    /// fixed-width order-preserving encoding: `String::from_utf8_lossy` turned
    /// that into `U+FFFD`-prefixed keys, and because distinct encoded terms can
    /// decode to the *same* replacement string, their unrelated document
    /// frequencies were summed onto one `BTreeMap` key. An error is the only
    /// honest result.
    #[test]
    fn field_terms_refuses_a_non_utf8_term_dictionary() {
        const NUMERIC_SCHEMA_TOML: &str = r#"
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
name = "views"
type = "int"
stored = true
fast = true
"#;
        let dir = TempDir::new().expect("create temp dir");
        let schema_path = dir.path().join("schema.toml");
        std::fs::write(&schema_path, NUMERIC_SCHEMA_TOML).expect("write schema.toml");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let index = CoreIndex::open(&schema_path, &data_dir, &ServerConfig::default())
            .expect("open test index");
        index
            .add_documents(
                &[
                    json!({"id": "n1", "body": "alpha", "views": 1}),
                    json!({"id": "n2", "body": "beta", "views": 300}),
                ],
                true,
            )
            .expect("add_documents");
        index.commit().expect("commit");

        // The text field still works, so this test is not just asserting that
        // `field_terms` is broken for everything.
        let text_terms = index.field_terms("body").expect("a text field enumerates");
        assert_eq!(
            text_terms.get("alpha").copied(),
            Some(1),
            "the text field must still enumerate normally: {text_terms:?}"
        );

        let err = index
            .field_terms("views")
            .expect_err("an int field's term dictionary is not UTF-8 and must error");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("views") && chain.contains("UTF-8"),
            "the error must name the field and say why, so a caller can tell \
             this from a generic IO failure, got {chain:?}"
        );
    }

    /// An independent, deliberately different implementation of the directory
    /// walk `dir_size_bytes` performs: iterative with an explicit stack rather
    /// than recursive, and `unwrap`ing where the real one skips. Used as the
    /// oracle for the size tests below so a bug in `dir_size_bytes` cannot be
    /// masked by the test computing its expectation with the same code.
    fn walk_size_oracle(dir: &Path) -> u64 {
        let mut stack = vec![dir.to_path_buf()];
        let mut total = 0u64;
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).expect("oracle reads a readable dir") {
                let entry = entry.expect("oracle reads a readable entry");
                // `DirEntry::metadata` does not traverse symlinks, so a link
                // is neither a file nor a dir here — the oracle ignores it for
                // the same reason the real walk does.
                let meta = entry.metadata().expect("oracle stats a readable entry");
                if meta.is_dir() {
                    stack.push(entry.path());
                } else if meta.is_file() {
                    total += meta.len();
                }
            }
        }
        total
    }

    /// The recursive walk must descend into subdirectories and sum every
    /// level, not just the top one. Nested two deep with a different-sized
    /// file at each level, so a walk that stops early produces a *different*
    /// non-zero number rather than accidentally matching.
    #[test]
    fn dir_size_bytes_sums_nested_subdirectories() {
        let dir = TempDir::new().expect("create temp dir");
        let root = dir.path();
        std::fs::write(root.join("top.bin"), vec![b'a'; 100]).expect("write top.bin");
        let mid = root.join("mid");
        std::fs::create_dir(&mid).expect("create mid");
        std::fs::write(mid.join("mid.bin"), vec![b'b'; 2000]).expect("write mid.bin");
        let deep = mid.join("deep");
        std::fs::create_dir(&deep).expect("create deep");
        std::fs::write(deep.join("deep.bin"), vec![b'c'; 30_000]).expect("write deep.bin");

        assert_eq!(
            dir_size_bytes(root),
            32_100,
            "every level of the tree must contribute its files' lengths"
        );
        assert_eq!(
            dir_size_bytes(root),
            walk_size_oracle(root),
            "the recursive walk must agree with an independent iterative walk"
        );
    }

    /// A single file of known length contributes exactly that many bytes —
    /// apparent size, no block rounding, no per-entry overhead.
    #[test]
    fn dir_size_bytes_counts_a_file_of_known_length_exactly() {
        let dir = TempDir::new().expect("create temp dir");
        let root = dir.path();
        assert_eq!(
            dir_size_bytes(root),
            0,
            "an empty directory is zero bytes, not an error"
        );

        std::fs::write(root.join("known.bin"), vec![0u8; 4096]).expect("write known.bin");
        assert_eq!(
            dir_size_bytes(root),
            4096,
            "one 4096-byte file must total exactly 4096"
        );

        std::fs::write(root.join("second.bin"), vec![0u8; 7]).expect("write second.bin");
        assert_eq!(
            dir_size_bytes(root),
            4103,
            "a second file adds exactly its own length"
        );
    }

    /// A directory that cannot be read — because it does not exist, because
    /// the path is a file, or because the process lacks permission — yields 0
    /// rather than panicking or propagating. The admin page is display-only
    /// (see `dir_size_bytes`'s doc comment); a stat failure must not 500 it.
    #[test]
    fn dir_size_bytes_returns_zero_for_an_unreadable_directory() {
        let dir = TempDir::new().expect("create temp dir");
        let root = dir.path();

        assert_eq!(
            dir_size_bytes(&root.join("no-such-dir")),
            0,
            "a nonexistent directory must be 0, not a panic"
        );

        let file = root.join("not-a-dir");
        std::fs::write(&file, vec![0u8; 512]).expect("write not-a-dir");
        assert_eq!(
            dir_size_bytes(&file),
            0,
            "a path that is a file, not a directory, must be 0, not a panic"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = root.join("locked");
            std::fs::create_dir(&locked).expect("create locked");
            std::fs::write(locked.join("hidden.bin"), vec![0u8; 1234]).expect("write hidden.bin");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
                .expect("chmod 000");

            let observed = dir_size_bytes(&locked);
            let readable_anyway = std::fs::read_dir(&locked).is_ok();

            // Restore before asserting, so a failure still leaves `TempDir`
            // able to clean up after itself.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
                .expect("restore mode");

            // A process running as root can read a 0o000 directory regardless,
            // in which case there is no permission failure to assert on.
            if !readable_anyway {
                assert_eq!(
                    observed, 0,
                    "an unreadable directory must be 0, not a panic"
                );
            }
        }
    }

    /// Symlinks must not inflate the total: `DirEntry::metadata` reports the
    /// link itself (not its target), so a link is neither a file nor a
    /// directory to the walk. A link to a file must not add the target's
    /// length a second time, and a link to a directory must not be descended
    /// into (which would double-count, and could loop forever on a cycle).
    #[cfg(unix)]
    #[test]
    fn dir_size_bytes_does_not_double_count_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("create temp dir");
        let root = dir.path();
        let real_file = root.join("real.bin");
        std::fs::write(&real_file, vec![0u8; 900]).expect("write real.bin");
        let sub = root.join("sub");
        std::fs::create_dir(&sub).expect("create sub");
        std::fs::write(sub.join("inner.bin"), vec![0u8; 100]).expect("write inner.bin");

        let baseline = dir_size_bytes(root);
        assert_eq!(baseline, 1000, "the real files total 1000 bytes");

        symlink(&real_file, root.join("link-to-file")).expect("symlink to file");
        symlink(&sub, root.join("link-to-dir")).expect("symlink to dir");
        // A self-referential link: descending into links would recurse forever.
        symlink(root, sub.join("link-to-root")).expect("symlink to root");

        assert_eq!(
            dir_size_bytes(root),
            baseline,
            "symlinks must contribute nothing — the target is already counted \
             through its real path, and links are never descended into"
        );
    }

    /// `CoreIndex::disk_size_bytes` must measure the core's real data dir:
    /// non-zero once a commit has written segments, and equal to an
    /// independent walk of that same directory. This is the test that goes red
    /// if `disk_size_bytes` is mutated to return a constant.
    #[test]
    fn disk_size_bytes_measures_the_core_data_dir() {
        let (dir, index) = open_test_index();
        let data_dir = dir.path().join("data");

        index
            .add_documents(
                &[json!({"id": "doc1", "body": "the quick brown fox jumps over the lazy dog"})],
                true,
            )
            .expect("add_documents");
        index.commit().expect("commit");

        let expected = walk_size_oracle(&data_dir);
        assert!(
            expected > 0,
            "a committed core must have written something to {}",
            data_dir.display()
        );
        assert_eq!(
            index.disk_size_bytes(),
            expected,
            "disk_size_bytes must report the real on-disk total for the core's data dir"
        );
        assert!(
            index.disk_size_bytes() > 0,
            "a committed core must not report a zero on-disk size"
        );

        // A file added under the data dir (including one nested a level down,
        // as Tantivy's own layout can be) is picked up on the next call — the
        // figure is measured per call, not captured once at open time.
        let nested = data_dir.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(nested.join("padding.bin"), vec![0u8; 50_000]).expect("write padding.bin");
        assert_eq!(
            index.disk_size_bytes(),
            expected + 50_000,
            "disk_size_bytes must re-walk the tree, including subdirectories"
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

    /// Indexes a single doc with the given `body`, commits, and returns the
    /// `DocAddress` of the hit for a `body:<term>` query -- the shared setup
    /// for the `highlight_field` multi-snippet tests below (issue #103).
    fn indexed_hit_for_term(body: &str, term: &str) -> (TempDir, CoreIndex, DocAddress) {
        let (dir, index) = open_test_index();
        index
            .add_documents(&[json!({"id": "doc1", "body": body})], true)
            .expect("add_documents");
        index.commit().expect("commit");
        let query = index.parse_query(term, "body").expect("parse_query");
        let hits = index
            .search(query.as_ref(), &[], &[])
            .expect("search should not fail");
        let (_score, addr) = hits
            .into_iter()
            .next()
            .expect("the indexed doc must match the term");
        (dir, index, addr)
    }

    /// Same body text (with the term swapped to "widget") as
    /// `HL_SNIPPETS_PROBE_DOCS` in `src/coverage.rs`, whose gaps between the
    /// three term occurrences are already pinned by
    /// `hl_snippets_probe_doc_gaps_exceed_a_snippet_window` to exceed
    /// Tantivy's 150-char default snippet window. Reusing that exact spacing
    /// here means a highlighter that produces one fragment per occurrence
    /// (rather than merging occurrences into a shared window) must return
    /// three genuinely separate, non-overlapping fragments for this body.
    const THREE_WELL_SEPARATED_OCCURRENCES: &str = "widget prototype unveiled at the trade show. \
        the weather in the valley stayed mild and overcast for most of the week without much \
        wind at all. a second widget shipment arrived at the warehouse yesterday. meanwhile the \
        local council debated a new bridge proposal for nearly three hours last tuesday evening. \
        engineers are already testing a third widget revision in the lab. several farmers \
        reported an unusually early harvest this year thanks to the warm and sunny spring \
        season.";

    /// `highlight_field`'s `snippets_cap` argument, set out of the way, for
    /// the tests below that are about *extraction* rather than capping: they
    /// want every fragment the primitive can find, and do their own
    /// `take(cap)` afterwards. `MAX_SNIPPETS_PER_FIELD` still bounds the loop
    /// internally, so this is "as many as exist", not an unbounded scan.
    const UNCAPPED: usize = usize::MAX;

    /// `CoreIndex::highlight_field` itself (issue #103): against a field with
    /// three well-separated occurrences of the query term, it must be able to
    /// surface more than Tantivy's single best-scoring fragment. Composes the
    /// primitive's result with `.take(cap)`, exactly the way
    /// `crate::highlight::highlighting` applies `hl.snippets` today, so this
    /// pins the *extraction* behavior `highlight_field` owns rather than
    /// re-testing the capping `take()` already does correctly.
    ///
    /// M < N: cap 2 over 3 real occurrences must yield exactly 2 snippets,
    /// not the single-fragment ceiling this asserted before #103.
    #[test]
    fn highlight_field_extracts_up_to_cap_when_cap_is_below_match_count() {
        let (_dir, index, addr) = indexed_hit_for_term(THREE_WELL_SEPARATED_OCCURRENCES, "widget");
        let query = index.parse_query("widget", "body").expect("parse_query");

        let all_snippets = index
            .highlight_field(query.as_ref(), addr, "body", 150, "<em>", "</em>", UNCAPPED)
            .expect("highlight_field");

        let cap = 2;
        let capped: Vec<&String> = all_snippets.iter().take(cap).collect();
        assert_eq!(
            capped.len(),
            cap,
            "issue #103: three well-separated occurrences of the query term must let \
             highlight_field surface enough distinct fragments to fill a cap of {cap}; got \
             {all_snippets:?}"
        );
        for snippet in &capped {
            assert!(
                snippet.contains("<em>widget</em>"),
                "each returned snippet must wrap the matched term in the requested markers, got {snippet:?}"
            );
        }
        assert_eq!(
            capped.iter().collect::<HashSet<_>>().len(),
            cap,
            "the {cap} snippets filling the cap must be distinct fragments, not the same \
             fragment repeated: {capped:?}"
        );
    }

    /// M >= N: a cap of 5 over exactly 3 real occurrences must yield exactly
    /// 3 snippets -- Solr's own `hl.snippets` never pads past what actually
    /// exists in the field (finding 53, `src/highlight.rs` module docs), and
    /// `highlight_field` must supply all 3 real fragments for the cap to have
    /// anything to reflect.
    #[test]
    fn highlight_field_returns_every_match_when_cap_exceeds_match_count() {
        let (_dir, index, addr) = indexed_hit_for_term(THREE_WELL_SEPARATED_OCCURRENCES, "widget");
        let query = index.parse_query("widget", "body").expect("parse_query");

        let all_snippets = index
            .highlight_field(query.as_ref(), addr, "body", 150, "<em>", "</em>", UNCAPPED)
            .expect("highlight_field");

        let cap = 5;
        let capped: Vec<&String> = all_snippets.iter().take(cap).collect();
        assert_eq!(
            capped.len(),
            3,
            "issue #103: three real occurrences of the query term, capped at {cap}, must \
             yield exactly 3 snippets (min(cap, actual matches)), never fewer and never padded; \
             got {all_snippets:?}"
        );
        for snippet in &capped {
            assert!(
                snippet.contains("<em>widget</em>"),
                "each returned snippet must wrap the matched term in the requested markers, got {snippet:?}"
            );
        }
        assert_eq!(
            capped.iter().collect::<HashSet<_>>().len(),
            3,
            "all 3 snippets must be distinct fragments, not the same fragment repeated: {capped:?}"
        );
    }

    /// Two occurrences of the query term close enough together (well inside
    /// a single ~150-char snippet window) that a naive "extract best
    /// fragment, then mask only the matched term's own byte range, then
    /// re-extract" loop could plausibly re-select overlapping text around
    /// the *other* occurrence, or emit the same fragment twice, or panic on
    /// a byte range that no longer lines up with the (partially masked)
    /// source text.
    ///
    /// This test does not assert a specific fragment count or boundary --
    /// nothing in the spec pins exactly how adjacent occurrences must be
    /// split -- but it does assert the safe/conservative contract the
    /// implementor must satisfy either way: no panic, no two returned
    /// fragments that are byte-for-byte identical, and no fragment so short
    /// a human would not call it a real snippet (an arbitrary but generous
    /// floor -- comfortably below what a masking bug that chopped a fragment
    /// down to a few stray characters would produce, comfortably above an
    /// empty or near-empty string).
    #[test]
    fn highlight_field_handles_adjacent_occurrences_without_duplicates_or_panics() {
        let body = "the widget alpha and the widget beta sit right next to each other in the \
            same storage crate on the loading dock this morning.";
        let (_dir, index, addr) = indexed_hit_for_term(body, "widget");
        let query = index.parse_query("widget", "body").expect("parse_query");

        // The production entry point never panics on this input -- this is
        // the actual assertion for the "no panic" half of the contract;
        // `expect` below turns any `Err` (never a panic) into a clear
        // failure message rather than an opaque one.
        let snippets = index
            .highlight_field(query.as_ref(), addr, "body", 150, "<em>", "</em>", UNCAPPED)
            .expect("highlight_field must not error on adjacent occurrences of the same term");

        assert!(
            !snippets.is_empty(),
            "a field that matched the query must return at least one snippet"
        );
        assert_eq!(
            snippets.iter().collect::<HashSet<_>>().len(),
            snippets.len(),
            "no two returned snippets may be byte-for-byte identical: {snippets:?}"
        );
        const MIN_REAL_FRAGMENT_LEN: usize = 20;
        for snippet in &snippets {
            assert!(
                snippet.len() >= MIN_REAL_FRAGMENT_LEN,
                "a masking bug that truncated a fragment down to a stray few characters must \
                 fail here: {snippet:?} is shorter than {MIN_REAL_FRAGMENT_LEN} bytes"
            );
            assert!(
                snippet.contains("<em>widget</em>"),
                "each returned snippet must still wrap a real match in the requested markers, \
                 got {snippet:?}"
            );
        }
    }

    /// `snippets_cap` bounds *extraction*, not just the returned slice.
    /// Every snippet past the first costs another full `SnippetGenerator`
    /// pass over the field text, so an `hl.snippets=2` request must not pay
    /// to find the third occurrence it will never show. Returning 2 (not 3)
    /// for a body with three findable occurrences is the observable proxy for
    /// "the loop stopped": a third snippet could only exist if a third pass
    /// had run.
    #[test]
    fn highlight_field_stops_extracting_once_the_cap_is_filled() {
        let (_dir, index, addr) = indexed_hit_for_term(THREE_WELL_SEPARATED_OCCURRENCES, "widget");
        let query = index.parse_query("widget", "body").expect("parse_query");

        let uncapped = index
            .highlight_field(query.as_ref(), addr, "body", 150, "<em>", "</em>", UNCAPPED)
            .expect("highlight_field");
        assert_eq!(
            uncapped.len(),
            3,
            "precondition: this body has three findable occurrences, so a cap below 3 is a \
             cap that must actually bite; got {uncapped:?}"
        );

        for cap in [1, 2] {
            let capped = index
                .highlight_field(query.as_ref(), addr, "body", 150, "<em>", "</em>", cap)
                .expect("highlight_field");
            assert_eq!(
                capped.len(),
                cap,
                "hl.snippets={cap} must stop extraction at {cap} snippets rather than \
                 extracting all 3 and letting the caller truncate; got {capped:?}"
            );
            assert_eq!(
                capped[..],
                uncapped[..cap],
                "capping early must yield the same leading snippets as extracting everything"
            );
        }
    }
}
