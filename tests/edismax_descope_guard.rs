//! Expiring guard for the edismax v1 six-param descope (#136, PRD §5 "v1
//! exception — edismax").
//!
//! PRD §5 lists `bf` function queries, `pf2`/`pf3`, `ps`, `stopwords`, and
//! `lowercaseOperators` as **Out** for v1. That descope is not "nobody asked
//! for it yet" — it is ratified by two pieces of frozen capture evidence:
//!
//!   1. None of the six is ever *sent by the client* across the 28 committed
//!      traces in `solr-ref/search-api/trace/`, on either of the two channels
//!      a Solr param can arrive on: as a request query-string parameter name,
//!      and as a local param inside a `{!...}` block in a query-string value
//!      (`{!edismax qf='...'}` — the form `search_api_solr` actually uses, so
//!      scanning query-string names alone would miss the one channel that
//!      matters).
//!   2. None of the six appears in the `captured_parameters` denominator in
//!      `coverage/search_api_coverage_contract.json`.
//!
//! Per CLAUDE.md's rule for deliberate skips, this file must fail the day
//! that evidence stops holding — i.e. the day a future capture trace or a
//! regenerated coverage contract starts mentioning any of the six. That is
//! this file's whole point: it is a **self-deleting** guard. When it goes
//! red, the fix is not to edit this file to make it pass again — it is to
//! revisit PRD §5's descope (see issue #136) with the new evidence in hand.
//!
//! It also currently holds the (deliberately red, until stage 2's PRD edits
//! land) assertions that ratify the descope in prose and resolve the two
//! documented PRD defects: `bf`/`{!func}` listed with two different
//! dispositions, and `boost` described as unconditionally supported when
//! only its constant form is implemented (finding 83).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// The six params PRD §5 lists as **Out** for v1 edismax.
const DESCOPED_PARAMS: &[&str] = &["bf", "pf2", "pf3", "ps", "stopwords", "lowercaseOperators"];

const TRACE_DIR: &str = "solr-ref/search-api/trace";
const CONTRACT_JSON: &str = include_str!("../coverage/search_api_coverage_contract.json");
const PRD: &str = include_str!("../docs/PRD.md");

#[derive(Deserialize)]
struct Contract {
    captured_parameters: Vec<CapturedParameter>,
}

#[derive(Deserialize)]
struct CapturedParameter {
    name: String,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn trace_files() -> Vec<PathBuf> {
    let dir = root().join(TRACE_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

fn load(path: &Path) -> Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn percent_decode(input: &str) -> String {
    let mut output = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => output.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("URL hex is UTF-8");
                output.push(u8::from_str_radix(hex, 16).expect("URL percent escape is hex"));
                i += 2;
            }
            byte => output.push(byte),
        }
        i += 1;
    }
    String::from_utf8(output).expect("captured query values are UTF-8")
}

/// Collect the local-param *keys* of every `{!...}` block in a decoded
/// query-string value: from `{!edismax qf='a b' ps=2}` this yields `qf` and
/// `ps`. This is the channel `search_api_solr` actually sends edismax params
/// on — they never appear as top-level query-string names — so a guard that
/// scans only names cannot see a descoped param arriving.
///
/// ponytail: a deliberately shallow lexer. It takes every `ident=` inside the
/// braces as a key, so a `=` appearing inside a quoted local-param *value*
/// (e.g. `qf='a=b'`) would also be read as a key. That direction only ever
/// over-collects, which for a guard whose failure mode of concern is missing
/// an occurrence is the safe direction; the corpus has no such value today.
/// Consequence if that ever changes: a value like `bq='title:"ps=2"'` yields a
/// spurious `ps` key and a false RED, whose failure message will wrongly point
/// at the descope. A hit on a param that only appears inside a quoted value is
/// a lexer artifact, not evidence the descope needs revisiting -- fix it by
/// skipping `=` inside a `'`/`"` region rather than by weakening the guard.
fn collect_local_param_keys(value: &str, names: &mut BTreeSet<String>) {
    let mut rest = value;
    while let Some(open) = rest.find("{!") {
        let inner_start = open + 2;
        let Some(close) = rest[inner_start..].find('}') else {
            break;
        };
        let inner = &rest[inner_start..inner_start + close];
        for (i, _) in inner.match_indices('=') {
            let key: String = inner[..i]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                .collect::<Vec<char>>()
                .into_iter()
                .rev()
                .collect();
            if !key.is_empty() {
                names.insert(key);
            }
        }
        rest = &rest[inner_start + close + 1..];
    }
}

/// Every client-sent parameter *name* observed across every committed trace,
/// decoded and deduplicated: both top-level query-string names and the local
/// param keys inside `{!...}` blocks in query-string values. Query values
/// themselves and request bodies are not scanned as text (see
/// `no_trace_carries_a_form_encoded_body`, which guards the body-scope
/// assumption).
fn observed_query_param_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in trace_files() {
        let capture = load(&file);
        let request = capture["request"]
            .as_object()
            .unwrap_or_else(|| panic!("{}: trace request object", file.display()));
        let path = request["path"]
            .as_str()
            .unwrap_or_else(|| panic!("{}: trace request path", file.display()));
        let query = path.split_once('?').map_or("", |(_, q)| q);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (raw_name, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
            names.insert(percent_decode(raw_name));
            collect_local_param_keys(&percent_decode(raw_value), &mut names);
        }
    }
    names
}

fn contract() -> Contract {
    serde_json::from_str(CONTRACT_JSON).expect("coverage contract must be valid JSON")
}

/// The "### v1 exception — edismax" section of the PRD, from its heading up
/// to (but not including) the next `###` heading.
fn edismax_section() -> &'static str {
    let start = PRD
        .find("### v1 exception — edismax")
        .expect("PRD must still contain the v1 exception — edismax section");
    let rest = &PRD[start..];
    let end = rest[1..]
        .find("\n### ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The section split into blank-line-separated blocks. Content assertions
/// below check co-occurrence *within one block* rather than anywhere in the
/// section: a bare section-wide substring search for a token like `28`,
/// `zero`, or `function` can be satisfied by unrelated prose that happens to
/// contain it, which would pass without the claim actually being recorded.
fn edismax_paragraphs() -> Vec<&'static str> {
    edismax_section()
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// A single Markdown list item of the section's In/Out list, e.g.
/// `list_item("- **Out:**")`, from its bullet up to the next bullet or blank
/// line. Needed because both bullets live in one blank-line-separated block.
fn list_item(bullet: &str) -> String {
    let section = edismax_section();
    let start = section
        .find(bullet)
        .unwrap_or_else(|| panic!("PRD §5's edismax section must still have a `{bullet}` bullet"));
    let rest = &section[start + bullet.len()..];
    let end = rest
        .match_indices('\n')
        .find(|(i, _)| {
            let next = &rest[i + 1..];
            next.starts_with("- ") || next.starts_with('\n') || next.is_empty()
        })
        .map_or(rest.len(), |(i, _)| i);
    rest[..end].to_lowercase()
}

// --- The expiring guard itself -------------------------------------------
//
// These assertions hold today because the evidence holds today. They are
// meant to be green now and to go red the instant a future capture or a
// regenerated coverage contract contradicts the descope's premise — that is
// what makes them a guard rather than a one-off audit.

#[test]
fn trace_corpus_is_the_28_traces_the_descope_was_ratified_against() {
    let count = trace_files().len();
    assert_eq!(
        count, 28,
        "the edismax six-param descope (#136, PRD §5) was ratified against exactly 28 \
         committed traces in solr-ref/search-api/trace/; the corpus now has {count}. If new \
         traces were added, re-check every descoped param against them before treating this \
         guard as still valid — see issue #136."
    );
}

#[test]
fn none_of_the_six_descoped_edismax_params_appear_as_a_request_query_parameter_in_any_trace() {
    let observed = observed_query_param_names();
    for param in DESCOPED_PARAMS {
        assert!(
            !observed.contains(*param),
            "`{param}` is now sent by the client in a committed trace under \
             solr-ref/search-api/trace/ — either as a query-string parameter name or as a local \
             param inside a `{{!...}}` block in a query value. PRD §5 (issue #136) descoped \
             `{param}` for v1 specifically because no captured client ever sent it — that \
             premise no longer holds and the descope must be revisited, not silently kept."
        );
    }
}

/// The scan's positive control: it must actually *see* the local-param channel
/// `search_api_solr` uses. `qf` never appears as a query-string parameter name
/// anywhere in the corpus — only as `{!edismax qf='...'}` inside `q` — so
/// observing `qf` proves the `{!...}` scan is live. Without this, the local
/// param scan could silently stop working and the descope guard above would go
/// permanently, falsely green.
#[test]
fn the_scan_sees_the_local_param_channel_the_client_actually_uses() {
    let observed = observed_query_param_names();
    assert!(
        observed.contains("qf"),
        "the scan no longer observes `qf`, which the captured client sends only as a local param \
         inside `{{!edismax qf='...'}}`. That is the exact channel a future descoped param would \
         arrive on, so the descope guard is now blind and must be fixed before it is trusted."
    );
    // Restated as the reason it is a positive control: `qf` is invisible to a
    // query-string-names-only scan.
    let mut names_only = BTreeSet::new();
    for file in trace_files() {
        let capture = load(&file);
        let path = capture["request"]["path"].as_str().expect("trace path");
        let query = path.split_once('?').map_or("", |(_, q)| q);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (raw_name, _) = pair.split_once('=').unwrap_or((pair, ""));
            names_only.insert(percent_decode(raw_name));
        }
    }
    assert!(
        !names_only.contains("qf"),
        "`qf` now appears as a top-level query-string parameter name, so it is no longer a valid \
         positive control for the local-param scan; pick another local-param-only key (the \
         corpus's local-param key set is `qf`/`key`) or drop this control."
    );
}

/// The scan reads query strings, not request bodies. That is sound only while
/// no trace carries form-encoded parameters in its body: Solr accepts
/// `application/x-www-form-urlencoded` POSTs to `/select`, and those params
/// would be invisible to the scan. Today's only POST (00001.json) is
/// `application/json` update traffic. This makes that one assumption break
/// loudly rather than silently.
///
/// It does not cover every body-borne param: Solr's JSON Request API also
/// accepts `application/json` POSTs to `/select` with params under
/// `{"params":{...}}`, and 00001 proves this client does POST JSON. A future
/// JSON-body `/select` trace could carry a descoped param invisibly. The
/// 28-trace count guard is the backstop, since any new trace trips it.
#[test]
fn no_trace_carries_a_form_encoded_body() {
    for file in trace_files() {
        let capture = load(&file);
        let headers = capture["request"]["headers"]
            .as_object()
            .unwrap_or_else(|| panic!("{}: trace request headers object", file.display()));
        let content_type = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .and_then(|(_, v)| v.as_str())
            .unwrap_or("");
        assert!(
            !content_type
                .to_ascii_lowercase()
                .contains("application/x-www-form-urlencoded"),
            "{} carries a form-encoded request body. The descoped-param scan reads query strings \
             only, so form-encoded params are invisible to it and the descope guard could stay \
             green while the client sends `bf`, `ps`, or another descoped param in a POST body. \
             Extend the scan to parse form bodies before trusting the guard again.",
            file.display()
        );
    }
}

#[test]
fn none_of_the_six_descoped_edismax_params_appear_in_the_coverage_denominator() {
    let contract = contract();
    let names: BTreeSet<&str> = contract
        .captured_parameters
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    for param in DESCOPED_PARAMS {
        assert!(
            !names.contains(param),
            "`{param}` now appears in coverage/search_api_coverage_contract.json's \
             captured_parameters. PRD §5 (issue #136) descoped `{param}` for v1 because building \
             it would move the coverage denominator by zero — that is no longer true and the \
             descope must be revisited."
        );
    }
}

/// The one real trap in the scan: `stopwords` is both an edismax *request
/// parameter* (descoped) and an analyzer *filter* name that legitimately
/// appears in schema/fieldtypes response bodies and in
/// `configset/stopwords_en.txt`. A guard that greps trace files for the bare
/// substring "stopwords" would false-positive on trace 00020 (schema
/// fieldtypes) forever and be worthless. This test pins both directions:
/// the filter sense is present in the evidence, and the guard's actual
/// scan — parameter *names* (query-string names plus `{!...}` local-param
/// keys), not full-text search — correctly does not see it.
#[test]
fn stopwords_the_analyzer_filter_is_present_but_does_not_trip_the_stopwords_param_guard() {
    let schema_trace = root().join(TRACE_DIR).join("00020.json");
    let capture = load(&schema_trace);
    let response_body = capture["response"]["body"]
        .as_str()
        .expect("00020.json response body must be a string");
    assert!(
        response_body.contains("stopwords_en.txt") || response_body.contains("stopwords_und.txt"),
        "expected trace 00020.json (schema/fieldtypes) to still document the stopwords analyzer \
         filter — if this changed, the positive control this test relies on is gone and it needs \
         a new source trace"
    );

    // The filter-sense occurrence lives in the *response body*, under a
    // fieldtype's "words" key — never as a parameter the client sends. The
    // guard above scans only client-sent parameter names (query-string names
    // and `{!...}` local-param keys), so it must not see this occurrence as
    // the `stopwords` param.
    let observed = observed_query_param_names();
    assert!(
        !observed.contains("stopwords"),
        "the stopwords *parameter* guard fired on trace 00020, which only carries the stopwords \
         analyzer *filter* in its response body — the scan is over-matching text instead of \
         query-parameter names and needs fixing, not the descope"
    );
}

// --- PRD defects named in #136 --------------------------------------------
//
// These are red until stage 2 edits docs/PRD.md. They define what "fixed"
// means for the two documented defects and for recording the ratification.
//
// ponytail: these are keyword *co-occurrence* checks scoped to one
// blank-line-separated block, not semantic checks. Scoping to a block rules
// out the cheap coincidence of an unrelated passage elsewhere in the section
// supplying a token, but it cannot distinguish a claim from its negation:
// prose reading "`boost` works for constant *and* function forms", or "`bf`
// will not land even in v4", would satisfy the co-occurrence and pass. That
// ceiling is inherent to asserting on prose from a test; the assertions are
// tripwires against the specific defect text being restored, not a proof that
// the replacement prose says the right thing. Correctness of the wording is a
// review responsibility.

#[test]
fn prd_edismax_section_gives_bf_a_single_disposition_pointing_at_v4() {
    let section = edismax_section();
    assert!(
        section.contains("bf"),
        "the v1 exception — edismax section should still mention `bf` as an Out item"
    );
    assert!(
        edismax_paragraphs()
            .iter()
            .any(|p| p.contains("bf") && p.contains("v4")),
        "PRD §5's v1 edismax section lists `bf` function queries as Out without saying where \
         they eventually land, while the v4 phase table separately lists `bf`/`{{!func}}` as in \
         scope for v4 — two dispositions for the same param. Issue #136 asks for a single \
         disposition: the v1 text should point at v4 (e.g. \"deferred to v4\") in the same breath \
         as `bf`, instead of independently restating the exclusion."
    );
    let out = list_item("- **Out:**");
    assert!(
        !out.contains("bf") || out.contains("v4"),
        "the **Out:** bullet still names `bf` without pointing at v4, which is the defect #136 \
         describes: the same param carrying an independent v1 exclusion and a separate v4 \
         commitment. Either drop `bf` from the bullet or have the bullet defer it to v4."
    );
}

#[test]
fn prd_edismax_section_states_boost_precisely_as_constant_only() {
    // Note the section already said "function" before this fix (in "`bf`
    // function queries" and "full Solr function-query syntax"), so a
    // section-wide search for that token proves nothing about `boost`. The
    // claim is pinned to a single block that ties `boost` to *both* the
    // implemented constant form and the unimplemented function-query form.
    assert!(
        edismax_paragraphs()
            .iter()
            .map(|p| p.to_lowercase())
            .any(|p| p.contains("boost") && p.contains("constant") && p.contains("function")),
        "PRD §5 lists `boost` as **In** and describes it only as \"a multiplicative wrapper\", \
         which reads as full boost support. Finding 83 records that real Solr's `boost` is a \
         function-query parameter and Wayfinder implements no function-query evaluator at all — \
         only the constant-numeric case actually works. Issue #136 asks the PRD to state the \
         implemented subset precisely: one place must say, together, that `boost` is supported in \
         its constant form and not as a function query."
    );
    let in_bullet = list_item("- **In:**");
    assert!(
        !in_bullet.contains("boost") || in_bullet.contains("constant"),
        "the **In:** bullet still lists `boost` unqualified. Only the constant-numeric form is \
         implemented (finding 83), so the bullet must qualify it rather than leaving a reader of \
         the scope list believing function-query `boost` works."
    );
}

#[test]
fn prd_edismax_section_records_the_capture_ratification_evidence() {
    // All four facts must land in *one* block. Checked section-wide, tokens
    // like "28" and "zero" are cheap coincidences (a line number, an
    // unrelated count) and would let the test pass with the evidence not
    // actually recorded.
    let ratification = edismax_paragraphs()
        .iter()
        .map(|p| p.to_lowercase())
        .find(|p| p.contains("28") && (p.contains("trace") || p.contains("capture")))
        .unwrap_or_else(|| {
            panic!(
                "issue #136 asks §5's edismax section to record that the descope was checked \
                 against the 28 v1.5 capture traces, not left as prose alone: no single passage \
                 ties the count 28 to the traces/capture"
            )
        });
    assert!(
        ratification.contains("zero"),
        "the ratification passage cites the 28 traces but does not state the finding: zero client \
         usage of the six descoped params across them"
    );
    assert!(
        ratification.contains("coverage"),
        "the ratification passage should also record the second half of the evidence — none of \
         the six is in the coverage denominator, so building them moves coverage by zero"
    );
    assert!(
        ratification.contains("#136"),
        "the ratification should reference issue #136 so a future reader can find the audit \
         that backs the descope"
    );
}
