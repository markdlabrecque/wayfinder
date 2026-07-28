//! Normaliser + differ for the Solr-vs-Wayfinder differential harness
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
use std::path::Path;

/// A value plus the list of JSON paths the normaliser touched (dropped or
/// otherwise altered) to produce it.
#[derive(Debug, Clone)]
pub struct Normalized {
    pub value: Value,
    pub touched: Vec<String>,
}

/// Normalises a Solr/Wayfinder response envelope per PRD §8 and
/// `docs/solr-ref-findings.md`:
/// - drops `responseHeader.QTime` (always variable).
/// - drops `_version_` / `_root_` from every doc in `response.docs`
///   (Wayfinder's explicit default-`fl` decision, finding 9 — reuses/extends
///   `normalize_envelope` in `tests/common/mod.rs`, does not duplicate it).
/// - on an error envelope, drops `error.msg` and `error.metadata` (finding
///   10) — `error.code` and the HTTP status are the only parts compared.
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

/// Structural diff of two already-normalised JSON values (human-readable
/// `path: expected vs actual`, not a bare `assert_eq!`). Recurses through
/// objects and arrays; any object key literally named `score` is compared
/// with `score_tolerance()` instead of exact equality, and its path is
/// recorded in `touched` regardless of whether it matched. Every other
/// mismatch — an added/removed key, a changed value, a reordered array
/// element — is a `Diff`. No key is ever silently ignored (spec: "no
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
            for key in keys {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (e.get(key), a.get(key)) {
                    (Some(ev), Some(av)) if key == "score" => {
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

/// Ranked-ID-list comparison for free-text relevance queries (PRD §8:
/// "compare ranked ID lists, not just result sets"). Order matters: an
/// identical multiset of ids in a different order is a `Diff`, not a pass.
pub fn diff_ranked_ids(expected_ids: &[String], actual_ids: &[String]) -> Vec<Diff> {
    let mut diffs = Vec::new();
    if expected_ids.len() != actual_ids.len() {
        diffs.push(Diff {
            path: "response.docs".to_string(),
            expected: format!("{} ids", expected_ids.len()),
            actual: format!("{} ids", actual_ids.len()),
        });
    }
    let n = expected_ids.len().max(actual_ids.len());
    for i in 0..n {
        let e = expected_ids
            .get(i)
            .map(String::as_str)
            .unwrap_or("<missing>");
        let a = actual_ids.get(i).map(String::as_str).unwrap_or("<missing>");
        if e != a {
            diffs.push(Diff {
                path: format!("response.docs[{i}].id"),
                expected: e.to_string(),
                actual: a.to_string(),
            });
        }
    }
    diffs
}

/// Extracts `response.docs[].id` as an ordered `Vec<String>`, for use with
/// `diff_ranked_ids`.
pub fn doc_ids(envelope: &Value) -> Vec<String> {
    envelope
        .pointer("/response/docs")
        .and_then(|d| d.as_array())
        .map(|docs| {
            docs.iter()
                .filter_map(|d| d.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// One line of `solr-ref/manifest.tsv`: `name<TAB>status<TAB>path-with-query`,
/// tolerant of extra trailing tab-separated columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub name: String,
    pub status: u16,
    pub path: String,
}

/// Loads a manifest file in `solr-ref/manifest.tsv` format: one entry per
/// line, skipping blank lines and `#`-comment lines, tolerant of extra
/// trailing columns beyond `name`, `status`, `path`.
pub fn load_manifest(path: &Path) -> Vec<ManifestEntry> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read manifest {}: {e}", path.display()));
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut cols = line.split('\t');
            let name = cols
                .next()
                .unwrap_or_else(|| panic!("manifest line missing name column: {line:?}"))
                .to_string();
            let status: u16 = cols
                .next()
                .unwrap_or_else(|| panic!("manifest line missing status column: {line:?}"))
                .parse()
                .unwrap_or_else(|e| panic!("manifest status column must be a u16: {e}"));
            let path = cols
                .next()
                .unwrap_or_else(|| panic!("manifest line missing path column: {line:?}"))
                .to_string();
            // Any further tab-separated columns are tolerated and ignored.
            ManifestEntry { name, status, path }
        })
        .collect()
}

/// Fetches `GET <base_url>/<path_and_query>` from a live Solr and returns
/// the HTTP status plus parsed JSON body. Only ever called when
/// `WAYFINDER_DIFF_SOLR=1` is set (see `tests/differential.rs`) — never
/// invoked by plain `cargo test`, so it adds no network dependency to the
/// default suite.
pub fn fetch_live(base_url: &str, path_and_query: &str) -> (u16, Value) {
    let url = format!("{base_url}/{path_and_query}");
    let output = std::process::Command::new("curl")
        .args(["-s", "-w", "\n%{http_code}", &url])
        .output()
        .unwrap_or_else(|e| panic!("failed to run curl against {url}: {e}"));
    assert!(
        output.status.success(),
        "curl exited non-zero fetching {url}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = text
        .rsplit_once('\n')
        .unwrap_or_else(|| panic!("curl output for {url} missing trailing status line: {text:?}"));
    let status: u16 = status
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("curl status code for {url} must be numeric: {e}"));
    let value: Value = serde_json::from_str(body.trim())
        .unwrap_or_else(|e| panic!("response body from {url} must be valid JSON: {e}"));
    (status, value)
}
