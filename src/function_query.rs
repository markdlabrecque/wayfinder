//! Solr function queries (issue #289): the arithmetic evaluator
//! `search_api_solr`'s `{!boost b=...}` document-boost path needs.
//!
//! ## What this is, and what it is not
//!
//! Per finding 129 (`docs/solr-ref-findings.md`), the module emits the score
//! inline in `q` as `{!boost b=sum(boost_document,...)}` or
//! `{!boost b=boost_document}` (`SearchApiSolrBackend.php:1953-1977`) — never
//! as `bf=`. That makes a function-query *evaluator* the real dependency,
//! reached through the `{!func}`/`{!boost}` query-parser local params. The
//! function set is open-ended by construction (processor-supplied templates),
//! so this module is a parser + per-document evaluator over an extensible AST,
//! not a fixed list.
//!
//! The concrete first targets are constants, numeric field references, and the
//! arithmetic functions the client path and the captured fixtures exercise:
//! `sum`, `product`, `max`, `min`, `recip`. **`payload_score` is out of
//! scope here** — `Utility::flattenKeysToPayloadScore` (verified against the
//! live `4.4.x` source, outside the three-file snapshot) emits
//! `{!payload_score f=boost_term v=... func=max}`, a *separate query parser*
//! over a payload-bearing `boost_term_payload` field type. `ms`/`rord` are off
//! the corrected client path too (finding 129 corrected the
//! `product(...,recip(ms(...)))`-as-`bf` premise) and need date/ordinal field
//! types. Both are follow-up increments; the AST below is the foundation
//! #292's `geodist()` will extend.
//!
//! ## Scoring model
//!
//! A function query is evaluated per document, returning an `f64` that Solr
//! narrows to `f32` for the final score (we match that cast at the scorer
//! boundary). Field references read a numeric fast-field column; a document
//! with no value resolves to `0.0`, Solr's function-query default for a
//! missing numeric value (confirmed by the `fnq_func_missing` fixture, where
//! `d5` has no `views` and `sum(views,rating)` scores it `0+rating`). A
//! referenced field that is not in the schema is a 400 `undefined field`,
//! validated up front before any query is built (`fnq_err_unknown_field`) —
//! the parser itself is schema-agnostic and only rejects *syntax* and
//! *unknown functions* (`fnq_err_unknown_func`, `fnq_err_unbalanced`,
//! `fnq_err_empty`).

use std::fmt;
use std::sync::Arc;

use tantivy::columnar::Column;
use tantivy::fastfield::AliveBitSet;
use tantivy::query::{EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::{COLLECT_BLOCK_BUFFER_LEN, DocId, DocSet, Score, SegmentReader, TERMINATED, Term};

/// One parsed function query. Plain data so it is cheap to share behind an
/// `Arc` across per-segment scorers and to extend with new variants without
/// touching every call site.
#[derive(Debug, Clone, PartialEq)]
pub enum FuncQuery {
    /// A numeric literal, e.g. `2` or `1.5`.
    Constant(f64),
    /// A bare field reference, e.g. `boost_document`. Resolves to the
    /// document's value for that numeric field, or `0.0` if absent.
    Field(String),
    /// `sum(a, b, ...)`. Variadic; the empty sum is `0.0` (Solr's `sum()`).
    Sum(Vec<FuncQuery>),
    /// `product(a, b, ...)`. Variadic; the empty product is `1.0` (Solr's
    /// `product()`).
    Product(Vec<FuncQuery>),
    /// `max(a, b, ...)`. Variadic.
    Max(Vec<FuncQuery>),
    /// `min(a, b, ...)`. Variadic.
    Min(Vec<FuncQuery>),
    /// `recip(x, m, a, b) = a / (m*x + b)`, the BoostMoreRecent classic in its
    /// pure-arithmetic form (no `rord`/`ms`).
    Recip(
        Box<FuncQuery>,
        Box<FuncQuery>,
        Box<FuncQuery>,
        Box<FuncQuery>,
    ),
    /// Argless `geodist()`: the haversine distance (km) from the request-param
    /// point `pt` to each document's `sfield` point. `sfield` is a `location`
    /// field whose two synthetic columns `sfield__lat`/`sfield__lon` hold the
    /// doc's point (#331); `pt` is the `(lat, lon)` origin the caller parsed
    /// from the `pt` request param. Carrying `pt` in the variant (rather than
    /// threading request params through `eval`) keeps `eval`'s closure-over-
    /// field-values signature unchanged for every other variant, and the
    /// argless request-param-driven form is the client-evidenced one (finding
    /// 133); the explicit-args `geodist(sfield, pt, ...)` form is a documented
    /// descope.
    GeoDist { sfield: String, pt: (f64, f64) },
}

impl FuncQuery {
    /// Field names referenced anywhere in the tree, in first-occurrence order,
    /// deduplicated. Drives both schema validation (before building the query)
    /// and per-segment column opening (inside the scorer).
    pub fn fields(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_fields(&mut out);
        out
    }

    fn collect_fields(&self, out: &mut Vec<String>) {
        match self {
            FuncQuery::Constant(_) => {}
            FuncQuery::Field(name) => {
                if !out.iter().any(|n| n == name) {
                    out.push(name.clone());
                }
            }
            FuncQuery::Sum(args)
            | FuncQuery::Product(args)
            | FuncQuery::Max(args)
            | FuncQuery::Min(args) => {
                for arg in args {
                    arg.collect_fields(out);
                }
            }
            FuncQuery::Recip(x, m, a, b) => {
                x.collect_fields(out);
                m.collect_fields(out);
                a.collect_fields(out);
                b.collect_fields(out);
            }
            FuncQuery::GeoDist { sfield, .. } => {
                // The two synthetic columns backing the `location` field.
                let lat = format!("{sfield}__lat");
                let lon = format!("{sfield}__lon");
                if !out.contains(&lat) {
                    out.push(lat);
                }
                if !out.contains(&lon) {
                    out.push(lon);
                }
            }
        }
    }

    /// Evaluates the function for one document. `field_value` returns the
    /// document's numeric value for a field name (the caller resolves it from
    /// the segment's fast-field columns, missing → `0.0`).
    fn eval(&self, field_value: &impl Fn(&str) -> f64) -> f64 {
        match self {
            FuncQuery::Constant(c) => *c,
            FuncQuery::Field(name) => field_value(name),
            FuncQuery::Sum(args) => args.iter().map(|a| a.eval(field_value)).sum(),
            FuncQuery::Product(args) => args.iter().map(|a| a.eval(field_value)).product(),
            FuncQuery::Max(args) => args
                .iter()
                .map(|a| a.eval(field_value))
                .fold(f64::NEG_INFINITY, f64::max),
            FuncQuery::Min(args) => args
                .iter()
                .map(|a| a.eval(field_value))
                .fold(f64::INFINITY, f64::min),
            FuncQuery::Recip(x, m, a, b) => {
                let xv = x.eval(field_value);
                let mv = m.eval(field_value);
                let av = a.eval(field_value);
                let bv = b.eval(field_value);
                av / (mv * xv + bv)
            }
            FuncQuery::GeoDist { sfield, pt } => {
                let lat = field_value(&format!("{sfield}__lat"));
                let lon = field_value(&format!("{sfield}__lon"));
                haversine_km(lat, lon, pt.0, pt.1)
            }
        }
    }

    /// Whether the function *exists* for a document — Solr's `exists()` over
    /// a `ValueSource`. A constant always exists; a field reference exists
    /// iff the document has a value for it; a compound function exists iff
    /// every argument exists (`MultiFunction`'s all-exist rule). This is the
    /// load-bearing difference between `{!func}` and `{!frange}` (#333):
    /// `{!func}` *scores* every document (a missing field resolves to `0.0`
    /// via `eval`), but `{!frange}` *filters*, and `ValueSourceRangeFilter`
    /// excludes any document whose function does not exist — so a doc with a
    /// missing field is dropped from `{!frange l=0 u=100}field` even though
    /// its evaluated `0.0` would fall in range (`frange_missing_excluded`).
    /// `field_exists` reports whether the document has a value for a field
    /// name (the caller resolves it from the segment's fast-field columns).
    fn exists(&self, field_exists: &impl Fn(&str) -> bool) -> bool {
        match self {
            FuncQuery::Constant(_) => true,
            FuncQuery::Field(name) => field_exists(name),
            FuncQuery::Sum(args)
            | FuncQuery::Product(args)
            | FuncQuery::Max(args)
            | FuncQuery::Min(args) => args.iter().all(|a| a.exists(field_exists)),
            FuncQuery::Recip(x, m, a, b) => {
                x.exists(field_exists)
                    && m.exists(field_exists)
                    && a.exists(field_exists)
                    && b.exists(field_exists)
            }
            // A `geodist()` exists iff the document has both synthetic
            // columns backing its `location` field. This is what makes
            // `{!frange}geodist()` (#332) exclude docs with no point even
            // though `eval` would resolve the missing columns to 0.0 -- the
            // same `{!func}`-scores vs `{!frange}`-filters distinction every
            // other variant honours (#333).
            FuncQuery::GeoDist { sfield, .. } => {
                field_exists(&format!("{sfield}__lat")) && field_exists(&format!("{sfield}__lon"))
            }
        }
    }
}

/// Great-circle distance in kilometres, the quantity Solr's `geodist()` returns
/// for a `LatLonPointSpatialField`. Standard haversine over the earth radius
/// Solr/Lucene use (`DistanceUtils.EARTH_EQUATORIAL_RADIUS_KM` /
/// `SloppyMath`'s `TO_METERS`, both `6371008.7714` m -- a mean radius despite
/// the legacy "equatorial" name).
///
/// Solr computes this through Lucene's `SloppyMath.haversinMeters`, a
/// speed-optimised approximation that is itself only accurate to ~40 cm, on
/// lat/lon re-read from Lucene's lossy 32-bit BKD quantisation. Wayfinder
/// computes the exact haversine on the full-precision f64 columns instead; the
/// two agree to well under a centimetre here, the same "same logical value,
/// different floating-point path" category the differential harness already
/// tolerates for BM25 `score` magnitudes and stats `sum`/`mean` (#331).
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R_KM: f64 = 6_371.008_771_4;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let lat1r = lat1.to_radians();
    let lat2r = lat2.to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1r.cos() * lat2r.cos() * (dlon / 2.0).sin().powi(2);
    R_KM * 2.0 * h.sqrt().asin()
}

/// A parse error carrying a Solr-shaped `SyntaxError` message. The message is
/// normalised away by the differential harness (`error.msg`), so only its
/// presence — a 400 — is wire-compared; it nonetheless stays Solr-shaped for
/// an operator reading raw JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncQueryError(pub String);

impl fmt::Display for FuncQueryError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FuncQueryError {}

/// Validates that every field referenced by `func` is resolvable. `exists`
/// reports whether the schema knows the field name. Returns an error naming
/// the first unknown field (Solr-shaped `undefined field`), which is the
/// `fnq_err_unknown_field` 400 path. The schema is the caller's concern, so
/// this takes a predicate rather than importing the schema type.
pub fn validate_fields(
    func: &FuncQuery,
    exists: impl Fn(&str) -> bool,
) -> Result<(), FuncQueryError> {
    for name in func.fields() {
        if !exists(&name) {
            return Err(FuncQueryError(format!("undefined field \"{name}\"")));
        }
    }
    Ok(())
}

/// Parses a function-query expression into a [`FuncQuery`].
///
/// The grammar is the function-call subset the client emits:
/// `name(arg, arg, ...)` for functions, a bare identifier for a field
/// reference, and a numeric literal for a constant. An identifier followed by
/// `(` is a call; without `(` it is a field. Unknown function names, unbalanced
/// parentheses, trailing tokens, and an empty body are all `SyntaxError`s,
/// matching real Solr's 400s (`fnq_err_*` fixtures).
pub fn parse(src: &str) -> Result<FuncQuery, FuncQueryError> {
    let mut p = Parser {
        bytes: src.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let expr = p.expr()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(FuncQueryError(format!(
            "unexpected trailing input in function query `{src}`"
        )));
    }
    Ok(expr)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expr(&mut self) -> Result<FuncQuery, FuncQueryError> {
        self.skip_ws();
        match self.peek() {
            None => Err(FuncQueryError(
                "Expected identifier in function query".to_string(),
            )),
            Some(b) if b.is_ascii_digit() || b == b'.' => self.number(),
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => self.ident_or_call(),
            Some(_) => Err(FuncQueryError(format!(
                "unexpected character `{}` in function query",
                self.bytes[self.pos] as char
            ))),
        }
    }

    /// A numeric literal: a run of digits, `.`, and an optional `e`/`E`
    /// exponent. Solr also accepts a leading sign, but the client only emits
    /// positive constants (`recip(rating,1,1,1)`, `product(rating,2)`), so a
    /// sign is left to a later infix-operator grammar rather than ambiguously
    /// consumed here.
    fn number(&mut self) -> Result<FuncQuery, FuncQueryError> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'.' || b == b'e' || b == b'E' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
        let v: f64 = s.parse().map_err(|_| {
            FuncQueryError(format!("invalid numeric literal `{s}` in function query"))
        })?;
        Ok(FuncQuery::Constant(v))
    }

    fn ident_or_call(&mut self) -> Result<FuncQuery, FuncQueryError> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let name = std::str::from_utf8(&self.bytes[start..self.pos])
            .unwrap_or("")
            .to_string();
        self.skip_ws();
        if self.peek() == Some(b'(') {
            // Function call.
            self.pos += 1; // consume '('
            let mut args = Vec::new();
            self.skip_ws();
            if self.peek() != Some(b')') {
                loop {
                    args.push(self.expr()?);
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => {
                            self.pos += 1;
                            self.skip_ws();
                        }
                        Some(b')') => break,
                        Some(_) => {
                            return Err(FuncQueryError(format!(
                                "expected `,` or `)` in function call `{name}`"
                            )));
                        }
                        None => {
                            return Err(FuncQueryError(format!(
                                "unexpected end of input in function call `{name}`"
                            )));
                        }
                    }
                }
            }
            // The ')' is guaranteed by the loop above; consume it.
            debug_assert_eq!(self.peek(), Some(b')'));
            self.pos += 1;
            build_call(&name, args)
        } else {
            Ok(FuncQuery::Field(name))
        }
    }
}

fn build_call(name: &str, args: Vec<FuncQuery>) -> Result<FuncQuery, FuncQueryError> {
    match name {
        "sum" => Ok(FuncQuery::Sum(args)),
        "product" => Ok(FuncQuery::Product(args)),
        "max" => Ok(FuncQuery::Max(args)),
        "min" => Ok(FuncQuery::Min(args)),
        "recip" => {
            if args.len() != 4 {
                return Err(FuncQueryError(format!(
                    "recip() requires exactly 4 arguments, got {}",
                    args.len()
                )));
            }
            let mut iter = args.into_iter();
            Ok(FuncQuery::Recip(
                Box::new(iter.next().expect("checked len")),
                Box::new(iter.next().expect("checked len")),
                Box::new(iter.next().expect("checked len")),
                Box::new(iter.next().expect("checked len")),
            ))
        }
        other => Err(FuncQueryError(format!("unknown function `{other}`"))),
    }
}

// --- per-document scoring ---------------------------------------------------

/// How a function value combines with the wrapped query's base score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreOp {
    /// `score *= value`. Used by `{!boost b=...}` and edismax's `boost`
    /// (function form).
    Multiply,
    /// `score += value`. Used by edismax's additive `bf`.
    Add,
}

/// A Tantivy [`Query`] that wraps a child query and modifies each document's
/// score by a per-document function value. The matched document set is exactly
/// the child's — the function only changes scores, never membership — so
/// `{!boost b=f}*:*` matches everything and scores each doc `1.0 * f`, and a
/// bare `{!func}f` is this query wrapping [`tantivy::query::AllQuery`] (which
/// scores every doc `1.0`), yielding `f` per doc. See [`Self::all`].
///
/// This mirrors Tantivy's own `BoostQuery` (a constant multiplier): the child
/// drives the `DocSet`, and `score()` applies the function on top. There is no
/// built-in per-document boost, so the `Weight`/`Scorer` pair is bespoke.
pub struct FunctionScoreQuery {
    child: Box<dyn Query>,
    func: Arc<FuncQuery>,
    op: ScoreOp,
}

impl FunctionScoreQuery {
    /// `{!func}` shape: matches every document (via `AllQuery`, score `1.0`)
    /// and scores each by the function value. Equivalent to
    /// `multiply(AllQuery, func)` because `1.0 * f == f`.
    pub fn all(func: FuncQuery) -> FunctionScoreQuery {
        FunctionScoreQuery {
            child: Box::new(tantivy::query::AllQuery),
            func: Arc::new(func),
            op: ScoreOp::Multiply,
        }
    }

    /// `{!boost b=...}` / edismax `boost` shape: multiply the child's score by
    /// the function value.
    pub fn multiply(child: Box<dyn Query>, func: FuncQuery) -> FunctionScoreQuery {
        FunctionScoreQuery {
            child,
            func: Arc::new(func),
            op: ScoreOp::Multiply,
        }
    }

    /// edismax `bf` shape: add the function value to the child's score.
    pub fn add(child: Box<dyn Query>, func: FuncQuery) -> FunctionScoreQuery {
        FunctionScoreQuery {
            child,
            func: Arc::new(func),
            op: ScoreOp::Add,
        }
    }
}

impl Clone for FunctionScoreQuery {
    fn clone(&self) -> Self {
        FunctionScoreQuery {
            child: self.child.box_clone(),
            func: Arc::clone(&self.func),
            op: self.op,
        }
    }
}

impl fmt::Debug for FunctionScoreQuery {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FunctionScoreQuery")
            .field("child", &self.child)
            .field("func", &self.func)
            .field("op", &self.op)
            .finish()
    }
}

impl Query for FunctionScoreQuery {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let child = self.child.weight(enable_scoring)?;
        Ok(Box::new(FunctionScoreWeight {
            child,
            func: Arc::clone(&self.func),
            op: self.op,
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.child.query_terms(visitor);
    }
}

struct FunctionScoreWeight {
    child: Box<dyn Weight>,
    func: Arc<FuncQuery>,
    op: ScoreOp,
}

impl Weight for FunctionScoreWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let child = self.child.scorer(reader, boost)?;
        let columns = FieldColumns::open(reader, &self.func.fields())?;
        Ok(Box::new(FunctionScoreScorer {
            child,
            columns,
            func: Arc::clone(&self.func),
            op: self.op,
        }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let child_explain = self.child.explain(reader, doc)?;
        let columns = FieldColumns::open(reader, &self.func.fields())?;
        let base = child_explain.value();
        let value = self.func.eval(&|name| columns.value(name, doc));
        let (score, label) = match self.op {
            ScoreOp::Multiply => (base * value as Score, "function multiply"),
            ScoreOp::Add => (base + value as Score, "function add"),
        };
        let mut explanation = Explanation::new_with_string(label.to_string(), score);
        explanation.add_detail(child_explain);
        Ok(explanation)
    }

    fn count(&self, reader: &SegmentReader) -> tantivy::Result<u32> {
        // Membership is the child's; the function adds no documents.
        self.child.count(reader)
    }
}

/// Evaluates `func` for a single document, opening that segment's fast-field
/// columns and resolving each field reference (missing numeric → `0.0`, Solr's
/// function-query default). The per-`DocAddress` entry point the `fl=`
/// computed-field path needs for `dist:geodist()` (#331): unlike the
/// [`FunctionScoreQuery`] scorer, it evaluates one doc at a time rather than
/// driving a `DocSet`, which is fine at site scale (small page of `rows` docs).
pub fn eval_doc(reader: &SegmentReader, doc: DocId, func: &FuncQuery) -> tantivy::Result<f64> {
    let columns = FieldColumns::open(reader, &func.fields())?;
    Ok(func.eval(&|name| columns.value(name, doc)))
}

/// The resolved fast-field columns a function references, for one segment.
/// Looked up by name (the field set is tiny — typically one to three names —
/// so a linear scan per field reference is negligible next to the function
/// evaluation itself).
struct FieldColumns {
    cols: Vec<(String, NumColumn)>,
}

enum NumColumn {
    I64(Column<i64>),
    F64(Column<f64>),
    /// The field exists in the schema but no numeric column is stored for
    /// this segment (e.g. a non-fast or non-numeric field). Every value is
    /// `0.0`, matching Solr's missing-numeric-value default.
    Missing,
}

impl FieldColumns {
    fn open(reader: &SegmentReader, fields: &[String]) -> tantivy::Result<FieldColumns> {
        let fast = reader.fast_fields();
        let mut cols = Vec::with_capacity(fields.len());
        for name in fields {
            // Probed in the same order as `SegmentSortColumn::open`: the
            // columnar reader is the authority on what is stored, and a field
            // materialises as exactly one of these.
            if let Some(col) = fast.column_opt::<i64>(name)? {
                cols.push((name.clone(), NumColumn::I64(col)));
            } else if let Some(col) = fast.column_opt::<f64>(name)? {
                cols.push((name.clone(), NumColumn::F64(col)));
            } else {
                cols.push((name.clone(), NumColumn::Missing));
            }
        }
        Ok(FieldColumns { cols })
    }

    fn value(&self, name: &str, doc: DocId) -> f64 {
        for (n, col) in &self.cols {
            if n == name {
                return match col {
                    NumColumn::I64(c) => c
                        .values_for_doc(doc)
                        .next()
                        .map(|v| v as f64)
                        .unwrap_or(0.0),
                    NumColumn::F64(c) => c.values_for_doc(doc).next().unwrap_or(0.0),
                    NumColumn::Missing => 0.0,
                };
            }
        }
        0.0
    }

    /// Whether the document has a value for the field — the per-document
    /// `exists()` backing [`FuncQuery::exists`] for `{!frange}` (#333). A
    /// `Missing` column (field declared but no numeric fast column for this
    /// segment) has no value for any document, so every doc is `false` there;
    /// `{!frange}` therefore matches nothing on such a field, while
    /// `{!func}` still scores every doc `0.0`.
    fn exists(&self, name: &str, doc: DocId) -> bool {
        for (n, col) in &self.cols {
            if n == name {
                return match col {
                    NumColumn::I64(c) => c.values_for_doc(doc).next().is_some(),
                    NumColumn::F64(c) => c.values_for_doc(doc).next().is_some(),
                    NumColumn::Missing => false,
                };
            }
        }
        false
    }
}

struct FunctionScoreScorer {
    child: Box<dyn Scorer>,
    columns: FieldColumns,
    func: Arc<FuncQuery>,
    op: ScoreOp,
}

impl DocSet for FunctionScoreScorer {
    fn advance(&mut self) -> DocId {
        self.child.advance()
    }
    fn doc(&self) -> DocId {
        self.child.doc()
    }
    fn size_hint(&self) -> u32 {
        self.child.size_hint()
    }
    // Forward the perf-sensitive DocSet methods to the child rather than
    // falling back to the default `advance()`-loop impls, exactly as
    // Tantivy's `BoostScorer` does.
    fn seek(&mut self, target: DocId) -> DocId {
        self.child.seek(target)
    }
    // `seek_danger` is deliberately not overridden: its return type
    // (`SeekDangerResult`) is not re-exported by tantivy 0.26, so it cannot be
    // named outside the crate. The default `DocSet::seek_danger` impl drives
    // `seek`/`doc`, both of which are forwarded above, so it stays correct --
    // only the fast-path optimisation is forgone, which is irrelevant to a
    // per-document function boost.
    fn fill_buffer(&mut self, buffer: &mut [DocId; COLLECT_BLOCK_BUFFER_LEN]) -> usize {
        self.child.fill_buffer(buffer)
    }
    fn cost(&self) -> u64 {
        self.child.cost()
    }
    fn count(&mut self, alive_bitset: &AliveBitSet) -> u32 {
        self.child.count(alive_bitset)
    }
    fn count_including_deleted(&mut self) -> u32 {
        self.child.count_including_deleted()
    }
}

impl Scorer for FunctionScoreScorer {
    fn score(&mut self) -> Score {
        let doc = self.child.doc();
        let base = self.child.score();
        let columns = &self.columns;
        let value = self.func.eval(&|name| columns.value(name, doc)) as Score;
        match self.op {
            ScoreOp::Multiply => base * value,
            ScoreOp::Add => base + value,
        }
    }
}

// --- per-document range filtering (#333) -------------------------------------

/// Whether a function value that may or may not exist falls in a
/// `{!frange}` range. Pure so the boundary logic is unit-testable and
/// mutation-testable without a Tantivy index. `exists` is the
/// [`FuncQuery::exists`] verdict for the document; when it is false the doc
/// never matches, regardless of the evaluated value — the `exists()` filter
/// that distinguishes `{!frange}` from `{!func}` (`frange_missing_excluded`).
fn frange_matches(
    value: f64,
    exists: bool,
    lower: Option<f64>,
    upper: Option<f64>,
    include_lower: bool,
    include_upper: bool,
) -> bool {
    if !exists {
        return false;
    }
    if let Some(l) = lower
        && (value < l || (!include_lower && value == l))
    {
        return false;
    }
    if let Some(u) = upper
        && (value > u || (!include_upper && value == u))
    {
        return false;
    }
    true
}

/// A Tantivy [`Query`] matching exactly the documents whose function value
/// exists and falls in a `{!frange l=.. u=..}` range — Solr's
/// `FunctionRangeQuery` over `ValueSourceRangeFilter`. Constant-score `1.0`,
/// matching Solr's `ConstantScoreScorer` for a parsed frange (`frange_on_q`
/// captures `score=1.0`). The matched set is independent of any child query:
/// the scorer enumerates every alive document (via [`tantivy::query::AllQuery`]
/// as the driver, exactly as [`FunctionScoreQuery::all`] does) and keeps those
/// `frange_matches` accepts.
///
/// This is the filter dual of [`FunctionScoreQuery`]: that query *scores* the
/// child's doc set by the function (missing → `0.0`), this one *selects* a doc
/// set by the function range (missing → excluded). `{!frange}` is deliberately
/// general — `geodist()` is just one function the client routes through it —
/// so it composes with the `GeoDist` variant #332 will add with no change here.
pub struct FunctionRangeQuery {
    func: Arc<FuncQuery>,
    lower: Option<f64>,
    upper: Option<f64>,
    include_lower: bool,
    include_upper: bool,
}

impl FunctionRangeQuery {
    /// `{!frange l=.. u=.. [incl=..] [incu=..]}<func>`: a range filter over
    /// `func`. `lower`/`upper` of `None` mean an open bound (absent `l`/`u`);
    /// `include_lower`/`include_upper` default to `true`, Solr's `incl`/`incu`
    /// defaults.
    pub fn new(
        func: FuncQuery,
        lower: Option<f64>,
        upper: Option<f64>,
        include_lower: bool,
        include_upper: bool,
    ) -> FunctionRangeQuery {
        FunctionRangeQuery {
            func: Arc::new(func),
            lower,
            upper,
            include_lower,
            include_upper,
        }
    }
}

impl Clone for FunctionRangeQuery {
    fn clone(&self) -> Self {
        FunctionRangeQuery {
            func: Arc::clone(&self.func),
            lower: self.lower,
            upper: self.upper,
            include_lower: self.include_lower,
            include_upper: self.include_upper,
        }
    }
}

impl fmt::Debug for FunctionRangeQuery {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FunctionRangeQuery")
            .field("func", &self.func)
            .field("lower", &self.lower)
            .field("upper", &self.upper)
            .field("include_lower", &self.include_lower)
            .field("include_upper", &self.include_upper)
            .finish()
    }
}

impl Query for FunctionRangeQuery {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        // `AllQuery` enumerates every alive document; the range predicate
        // narrows it. `enable_scoring` is ignored: frange is constant-score.
        let child = tantivy::query::AllQuery.weight(enable_scoring)?;
        Ok(Box::new(FunctionRangeWeight {
            child,
            func: Arc::clone(&self.func),
            lower: self.lower,
            upper: self.upper,
            include_lower: self.include_lower,
            include_upper: self.include_upper,
        }))
    }

    fn query_terms<'a>(&'a self, _visitor: &mut dyn FnMut(&'a Term, bool)) {
        // A range over fast-field columns contributes no term dictionary
        // clauses; membership is decided by scanning columns, like AllQuery.
    }
}

struct FunctionRangeWeight {
    child: Box<dyn Weight>,
    func: Arc<FuncQuery>,
    lower: Option<f64>,
    upper: Option<f64>,
    include_lower: bool,
    include_upper: bool,
}

impl Weight for FunctionRangeWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let child = self.child.scorer(reader, boost)?;
        let columns = FieldColumns::open(reader, &self.func.fields())?;
        let mut scorer = FunctionRangeScorer {
            child,
            columns,
            func: Arc::clone(&self.func),
            lower: self.lower,
            upper: self.upper,
            include_lower: self.include_lower,
            include_upper: self.include_upper,
        };
        // Tantivy's `DocSet` iteration is `doc()`-first (`fill_buffer`, `seek`,
        // the default `for_each` all read `doc()` before `advance()`), so a
        // fresh scorer must be positioned AT its first match, not before it.
        // `AllQuery`'s scorer is pre-advanced to the first alive doc; walk
        // forward from there to the first doc the range accepts (or TERMINATED).
        scorer.position_at_first_match();
        Ok(Box::new(scorer))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let columns = FieldColumns::open(reader, &self.func.fields())?;
        let value = self.func.eval(&|name| columns.value(name, doc));
        let exists = self.func.exists(&|name| columns.exists(name, doc));
        let matches = frange_matches(
            value,
            exists,
            self.lower,
            self.upper,
            self.include_lower,
            self.include_upper,
        );
        let mut explanation =
            Explanation::new_with_string("frange".to_string(), if matches { 1.0 } else { 0.0 });
        explanation.add_detail(Explanation::new_with_string(
            format!("value={value} exists={exists}"),
            value as Score,
        ));
        Ok(explanation)
    }

    fn count(&self, reader: &SegmentReader) -> tantivy::Result<u32> {
        // The matched set is a subset of AllQuery's, so the child's count is
        // only an upper bound; fall back to scanning the scorer for an exact
        // count (filters are not on a hot path that needs the cheaper bound).
        let mut scorer = self.scorer(reader, 1.0)?;
        let mut n = 0u32;
        while scorer.advance() != TERMINATED {
            n += 1;
        }
        Ok(n)
    }
}

struct FunctionRangeScorer {
    child: Box<dyn Scorer>,
    columns: FieldColumns,
    func: Arc<FuncQuery>,
    lower: Option<f64>,
    upper: Option<f64>,
    include_lower: bool,
    include_upper: bool,
}

impl FunctionRangeScorer {
    fn matches_at(&self, doc: DocId) -> bool {
        let value = self.func.eval(&|name| self.columns.value(name, doc));
        let exists = self.func.exists(&|name| self.columns.exists(name, doc));
        frange_matches(
            value,
            exists,
            self.lower,
            self.upper,
            self.include_lower,
            self.include_upper,
        )
    }

    /// Advance the child forward from its current position until it sits on a
    /// document the range accepts (or is exhausted). Used once at construction
    /// so the `doc()`-first `DocSet` contract holds from the first read.
    fn position_at_first_match(&mut self) {
        while self.child.doc() != TERMINATED && !self.matches_at(self.child.doc()) {
            self.child.advance();
        }
    }
}

impl DocSet for FunctionRangeScorer {
    fn advance(&mut self) -> DocId {
        loop {
            let doc = self.child.advance();
            if doc == TERMINATED {
                return TERMINATED;
            }
            if self.matches_at(doc) {
                return doc;
            }
        }
    }
    fn doc(&self) -> DocId {
        // The child is always positioned on a match (or TERMINATED): by
        // `position_at_first_match` at construction, and by the filtering
        // `advance` thereafter. So forwarding is correct, matching
        // `FunctionScoreScorer`'s relationship to its child.
        self.child.doc()
    }
    fn size_hint(&self) -> u32 {
        self.child.size_hint()
    }
}

impl Scorer for FunctionRangeScorer {
    fn score(&mut self) -> Score {
        // Constant-score 1.0, matching Solr's ConstantScoreScorer for a
        // parsed frange (the boost a `Query` defaults to).
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_constants_fields_and_calls() {
        assert_eq!(parse("2"), Ok(FuncQuery::Constant(2.0)));
        assert_eq!(
            parse("boost_document"),
            Ok(FuncQuery::Field("boost_document".into()))
        );
        assert_eq!(
            parse("sum(boost_document,rating)"),
            Ok(FuncQuery::Sum(vec![
                FuncQuery::Field("boost_document".into()),
                FuncQuery::Field("rating".into()),
            ]))
        );
        assert_eq!(
            parse("sum(1,2,3)"),
            Ok(FuncQuery::Sum(vec![
                FuncQuery::Constant(1.0),
                FuncQuery::Constant(2.0),
                FuncQuery::Constant(3.0),
            ]))
        );
        assert!(matches!(
            parse("product(rating,2)"),
            Ok(FuncQuery::Product(_))
        ));
        assert!(matches!(
            parse("recip(rating,1,1,1)"),
            Ok(FuncQuery::Recip(_, _, _, _))
        ));
    }

    #[test]
    fn rejects_unknown_function_and_bad_syntax() {
        // Mirrors the `fnq_err_*` fixtures: unknown function, unbalanced
        // parens, empty body, trailing junk.
        assert!(parse("bogus(1,2)").is_err());
        assert!(parse("sum(boost_document").is_err());
        assert!(parse("").is_err());
        assert!(parse("sum(1) garbage").is_err());
        assert!(parse("recip(rating,1)").is_err(), "recip needs 4 args");
    }

    #[test]
    fn evaluates_against_field_values() {
        // sum(boost_document=1, rating=2) == 3
        let f = parse("sum(boost_document,rating)").unwrap();
        assert_eq!(
            f.eval(&|name| match name {
                "boost_document" => 1.0,
                "rating" => 2.0,
                _ => 0.0,
            }),
            3.0
        );
        // Missing field -> 0 (Solr default).
        let f = parse("sum(views,rating)").unwrap();
        assert_eq!(
            f.eval(&|name| match name {
                "views" => 0.0,
                "rating" => 5.0,
                _ => 0.0,
            }),
            5.0
        );
        // recip(rating=6,1,1,1) = 1/(6+1) = 1/7.
        let f = parse("recip(rating,1,1,1)").unwrap();
        assert!(
            (f.eval(&|name| if name == "rating" { 6.0 } else { 0.0 }) - 1.0 / 7.0).abs() < 1e-12
        );
        // product() identity == 1, sum() identity == 0.
        assert_eq!(parse("product()").unwrap().eval(&|_| 0.0), 1.0);
        assert_eq!(parse("sum()").unwrap().eval(&|_| 0.0), 0.0);
    }

    #[test]
    fn fields_are_unique_and_in_order() {
        let f = parse("sum(boost_document,rating,boost_document)").unwrap();
        assert_eq!(
            f.fields(),
            vec!["boost_document".to_string(), "rating".to_string()]
        );
    }

    #[test]
    fn exists_is_field_gated_and_compound_is_all_exist() {
        // The `{!func}` vs `{!frange}` distinction (#333): a constant always
        // exists; a field exists iff the doc has a value; a compound function
        // exists iff every argument does (Solr MultiFunction all-exist).
        assert!(parse("2").unwrap().exists(&|_| false));
        assert!(parse("rating").unwrap().exists(&|n| n == "rating"));
        assert!(!parse("rating").unwrap().exists(&|_| false));
        // sum(rating,price): exists only when BOTH fields exist.
        let f = parse("sum(rating,price)").unwrap();
        assert!(f.exists(&|n| n == "rating" || n == "price"));
        assert!(!f.exists(&|n| n == "rating"));
        assert!(!f.exists(&|n| n == "price"));
        // A missing field in a compound drops exists even though eval would
        // yield a value in range (the `frange_compound_missing` case: d4 has
        // no `price`, so sum(price,1) does not exist for it).
        assert!(!f.exists(&|n| n == "rating"));
        // recip's args are all-or-nothing too.
        let r = parse("recip(rating,1,1,1)").unwrap();
        assert!(r.exists(&|n| n == "rating"));
        assert!(!r.exists(&|_| false));
    }

    #[test]
    fn frange_range_predicate_handles_bounds_inclusivity_and_exists() {
        // `[2,6]` inclusive both.
        let (l, u, il, iu) = (Some(2.0), Some(6.0), true, true);
        assert!(frange_matches(2.0, true, l, u, il, iu));
        assert!(frange_matches(6.0, true, l, u, il, iu));
        assert!(frange_matches(4.0, true, l, u, il, iu));
        assert!(!frange_matches(1.9, true, l, u, il, iu));
        assert!(!frange_matches(6.1, true, l, u, il, iu));
        // incl=false -> lower-exclusive: 2.0 drops, 6.0 stays.
        let (l, u, il, iu) = (Some(2.0), Some(6.0), false, true);
        assert!(!frange_matches(2.0, true, l, u, il, iu));
        assert!(frange_matches(6.0, true, l, u, il, iu));
        // incu=false -> upper-exclusive: 6.0 drops, 2.0 stays.
        let (l, u, il, iu) = (Some(2.0), Some(6.0), true, false);
        assert!(frange_matches(2.0, true, l, u, il, iu));
        assert!(!frange_matches(6.0, true, l, u, il, iu));
        // Open bounds: lower-only, upper-only, neither.
        assert!(frange_matches(100.0, true, Some(4.0), None, true, true));
        assert!(!frange_matches(3.0, true, Some(4.0), None, true, true));
        assert!(frange_matches(-5.0, true, None, Some(2.0), true, true));
        assert!(frange_matches(0.0, true, None, None, true, true));
        // exists=false never matches, even when the value would be in range
        // (the load-bearing `{!frange}` rule, frange_missing_excluded).
        assert!(!frange_matches(
            0.0,
            false,
            Some(0.0),
            Some(100.0),
            true,
            true
        ));
    }

    #[test]
    fn geodist_reports_the_two_synthetic_columns_and_haversines() {
        // `geodist()` is constructed with the request params resolved (sfield,
        // pt) rather than parsed, since the argless form is request-param-
        // driven and `parse` has no request context.
        let f = FuncQuery::GeoDist {
            sfield: "loc".to_string(),
            pt: (40.0, -74.0),
        };
        assert_eq!(
            f.fields(),
            vec!["loc__lat".to_string(), "loc__lon".to_string()]
        );

        // The origin doc itself is 0 km away.
        let origin = |name: &str| match name {
            "loc__lat" => 40.0,
            "loc__lon" => -74.0,
            _ => 0.0,
        };
        assert_eq!(f.eval(&origin), 0.0);

        // One degree of latitude is ~111.2 km. This is the exact-haversine
        // value; Solr's SloppyMath agrees to <1 cm (see `haversine_km`).
        let one_deg_n = |name: &str| match name {
            "loc__lat" => 41.0,
            "loc__lon" => -74.0,
            _ => 0.0,
        };
        let km = f.eval(&one_deg_n);
        assert!(
            (km - 111.195).abs() < 1e-2,
            "1 degree of latitude ≈ 111.2 km, got {km}"
        );

        // Symmetry: +1 lat and -1 lat are the same distance from the origin.
        let one_deg_s = |name: &str| match name {
            "loc__lat" => 39.0,
            "loc__lon" => -74.0,
            _ => 0.0,
        };
        assert_eq!(f.eval(&one_deg_n), f.eval(&one_deg_s));
    }
}
