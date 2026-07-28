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
/// - on an error envelope, drops `error.msg`, `error.metadata`, and
///   `error.trace` (finding 10, extended by finding 45's captured 500 —
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
        // `error.msg` above (finding 10, extended by finding 45).
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
    // `-g` disables curl's URL globbing, or the `[` in `err_bad_syntax`'s
    // `fq=category:[unclosed` is read as a glob range and curl exits non-zero
    // before issuing a request. `capture.sh`'s `cap()` has always passed `-sg`
    // for the same reason; this side was missing it, which broke live mode for
    // the whole manifest (issue #1 follow-up: "live mode never exercised
    // end-to-end").
    let output = std::process::Command::new("curl")
        .args(["-sg", "-w", "\n%{http_code}", &url])
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

/// One line of `solr-ref/manifest-errors.tsv`'s 6-column format: `name,
/// status, method, url-after-/solr/, body, [base-url]`. `body` and
/// `base_url` may be empty/absent columns — `None` when so, never an empty
/// `Some("")` — since a present-but-empty body (e.g. a GET) is different from
/// no body column at all only in this format's intent, and callers
/// (`common::request_full`, live `curl -X ... -d ...`) need to tell "send no
/// body" apart from "send an empty body".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestErrorEntry {
    pub name: String,
    pub status: u16,
    pub method: String,
    /// The URL after `/solr/`, e.g. `content/update?commit=true&wt=json` or
    /// `facets/select?q=*:*&wt=json` — core-qualified, unlike
    /// `ManifestEntry::path` which is always relative to the one core
    /// `manifest.tsv` GETs against.
    pub url: String,
    pub body: Option<String>,
    /// Defaults to the canonical `http://localhost:8983/solr` base when the
    /// column is absent — every row without one belongs to the reference
    /// container on the default port.
    pub base_url: Option<String>,
}

/// Loads `solr-ref/manifest-errors.tsv`'s 6-column format, skipping blank
/// lines and `#`-comment lines exactly like `load_manifest` — but that
/// function stays untouched (3-column format only); this is a new,
/// independent loader (issue #31: "keep `load_manifest` untouched for the
/// 3-column file").
pub fn load_manifest_errors(path: &Path) -> Vec<ManifestErrorEntry> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read manifest-errors {}: {e}", path.display()));
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut cols = line.split('\t');
            let name = cols
                .next()
                .unwrap_or_else(|| panic!("manifest-errors line missing name column: {line:?}"))
                .to_string();
            let status: u16 = cols
                .next()
                .unwrap_or_else(|| panic!("manifest-errors line missing status column: {line:?}"))
                .parse()
                .unwrap_or_else(|e| panic!("manifest-errors status column must be a u16: {e}"));
            let method = cols
                .next()
                .unwrap_or_else(|| panic!("manifest-errors line missing method column: {line:?}"))
                .to_string();
            let url = cols
                .next()
                .unwrap_or_else(|| panic!("manifest-errors line missing url column: {line:?}"))
                .to_string();
            let body = cols.next().filter(|s| !s.is_empty()).map(str::to_string);
            let base_url = cols.next().filter(|s| !s.is_empty()).map(str::to_string);
            ManifestErrorEntry {
                name,
                status,
                method,
                url,
                body,
                base_url,
            }
        })
        .collect()
}

/// Live counterpart of `fetch_live`, for `manifest-errors.tsv` rows: issues
/// `<method>` (optionally with `-d <body>`) against
/// `<base_url>/<path_and_query>` and returns the HTTP status plus parsed JSON
/// body. Only ever called under `WAYFINDER_DIFF_SOLR=1`.
pub fn fetch_live_full(
    base_url: &str,
    method: &str,
    path_and_query: &str,
    body: Option<&str>,
) -> (u16, Value) {
    let (status, raw) = fetch_live_full_raw(base_url, method, path_and_query, body);
    let value: Value = serde_json::from_str(raw.trim()).unwrap_or_else(|e| {
        panic!(
            "response body from {base_url}/{path_and_query} must be valid JSON: {e} (body: \
             {raw:?})"
        )
    });
    (status, value)
}

/// Status-only counterpart of `fetch_live_full`, for rows whose expected
/// body is not JSON at all (`err_missing_core`'s HTML easter egg is the one
/// `ACCEPTED_DIVERGENCES` member with a non-JSON fixture) — `fetch_live_full`
/// would panic trying to parse it, but the live counterpart of an accepted
/// divergence only ever needs to re-confirm the status code.
pub fn fetch_live_status(
    base_url: &str,
    method: &str,
    path_and_query: &str,
    body: Option<&str>,
) -> u16 {
    fetch_live_full_raw(base_url, method, path_and_query, body).0
}

/// Shared curl plumbing for `fetch_live_full`/`fetch_live_status`: issues
/// `<method>` (optionally with `-d <body>`) against
/// `<base_url>/<path_and_query>` and returns the HTTP status plus the raw
/// response text, unparsed.
fn fetch_live_full_raw(
    base_url: &str,
    method: &str,
    path_and_query: &str,
    body: Option<&str>,
) -> (u16, String) {
    let url = format!("{base_url}/{path_and_query}");
    let mut args = vec![
        "-sg".to_string(),
        "-w".to_string(),
        "\n%{http_code}".to_string(),
        "-X".to_string(),
        method.to_string(),
    ];
    if let Some(b) = body {
        args.push("-d".to_string());
        args.push(b.to_string());
    }
    args.push(url.clone());
    let output = std::process::Command::new("curl")
        .args(&args)
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
    (status, body.to_string())
}

/// A quick reachability probe for a manifest-errors row's effective base
/// URL, so an absent per-issue container (8984/8985/8986) is a printed,
/// named skip rather than a silent one or a hard failure. The default 8983
/// base must always answer — a row on it that fails this probe is a real
/// problem, not a skip.
pub fn live_reachable(base_url: &str) -> bool {
    // `admin/ping`-shaped probe: any HTTP response at all (even a 404) means the
    // base URL has something listening, which is all this needs to know — the
    // per-row status/JSON comparisons are the harness's own job, not this
    // probe's. A short `--max-time` keeps an absent per-issue container
    // (8984/8985/8986) from stalling the whole test.
    std::process::Command::new("curl")
        .args(["-sg", "-o", "/dev/null", "--max-time", "2", base_url])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
