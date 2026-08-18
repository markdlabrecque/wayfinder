//! Normaliser + differ for Solr-fixture comparisons.
//! (issue #1, PRD §8).
//!
//! Every normalisation is *recorded*: `normalize` returns the normalised
//! value plus every JSON path it touched (dropped or otherwise altered), and
//! `diff` records the paths where a tolerance rule (currently: `score`) was
//! applied, whether or not it let the values through. A normaliser that
//! silently greens the suite is the failure mode the PRD names — nothing
//! here is allowed to touch a path without saying so.
//!
//! Path format: dotted for object keys, `[i]` for array indices — e.g.
//! `responseHeader.QTime`, `response.docs[0].score`, `error.msg`.

use serde_json::Value;
use std::fmt;

/// A value plus the list of JSON paths the normaliser touched (dropped or
/// otherwise altered) to produce it.
#[derive(Debug, Clone)]
pub struct Normalized {
    pub value: Value,
    pub touched: Vec<String>,
}

/// Normalises a Solr/Wayfinder response envelope per PRD §8 and
/// `solr-ref/FINDINGS.md`:
/// - drops `responseHeader.QTime` (always variable).
/// - drops `_version_` / `_root_` from every doc in `response.docs`
///   (Wayfinder's explicit default-`fl` decision, finding 9 — reuses/extends
///   `normalize_envelope` in `tests/common/mod.rs`, does not duplicate it).
/// - on an error envelope, drops `error.msg`, `error.metadata`, and
///   `error.trace` (finding 10, extended by finding 59's captured 500 —
///   `error.trace` is a Java stack trace, free text no other engine can
///   reproduce) — `error.code` and the HTTP status are the only parts
///   compared.
///
/// Timestamps are in scope per the PRD but no current fixture has a
/// date-ish field, so there is deliberately no format-handling hook here yet
/// (spec: "keep the hook minimal, do not invent format handling that no
/// fixture exercises").
pub fn normalize(mut value: Value) -> Normalized {
    let mut touched = Vec::new();

    if let Some(header) = value
        .get_mut("responseHeader")
        .and_then(|h| h.as_object_mut())
        && header.remove("QTime").is_some()
    {
        touched.push("responseHeader.QTime".to_string());
    }

    if let Some(docs) = value
        .pointer_mut("/response/docs")
        .and_then(|d| d.as_array_mut())
    {
        for (i, doc) in docs.iter_mut().enumerate() {
            if let Some(obj) = doc.as_object_mut() {
                if obj.remove("_version_").is_some() {
                    touched.push(format!("response.docs[{i}]._version_"));
                }
                if obj.remove("_root_").is_some() {
                    touched.push(format!("response.docs[{i}]._root_"));
                }
            }
        }
    }

    if let Some(error) = value.get_mut("error").and_then(|e| e.as_object_mut()) {
        if error.remove("msg").is_some() {
            touched.push("error.msg".to_string());
        }
        if error.remove("metadata").is_some() {
            touched.push("error.metadata".to_string());
        }
        // `err_regex_bad_class.json`'s one 500 carries a Java stack trace —
        // free text no other engine can reproduce, same rationale as
        // `error.msg` above (finding 10, extended by finding 59).
        if error.remove("trace").is_some() {
            touched.push("error.trace".to_string());
        }
    }

    Normalized { value, touched }
}

/// Score-comparison tolerance for BM25 floats (`response.docs[].score`).
/// Exposed as a function rather than a bare constant so the chosen value is
/// a runtime-visible, documented decision the implementor makes (PRD §8:
/// "float tolerance on scores... log every field the normaliser touched").
///
/// `1e-3` is chosen because it is far tighter than any real relevance
/// difference (adjacent-ranked BM25 scores in this corpus differ by ~1e-1 or
/// more) while still absorbing float round-off from two independent BM25
/// implementations (Tantivy vs Solr/Lucene) computing the same score along
/// slightly different floating-point paths (e.g. summation order, `f32` vs
/// `f64` intermediate precision).
pub fn score_tolerance() -> f64 {
    1e-3
}

/// A single structural difference between an expected and actual JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub path: String,
    pub expected: String,
    pub actual: String,
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} vs {}", self.path, self.expected, self.actual)
    }
}

/// The result of diffing two already-normalised envelopes: every real
/// difference, plus every path where a tolerance rule was *applied* — so a
/// report can say what was allowed to differ even when `diffs` is empty.
#[derive(Debug, Clone, Default)]
pub struct DiffReport {
    pub diffs: Vec<Diff>,
    pub touched: Vec<String>,
}

/// Object keys under `stats.stats_fields.<field>` (issue #5) whose values are
/// diff-noise floats — Tantivy vs Solr/Lucene summation-order differences,
/// same root cause as `score` — and therefore get `score_tolerance()` instead
/// of exact equality. Deliberately excludes `min`/`max`/`count`/`missing`:
/// those are exact integers/values in every captured fixture (`stats_views`,
/// `stats_multi_fields`, `stats_zero`, `stats_zero_fq`) and a real regression
/// in any of them must still fail. Reused, not duplicated, by `diff_at` below
/// — matching is scoped to the `stats_fields` subtree specifically (checked
/// via the parent `path`, not just the key name) so an unrelated object
/// elsewhere in the envelope that happens to have a key named `sum` or `mean`
/// is not accidentally tolerated too.
const STATS_METRIC_TOLERANCE_KEYS: &[&str] = &["sum", "sumOfSquares", "mean", "stddev"];

/// Structural diff of two already-normalised JSON values (human-readable
/// `path: expected vs actual`, not a bare `assert_eq!`). Recurses through
/// objects and arrays; any object key literally named `score` is compared
/// with `score_tolerance()` instead of exact equality, and its path is
/// recorded in `touched` regardless of whether it matched. The same
/// tolerance applies to `sum`/`sumOfSquares`/`mean`/`stddev` keys, but only
/// under `stats.stats_fields.<field>` (see `STATS_METRIC_TOLERANCE_KEYS`).
/// Every other mismatch — an added/removed key, a changed value, a reordered
/// array element — is a `Diff`. No key is ever silently ignored (spec: "no
/// blanket ignore-unknown-keys escape hatch").
pub fn diff(expected: &Value, actual: &Value) -> DiffReport {
    let mut report = DiffReport::default();
    diff_at("", expected, actual, &mut report);
    report
}

/// Recursive worker for `diff`. `path` is the dotted/`[i]` path to `expected`
/// and `actual` themselves (empty string at the root).
fn diff_at(path: &str, expected: &Value, actual: &Value, report: &mut DiffReport) {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            let mut keys: Vec<&String> = e.keys().chain(a.keys()).collect();
            keys.sort();
            keys.dedup();
            let in_stats_fields = path.starts_with("stats.stats_fields.");
            for key in keys {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let tolerate = key == "score"
                    || (in_stats_fields && STATS_METRIC_TOLERANCE_KEYS.contains(&key.as_str()));
                match (e.get(key), a.get(key)) {
                    (Some(ev), Some(av)) if tolerate => {
                        report.touched.push(child_path.clone());
                        let matches = match (ev.as_f64(), av.as_f64()) {
                            (Some(ef), Some(af)) => (ef - af).abs() <= score_tolerance(),
                            _ => ev == av,
                        };
                        if !matches {
                            report.diffs.push(Diff {
                                path: child_path,
                                expected: ev.to_string(),
                                actual: av.to_string(),
                            });
                        }
                    }
                    (Some(ev), Some(av)) => diff_at(&child_path, ev, av, report),
                    (Some(ev), None) => report.diffs.push(Diff {
                        path: child_path,
                        expected: ev.to_string(),
                        actual: "<missing>".to_string(),
                    }),
                    (None, Some(av)) => report.diffs.push(Diff {
                        path: child_path,
                        expected: "<missing>".to_string(),
                        actual: av.to_string(),
                    }),
                    (None, None) => unreachable!("key came from one of the two maps"),
                }
            }
        }
        (Value::Array(e), Value::Array(a)) => {
            let n = e.len().max(a.len());
            for i in 0..n {
                let child_path = format!("{path}[{i}]");
                match (e.get(i), a.get(i)) {
                    (Some(ev), Some(av)) => diff_at(&child_path, ev, av, report),
                    (Some(ev), None) => report.diffs.push(Diff {
                        path: child_path,
                        expected: ev.to_string(),
                        actual: "<missing>".to_string(),
                    }),
                    (None, Some(av)) => report.diffs.push(Diff {
                        path: child_path,
                        expected: "<missing>".to_string(),
                        actual: av.to_string(),
                    }),
                    (None, None) => unreachable!("index within [0, max(len))"),
                }
            }
        }
        _ if expected != actual => report.diffs.push(Diff {
            path: path.to_string(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        }),
        _ => {}
    }
}

/// One ranked-relevance doc: an `id` plus its `score`, when the response
/// carried one (issue #31 follow-up: no fixture's `fl` used to include
/// `score`, so this path was exercised only by synthetic tests. Now that
/// `select_term_scored`/`select_quick_scored` do, `diff_ranked_ids` compares
/// scores for real, not just id order).
#[derive(Debug, Clone, PartialEq)]
pub struct RankedDoc {
    pub id: String,
    pub score: Option<f64>,
}

/// Extracts `response.docs[]` as an ordered `Vec<RankedDoc>` (`id` plus
/// `score` when present), for use with `diff_ranked_ids`. Callers must feed
/// `diff_ranked_ids` this real doc data — not a pre-stripped id list — so the
/// score-tolerance path actually runs against real fixtures.
pub fn ranked_docs(envelope: &Value) -> Vec<RankedDoc> {
    envelope
        .pointer("/response/docs")
        .and_then(Value::as_array)
        .map(|docs| {
            docs.iter()
                .map(|doc| RankedDoc {
                    id: doc
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| panic!("doc missing string `id`: {doc}"))
                        .to_string(),
                    score: doc.get("score").and_then(Value::as_f64),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Ranked-ID-list comparison for free-text relevance queries (PRD §8:
/// "compare ranked ID lists, not just result sets"). Order matters: an
/// identical multiset of ids in a different order is a `Diff`, not a pass.
///
/// When either side's doc at a position carries a `score`, the scores are
/// compared under `score_tolerance()` instead of exact equality, and the
/// comparison is always recorded in `DiffReport::touched` — whether or not it
/// passed — the same way the generic differ in `diff_at` logs every
/// tolerance application. A doc with a score on one side and none on the
/// other is a diff, not silently skipped: present-vs-missing is real
/// information the harness must not throw away. Order comparison (the `id`
/// list) stays primary and independent of the score check.
pub fn diff_ranked_ids(expected: &[RankedDoc], actual: &[RankedDoc]) -> DiffReport {
    let mut report = DiffReport::default();

    let expected_ids: Vec<&str> = expected.iter().map(|d| d.id.as_str()).collect();
    let actual_ids: Vec<&str> = actual.iter().map(|d| d.id.as_str()).collect();
    if expected_ids != actual_ids {
        report.diffs.push(Diff {
            path: "response.docs[].id".to_string(),
            expected: format!("{expected_ids:?}"),
            actual: format!("{actual_ids:?}"),
        });
    }

    let n = expected.len().max(actual.len());
    for i in 0..n {
        let e_score = expected.get(i).and_then(|d| d.score);
        let a_score = actual.get(i).and_then(|d| d.score);
        if e_score.is_none() && a_score.is_none() {
            continue;
        }
        let path = format!("response.docs[{i}].score");
        report.touched.push(path.clone());
        match (e_score, a_score) {
            (Some(ef), Some(af)) => {
                if (ef - af).abs() > score_tolerance() {
                    report.diffs.push(Diff {
                        path,
                        expected: ef.to_string(),
                        actual: af.to_string(),
                    });
                }
            }
            (Some(ef), None) => report.diffs.push(Diff {
                path,
                expected: ef.to_string(),
                actual: "<missing>".to_string(),
            }),
            (None, Some(af)) => report.diffs.push(Diff {
                path,
                expected: "<missing>".to_string(),
                actual: af.to_string(),
            }),
            (None, None) => unreachable!("checked above"),
        }
    }

    report
}

/// Strips every `<meta name="X-Parsed-By" content="..." />` element (plus a
/// following newline) from an XHTML `file` value, reporting whether anything
/// was removed. A free function (not inlined into `normalize_extract`) so it
/// can be unit-tested against strings that do and do not contain the marker
/// without going through a whole envelope `Value`.
fn strip_x_parsed_by_meta(file: &str) -> (String, bool) {
    const MARKER: &str = "<meta name=\"X-Parsed-By\"";
    let mut result = String::new();
    let mut touched = false;
    let mut rest = file;
    loop {
        match rest.find(MARKER) {
            None => {
                result.push_str(rest);
                break;
            }
            Some(idx) => {
                result.push_str(&rest[..idx]);
                touched = true;
                let after_marker = &rest[idx..];
                let mut consumed = after_marker
                    .find("/>")
                    .map(|i| i + 2)
                    .unwrap_or(after_marker.len());
                // Also eat one trailing newline, so removing an element does
                // not leave a blank line behind it — every captured `file`
                // value puts exactly one `\n` after each `<meta .../>`.
                if after_marker[consumed..].starts_with('\n') {
                    consumed += 1;
                }
                rest = &after_marker[consumed..];
            }
        }
    }
    (result, touched)
}

/// Strips every ` shape="rect"` attribute (Tika's own addition to every
/// HTML-parsed `<a>` element) from a `file` value, reporting whether
/// anything was removed.
fn strip_shape_rect(file: &str) -> (String, bool) {
    const MARKER: &str = " shape=\"rect\"";
    if file.contains(MARKER) {
        (file.replace(MARKER, ""), true)
    } else {
        (file.to_string(), false)
    }
}

/// Removes the `X-Parsed-By` key (and its value array) from a `file_metadata`
/// alternating-array, reporting whether it was present.
/// Removes the `X-Parsed-By` entry from a `file_metadata` value rendered in
/// any of Solr's `json.nl` shapes (flat / map / arrarr / arrmap), returning
/// the rewritten value and whether the entry was present. `X-Parsed-By` names
/// Java class names Wayfinder has no honest equivalent for (PRD divergence
/// 10); it is stripped from both sides before the differential compare so the
/// remaining keys compare exactly regardless of the rendered shape — issue
/// #274 made the extract handler honour `json.nl`, so `file_metadata` is no
/// longer guaranteed to be the flat array the prior normaliser assumed.
fn strip_x_parsed_by_metadata(value: Value) -> (Value, bool) {
    match value {
        // `json.nl=map`: `{"key": [values], ...}`.
        Value::Object(mut map) => {
            let removed = map.remove("X-Parsed-By").is_some();
            (Value::Object(map), removed)
        }
        Value::Array(arr) => {
            // Distinguish the three array shapes by the first element:
            //   flat   -> ["key", [values], ...]   (first element is a String)
            //   arrarr -> [["key", [values]], ...] (first element is an Array)
            //   arrmap -> [{"key": [values]}, ...] (first element is an Object)
            match arr.first() {
                Some(Value::String(_)) => strip_x_parsed_by_metadata_flat(arr),
                Some(Value::Array(_)) => strip_x_parsed_by_metadata_arrarr(arr),
                Some(Value::Object(_)) => strip_x_parsed_by_metadata_arrmap(arr),
                // An empty array, or a leading element of an unexpected type,
                // carries no X-Parsed-By entry to strip; return it untouched
                // so a genuine shape difference still surfaces in the diff.
                _ => (Value::Array(arr), false),
            }
        }
        // Not a recognised file_metadata shape (e.g. an error body without
        // one); leave it untouched.
        other => (other, false),
    }
}

/// `flat`: `["key", [values], ...]` — drop the `X-Parsed-By` key/value pair.
fn strip_x_parsed_by_metadata_flat(arr: Vec<Value>) -> (Value, bool) {
    let mut out = Vec::with_capacity(arr.len());
    let mut removed = false;
    let mut i = 0;
    while i + 1 < arr.len() {
        let key = arr[i].as_str().unwrap_or_default();
        if key == "X-Parsed-By" {
            removed = true;
        } else {
            out.push(arr[i].clone());
            out.push(arr[i + 1].clone());
        }
        i += 2;
    }
    // An odd trailing element (malformed input) is preserved rather than
    // silently dropped, so a bug elsewhere producing a lopsided array is
    // still visible in the diff instead of being swallowed here.
    if i < arr.len() {
        out.push(arr[i].clone());
    }
    (Value::Array(out), removed)
}

/// `arrarr`: `[["key", [values]], ...]` — drop any pair whose key is
/// `X-Parsed-By`.
fn strip_x_parsed_by_metadata_arrarr(arr: Vec<Value>) -> (Value, bool) {
    let mut removed = false;
    let out: Vec<Value> = arr
        .into_iter()
        .filter(|pair| {
            let is_xpb = pair
                .as_array()
                .and_then(|p| p.first())
                .and_then(|k| k.as_str())
                == Some("X-Parsed-By");
            if is_xpb {
                removed = true;
            }
            !is_xpb
        })
        .collect();
    (Value::Array(out), removed)
}

/// `arrmap`: `[{"key": [values]}, ...]` — drop any one-entry object keyed
/// `X-Parsed-By`. Only a genuine one-entry `{X-Parsed-By: [...]}` element is
/// dropped; a multi-key element is a shape violation best left for the diff
/// to surface rather than partially rewritten here.
fn strip_x_parsed_by_metadata_arrmap(arr: Vec<Value>) -> (Value, bool) {
    let mut removed = false;
    let out: Vec<Value> = arr
        .into_iter()
        .filter(|obj| {
            let is_xpb = obj
                .as_object()
                .map(|m| m.len() == 1 && m.contains_key("X-Parsed-By"))
                .unwrap_or(false);
            if is_xpb {
                removed = true;
            }
            !is_xpb
        })
        .collect();
    (Value::Array(out), removed)
}

/// The ratified `/update/extract` divergences. Two scopes, each handling a
/// distinct, documented class of Tika-specific content Wayfinder deliberately
/// does not reproduce — see the PRD's ratified-divergence entry and
/// `solr-ref/FINDINGS.md` findings 120-127:
///
/// **Plain formats (issue #258) — text/HTML/etc.**: `X-Parsed-By` names Java
/// Tika/PDFBox/etc. class names Wayfinder has no honest equivalent for, in
/// both the XHTML `file` value's `<meta>` elements and the `file_metadata`
/// array; `shape="rect"` is an attribute Tika's own HTML parser injects onto
/// every `<a>` element. Only those two markers are stripped.
///
/// **Office formats (issue #260) — DOCX/PPTX/XLSX/ODT/ODP/ODS/RTF**: Tika
/// emits a rich set of format-specific metadata (document properties, page
/// counts, parser provenance, etc.) — dozens of `<meta>` elements and
/// `file_metadata` keys per capture — that Wayfinder does not reproduce. The
/// office scope keeps only the six envelope keys both sides agree on
/// (`resourceName`, `Content-Type`, `stream_name`, `stream_source_info`,
/// `stream_size`, `stream_content_type`), strips every `<meta>` from the
/// XHTML `<head>` (leaving `<title>` and `<body>` to compare), and collapses
/// the leading-newline run of text-format bodies (finding 124: Tika's count
/// is a function of its meta count, which Wayfinder cannot reproduce with
/// narrow metadata).
///
/// Nothing else in the extract envelope is touched: per CLAUDE.md's
/// compatibility contract, a real difference must still be reported by
/// `diff()`, and `normalize_extract` deliberately does not widen to hide one
/// (the extraction feature tests prove exactly that, including for office
/// rows).
pub fn normalize_extract(mut value: Value) -> Normalized {
    let mut touched = Vec::new();

    if is_office_content_type(&value) {
        if let Some(file) = value.get("file").and_then(|f| f.as_str()) {
            if file.contains("<head>") {
                let (stripped, did) = strip_all_head_metas(file);
                if did {
                    touched.push(
                        "file (<head> <meta> elements — office formats emit Tika \
                         format-specific metadata Wayfinder does not reproduce)"
                            .to_string(),
                    );
                    value["file"] = Value::String(stripped);
                }
            } else {
                // extractFormat=text: a plain-text body with no markup. The
                // leading-newline run is collapsed (finding 124).
                let stripped = file.trim_start_matches('\n');
                if stripped.len() != file.len() {
                    touched.push(
                        "file (leading newlines collapsed — count is a function of \
                         Tika meta count, finding 124)"
                            .to_string(),
                    );
                    value["file"] = Value::String(stripped.to_string());
                }
            }
        }
        if let Some(arr) = value.get("file_metadata").and_then(|m| m.as_array()) {
            let (stripped, dropped) = keep_envelope_metadata_keys(arr);
            if dropped > 0 {
                touched.push(format!(
                    "file_metadata (kept the six envelope keys, dropped {dropped} \
                     Tika format-specific entries)"
                ));
                value["file_metadata"] = Value::Array(stripped);
            }
        }
    } else if is_pdf_content_type(&value) {
        // PDF (issue #294). `pdf-extract`'s coordinate-based text device and
        // Tika/PDFBox emit the same words in the same order but different
        // whitespace (single vs double newline between columns, none vs
        // `\n\n\n\n` between pages). The #261 GO report ratified this as
        // "match (whitespace divergence only) ... a normalisation detail for
        // the renderer, not an extraction defect", so the text body is
        // compared by its non-whitespace token sequence. Tika's rich PDF
        // metadata (`pdf:*`, `access_permission:*`, `xmpTPg:NPages`, ...) is
        // dropped to the six envelope keys exactly as the office formats are.
        if let Some(file) = value.get("file").and_then(|f| f.as_str()) {
            // extractFormat=text bodies carry no `<head>` markup; only those
            // are PDF text rows. Whitespace-run collapse trims and reduces
            // every run to a single space, applied symmetrically to both
            // sides, so it cannot hide a real content difference (a dropped
            // word, a mojibake glyph, a reordered column still differ).
            if !file.contains("<head>") {
                let collapsed = file.split_whitespace().collect::<Vec<_>>().join(" ");
                if collapsed != file {
                    touched.push(
                        "file (PDF body whitespace collapsed — pdf-extract's coordinate \
                         spacing differs from Tika/PDFBox's; content compared by token, \
                         per the #261 GO report)"
                            .to_string(),
                    );
                    value["file"] = Value::String(collapsed);
                }
            }
        }
        if let Some(arr) = value.get("file_metadata").and_then(|m| m.as_array()) {
            let (stripped, dropped) = keep_envelope_metadata_keys(arr);
            if dropped > 0 {
                touched.push(format!(
                    "file_metadata (kept the six envelope keys, dropped {dropped} \
                     Tika PDF-specific entries)"
                ));
                value["file_metadata"] = Value::Array(stripped);
            }
        }
    } else {
        if let Some(file) = value.get("file").and_then(|f| f.as_str()) {
            let (stripped, meta_touched) = strip_x_parsed_by_meta(file);
            let (stripped, shape_touched) = strip_shape_rect(&stripped);
            if meta_touched {
                touched.push("file (X-Parsed-By meta elements)".to_string());
            }
            if shape_touched {
                touched.push("file (shape=\"rect\" attributes)".to_string());
            }
            if meta_touched || shape_touched {
                value["file"] = Value::String(stripped);
            }
        }

        if let Some(fm) = value.get("file_metadata").cloned() {
            let (stripped, removed) = strip_x_parsed_by_metadata(fm);
            if removed {
                touched.push("file_metadata (X-Parsed-By)".to_string());
                value["file_metadata"] = stripped;
            }
        }
    }

    Normalized { value, touched }
}

/// The seven office MIME types issue #260 adds extractors for. Used by
/// `normalize_extract` to pick the office-scoped waiver. Checked against the
/// `Content-Type` / `stream_content_type` envelope values (declared and
/// resolved) in `file_metadata`.
const OFFICE_CONTENT_TYPES: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/rtf",
];

/// True when the envelope carries an office content type (issue #260),
/// detected from the `file_metadata` `Content-Type` / `stream_content_type`
/// values. Returns `false` for plain text/HTML/etc. envelopes and for any
/// envelope without `file_metadata` (e.g. an error body).
fn is_office_content_type(value: &Value) -> bool {
    let Some(arr) = value.get("file_metadata").and_then(|m| m.as_array()) else {
        return false;
    };
    let mut i = 0;
    while i + 1 < arr.len() {
        let key = arr[i].as_str().unwrap_or_default();
        if matches!(key, "Content-Type" | "stream_content_type")
            && let Some(vals) = arr[i + 1].as_array()
        {
            for v in vals {
                if let Some(s) = v.as_str()
                    && OFFICE_CONTENT_TYPES.contains(&s)
                {
                    return true;
                }
            }
        }
        i += 2;
    }
    false
}
/// True when the envelope carries a PDF content type (issue #294), detected
/// the same way as [`is_office_content_type`]. Used by `normalize_extract` to
/// pick the PDF-scoped waiver: whitespace-run collapse on the text body (the
/// #261 report's ratified divergence) plus the six-envelope-key metadata
/// filter. Returns `false` for every non-PDF envelope and for any envelope
/// without `file_metadata` (e.g. an error body — `extract_corrupt_pdf`'s 500
/// carries none, and is compared as a plain error envelope, not normalised).
fn is_pdf_content_type(value: &Value) -> bool {
    let Some(arr) = value.get("file_metadata").and_then(|m| m.as_array()) else {
        return false;
    };
    let mut i = 0;
    while i + 1 < arr.len() {
        let key = arr[i].as_str().unwrap_or_default();
        if matches!(key, "Content-Type" | "stream_content_type")
            && let Some(vals) = arr[i + 1].as_array()
        {
            for v in vals {
                if v.as_str() == Some("application/pdf") {
                    return true;
                }
            }
        }
        i += 2;
    }
    false
}

/// Strips every `<meta ... />` element (plus its trailing newline) from an
/// XHTML `file` value's `<head>`, leaving `<title>` and `<body>` untouched.
/// Office captures carry dozens of format-specific `<meta>` elements;
/// Wayfinder emits none beyond the envelope, so the whole head metadata set
/// is ratified-divergence rather than any one named key.
fn strip_all_head_metas(file: &str) -> (String, bool) {
    // Only touch the `<head>...</head>` region so a `<meta>` that legitimately
    // appears inside a body run (never in these fixtures, but defensively) is
    // left alone.
    let Some(head_start) = file.find("<head>") else {
        return (file.to_string(), false);
    };
    let Some(head_end_rel) = file[head_start..].find("</head>") else {
        return (file.to_string(), false);
    };
    let head_end = head_start + head_end_rel;
    let head = &file[head_start..head_end];
    if !head.contains("<meta ") {
        return (file.to_string(), false);
    }
    let mut stripped_head = String::new();
    let mut rest = head;
    loop {
        match rest.find("<meta ") {
            None => {
                stripped_head.push_str(rest);
                break;
            }
            Some(idx) => {
                stripped_head.push_str(&rest[..idx]);
                let after = &rest[idx..];
                let mut consumed = after.find("/>").map(|i| i + 2).unwrap_or(after.len());
                // Eat one trailing newline so removal leaves no blank line.
                if after[consumed..].starts_with('\n') {
                    consumed += 1;
                }
                rest = &after[consumed..];
            }
        }
    }
    let mut out = String::with_capacity(file.len());
    out.push_str(&file[..head_start]);
    out.push_str(&stripped_head);
    out.push_str(&file[head_end..]);
    (out, true)
}

/// The six `file_metadata` keys that are part of the wire envelope both Solr
/// and Wayfinder always emit identically (in this order): everything else in
/// an office capture is Tika format-specific metadata.
const ENVELOPE_METADATA_KEYS: &[&str] = &[
    "resourceName",
    "Content-Type",
    "stream_name",
    "stream_source_info",
    "stream_size",
    "stream_content_type",
];

/// Keeps only the six envelope key/value pairs from a `file_metadata`
/// alternating array, preserving their captured order and dropping every
/// Tika format-specific entry. Returns the filtered array and a count of how
/// many entries were dropped.
fn keep_envelope_metadata_keys(arr: &[Value]) -> (Vec<Value>, usize) {
    let mut out = Vec::with_capacity(arr.len());
    let mut dropped = 0;
    let mut i = 0;
    while i + 1 < arr.len() {
        let key = arr[i].as_str().unwrap_or_default();
        if ENVELOPE_METADATA_KEYS.contains(&key) {
            out.push(arr[i].clone());
            out.push(arr[i + 1].clone());
        } else {
            dropped += 1;
        }
        i += 2;
    }
    // Preserve a malformed odd trailing element rather than silently dropping
    // it, mirroring `strip_x_parsed_by_metadata_flat`.
    if i < arr.len() {
        out.push(arr[i].clone());
    }
    (out, dropped)
}
