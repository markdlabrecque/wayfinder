//! Hermetic Search API coverage report for the frozen #55 capture.
//!
//! The production contract supplies the denominator and provenance only. The
//! report calculates classifications from the real router, strict allowlists,
//! and typed renderer/semantic capability surfaces below; it never deserializes
//! or reads a contract `covered` value.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

use crate::ROUTES;

const CONTRACT: &str = include_str!("../coverage/search_api_coverage_contract.json");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Trace {
    file: String,
    seq: u64,
    method: String,
    endpoint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ContractItem {
    id: String,
    trace: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SemanticParameter {
    name: String,
    variant: String,
    values: Vec<String>,
    trace: Vec<String>,
    occurrences: Vec<Occurrence>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BodyVariant {
    kind: String,
    trace: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SemanticItem {
    id: String,
    trace: Vec<String>,
    parameters: Vec<SemanticParameter>,
    body_variants: Vec<BodyVariant>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Consumer {
    source: String,
    symbol: String,
    evidence: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ResponseItem {
    id: String,
    trace: Vec<String>,
    consumer: Consumer,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedParameter {
    name: String,
    occurrences: Vec<Occurrence>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Occurrence {
    value: String,
    trace: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    traces: Vec<Trace>,
    captured_parameters: Vec<CapturedParameter>,
    endpoints: Vec<ContractItem>,
    request_semantics: Vec<SemanticItem>,
    response_fields: Vec<ResponseItem>,
}

#[derive(Debug, Serialize)]
struct Evidence {
    kind: &'static str,
    source: String,
}

#[derive(Debug, Serialize)]
struct ReportedItem {
    id: String,
    covered: bool,
    trace: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    parameters: Vec<SemanticParameter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    consumer: Option<Consumer>,
    evidence: Vec<Evidence>,
}

#[derive(Debug, Serialize)]
struct Bucket {
    items: Vec<ReportedItem>,
    covered: usize,
    uncovered: usize,
    total: usize,
    fraction: String,
    uncovered_items: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Totals {
    covered: usize,
    uncovered: usize,
    total: usize,
    fraction: String,
}

#[derive(Debug, Serialize)]
struct Report {
    traces: Vec<Trace>,
    endpoints: Bucket,
    request_semantics: Bucket,
    response_fields: Bucket,
    overall: Totals,
}

fn contract() -> Contract {
    let contract: Contract = serde_json::from_str(CONTRACT)
        .expect("built-in Search API coverage contract is valid JSON");
    validate_contract(&contract);
    contract
}

/// Reject an accidentally partial or internally inconsistent checked-in
/// denominator before it can become a plausible-looking coverage number.
fn validate_contract(contract: &Contract) {
    assert_eq!(
        contract.traces.len(),
        28,
        "coverage contract has every frozen trace"
    );
    assert_eq!(
        contract.captured_parameters.len(),
        43,
        "coverage contract has every captured parameter"
    );
    assert_eq!(
        contract.endpoints.len(),
        9,
        "coverage contract has every endpoint"
    );
    assert_eq!(
        contract.request_semantics.len(),
        51,
        "coverage contract has every semantic variant"
    );
    assert_eq!(
        contract.response_fields.len(),
        15,
        "coverage contract has every client-consumed response field"
    );

    let trace_names = contract
        .traces
        .iter()
        .map(|trace| trace.file.as_str())
        .collect::<HashSet<_>>();
    let parameter_names = contract
        .captured_parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect::<HashSet<_>>();
    for parameter in &contract.captured_parameters {
        assert!(
            !parameter.occurrences.is_empty(),
            "captured parameter has occurrences"
        );
        for occurrence in &parameter.occurrences {
            assert!(
                !occurrence.value.is_empty(),
                "captured parameter value is nonempty"
            );
            assert!(
                occurrence
                    .trace
                    .iter()
                    .all(|trace| trace_names.contains(trace.as_str()))
            );
        }
    }
    for endpoint in &contract.endpoints {
        let expected = contract
            .traces
            .iter()
            .filter(|trace| format!("{} {}", trace.method, trace.endpoint) == endpoint.id)
            .map(|trace| trace.file.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            endpoint.trace, expected,
            "endpoint provenance contains every frozen exchange"
        );
    }
    let captured_occurrences = contract
        .captured_parameters
        .iter()
        .flat_map(|parameter| {
            parameter.occurrences.iter().flat_map(move |occurrence| {
                occurrence.trace.iter().map(move |trace| {
                    (
                        parameter.name.as_str(),
                        occurrence.value.as_str(),
                        trace.as_str(),
                    )
                })
            })
        })
        .collect::<HashSet<_>>();
    let mut semantic_occurrences = HashSet::new();
    for semantic in &contract.request_semantics {
        assert!(!semantic.parameters.is_empty() || !semantic.body_variants.is_empty());
        assert!(
            semantic
                .trace
                .iter()
                .all(|trace| trace_names.contains(trace.as_str()))
        );
        assert!(
            semantic
                .parameters
                .iter()
                .all(|parameter| parameter_names.contains(parameter.name.as_str()))
        );
        for parameter in &semantic.parameters {
            assert_eq!(
                parameter.values.iter().collect::<HashSet<_>>(),
                parameter
                    .occurrences
                    .iter()
                    .map(|occurrence| &occurrence.value)
                    .collect::<HashSet<_>>(),
                "semantic parameter retains per-value occurrence provenance"
            );
            assert_eq!(
                parameter.trace.iter().collect::<HashSet<_>>(),
                parameter
                    .occurrences
                    .iter()
                    .flat_map(|occurrence| occurrence.trace.iter())
                    .collect::<HashSet<_>>(),
                "semantic parameter retains exact trace provenance"
            );
            for occurrence in &parameter.occurrences {
                for trace in &occurrence.trace {
                    let triple = (
                        parameter.name.as_str(),
                        occurrence.value.as_str(),
                        trace.as_str(),
                    );
                    assert!(
                        captured_occurrences.contains(&triple),
                        "semantic occurrence is captured"
                    );
                    semantic_occurrences.insert(triple);
                }
            }
        }
    }
    assert_eq!(
        semantic_occurrences, captured_occurrences,
        "every captured parameter occurrence belongs to a semantic class"
    );
    for field in &contract.response_fields {
        assert!(
            field
                .trace
                .iter()
                .all(|trace| trace_names.contains(trace.as_str()))
        );
        assert_eq!(
            field.consumer.source,
            "coverage/search_api_solr_4.4.0_source_evidence.json"
        );
        assert!(field.consumer.symbol.contains("::"));
        assert!(!field.consumer.evidence.is_empty());
    }
}

fn endpoint_covered(id: &str) -> bool {
    let (method, path) = id
        .split_once(' ')
        .expect("endpoint denominator id starts with an HTTP method");
    ROUTES
        .iter()
        .any(|route| route.path == path && (route.accepts_method)(method))
}

/// A hermetic, strict-parameter app used only by the coverage command. The
/// report asks the same routed handlers that serve requests; it does not carry
/// a second Boolean capability inventory.
struct ProbeApp {
    app: Router,
    _workspace: ProbeWorkspace,
}

struct ProbeWorkspace {
    root: tempfile::TempDir,
}

impl ProbeWorkspace {
    fn new() -> Self {
        Self {
            root: tempfile::Builder::new()
                .prefix("wayfinder-coverage-")
                .tempdir()
                .expect("create hermetic coverage workspace"),
        }
    }
}

const PROBE_SCHEMA: &str = r#"
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
name = "category"
type = "string"
stored = true
fast = true
multi_valued = true

[[fields]]
name = "rating"
type = "int"
stored = true
fast = true

[[fields]]
name = "created"
type = "date"
stored = true
fast = true

[[fields]]
name = "featured"
type = "string"
stored = true
fast = true

# One stored dynamic rule, so the `fl=*,score` probes' real-app leg can tell a
# full wildcard expansion from a declared-fields-only one. `render_doc` walks
# declared `[[fields]]` and stored dynamic fields in two separate loops, and
# `solr-ref/search-api/trace/00010.json` (real `fl=*,score`) returns both
# classes -- with a declared-only schema here, a probe that fixed one loop and
# not the other would still read covered.
[[dynamic_fields]]
pattern = "ss_*"
type = "string"
stored = true
fast = true
"#;

const PROBE_DOCS: &str = r#"[
  {"id":"doc1","ss_sku":"sku-doc1","body":"quick brown fox rocket","category":["animals","classic"],"rating":3,"created":"2024-01-02T00:00:00Z","featured":"true"},
  {"id":"doc2","ss_sku":"sku-doc2","body":"quick fox rocket","category":["garden"],"rating":1,"created":"2024-01-01T00:00:00Z","featured":"false"},
  {"id":"doc3","body":"slow turtle","category":["misc"],"rating":5,"created":"2024-01-03T00:00:00Z","featured":"true"},
  {"id":"facet","body":"facet probe","category":["a","b","c","d","e","f","g","h","i","j","k"],"rating":0,"created":"2024-01-04T00:00:00Z","featured":"z"},
  {"id":"mlt1","body":"the chef prepared a delicious pasta dish with fresh tomatoes and basil","category":["cooking","italian"],"rating":10,"created":"2024-02-01T00:00:00Z","featured":"m"},
  {"id":"mlt2","body":"fresh basil and ripe tomatoes make a wonderful pasta sauce","category":["cooking","italian"],"rating":11,"created":"2024-02-02T00:00:00Z","featured":"m"},
  {"id":"mlt3","body":"grilling chicken with garlic and rosemary is a classic dinner","category":["cooking","grilling"],"rating":12,"created":"2024-02-03T00:00:00Z","featured":"m"},
  {"id":"mlt4","body":"roasted vegetables with olive oil and garlic taste amazing","category":["cooking","vegetarian"],"rating":13,"created":"2024-02-04T00:00:00Z","featured":"m"},
  {"id":"mlt5","body":"baking bread requires yeast flour water and patience","category":["cooking","baking"],"rating":14,"created":"2024-02-05T00:00:00Z","featured":"m"},
  {"id":"mlt6","body":"planting tomatoes and basil in the garden this spring","category":["gardening"],"rating":15,"created":"2024-02-06T00:00:00Z","featured":"m"},
  {"id":"mlt7","body":"the garden needs watering every morning during summer heat","category":["gardening"],"rating":16,"created":"2024-02-07T00:00:00Z","featured":"m"},
  {"id":"mlt8","body":"pruning rose bushes keeps the garden looking tidy","category":["gardening"],"rating":17,"created":"2024-02-08T00:00:00Z","featured":"m"},
  {"id":"mlt9","body":"composting kitchen scraps enriches garden soil naturally","category":["gardening"],"rating":18,"created":"2024-02-09T00:00:00Z","featured":"m"},
  {"id":"mlt10","body":"growing herbs like basil and rosemary indoors year round","category":["gardening","cooking"],"rating":19,"created":"2024-02-10T00:00:00Z","featured":"m"},
  {"id":"mlt11","ss_sku":"sku-mlt11","body":"astronomers observed a bright comet streaking across the night sky","category":["astronomy"],"rating":20,"created":"2024-02-11T00:00:00Z","featured":"m"},
  {"id":"mlt12","ss_sku":"sku-mlt12","body":"the telescope revealed distant galaxies and bright stars","category":["astronomy"],"rating":21,"created":"2024-02-12T00:00:00Z","featured":"m"},
  {"id":"mlt13","ss_sku":"sku-mlt13","body":"a lunar eclipse darkened the night sky for hours","category":["astronomy"],"rating":22,"created":"2024-02-13T00:00:00Z","featured":"m"},
  {"id":"mlt14","ss_sku":"sku-mlt14","body":"scientists study the orbit of planets around distant stars","category":["astronomy"],"rating":23,"created":"2024-02-14T00:00:00Z","featured":"m"},
  {"id":"mlt15","ss_sku":"sku-mlt15","body":"the night sky was clear enough to see the milky way","category":["astronomy"],"rating":24,"created":"2024-02-15T00:00:00Z","featured":"m"},
  {"id":"mlt16","body":"hiking through the mountains offers stunning views of the valley","category":["outdoors"],"rating":25,"created":"2024-02-16T00:00:00Z","featured":"m"},
  {"id":"mlt17","body":"camping near the lake was peaceful and quiet at night","category":["outdoors"],"rating":26,"created":"2024-02-17T00:00:00Z","featured":"m"},
  {"id":"mlt18","body":"the river flows quietly through the quiet forest valley","category":["outdoors"],"rating":27,"created":"2024-02-18T00:00:00Z","featured":"m"},
  {"id":"mlt19","body":"a short trip to buy office supplies and paper clips","category":["misc"],"rating":28,"created":"2024-02-19T00:00:00Z","featured":"m"},
  {"id":"mlt20","body":"nothing here relates to any other document in this corpus","category":["misc"],"rating":29,"created":"2024-02-20T00:00:00Z","featured":"m"}
]"#;

/// Seeded alongside `PROBE_DOCS`, but kept a separate batch on purpose: this
/// doc exists only so the `hl.snippets` probe has a corpus that can tell "the
/// cap is honored" apart from "the cap is ignored", and holding it out of
/// `PROBE_DOCS` keeps that intent legible next to the field-value assertions
/// the other probes make against `doc1`/`doc2`/`doc3`/`mlt*`.
///
/// `body` repeats a term unique to this doc ("gizmo") three times, each
/// occurrence separated by 100+ chars of filler that shares no term with any
/// other probe doc -- wide enough that a real multi-fragment highlighter would
/// produce three distinct, non-overlapping ~150-char snippet windows rather
/// than merging them into one. It carries no `category`/`rating`/`created`/
/// `featured` value, so the facet-count and sort probes are untouched by it.
const HL_SNIPPETS_PROBE_DOCS: &str = r#"[
  {"id":"hl-snippets-gizmo","body":"gizmo prototype unveiled at the trade show. the weather in the valley stayed mild and overcast for most of the week without much wind at all. a second gizmo shipment arrived at the warehouse yesterday. meanwhile the local council debated a new bridge proposal for nearly three hours last tuesday evening. engineers are already testing a third gizmo revision in the lab. several farmers reported an unusually early harvest this year thanks to the warm and sunny spring season."}
]"#;

/// Seeded alongside `PROBE_DOCS`/`HL_SNIPPETS_PROBE_DOCS`, again as its own
/// batch: this doc exists only so the `hl.fragsize=0` half of the
/// `select.highlight.fragsize` probe can tell "the whole field came back
/// unfragmented" apart from "a fragment happened to come back", which needs a
/// field long enough that a fragmenting highlighter and a whole-field
/// highlighter produce visibly different output. `doc1`'s `body` ("quick
/// brown fox rocket") is short enough that both strategies return the same
/// four words, so it cannot discriminate.
///
/// `body` is otherwise the same ~310-char paragraph captured against real
/// Solr 9 in `solr-ref/responses/hl_fragsize_zero_whole_field.json` /
/// `hl_fragsize_zero_whole_field_method_original.json` (issue #104): real
/// Solr's `hl.fragsize=0` returns this entire field as one highlighted
/// snippet, for both the default `hl.method` (unified) and
/// `hl.method=original`. The leading term is swapped from "quick" to
/// "wexford" (unique across `PROBE_DOCS`/`HL_SNIPPETS_PROBE_DOCS`) so seeding
/// this doc into the shared probe corpus doesn't change `numFound` for the
/// several other `"quick"`-keyed probes (`select.q.plain-query`,
/// `select.sort.*`, the edismax probes) that assert an exact count of 2 --
/// the fixture-backed assertion of the literal captured text lives in
/// `tests/highlighting.rs` against its own isolated single-doc app instead.
const HL_FRAGSIZE_PROBE_DOCS: &str = r#"[
  {"id":"hl-fragsize-long","body":"wexford prototype notes from the engineering standup this morning. the team reviewed the roadmap for the next quarter and discussed several open risks around supply chain timing. afterwards everyone broke for lunch and reconvened at two in the afternoon to continue the planning session for the rest of the week."}
]"#;

impl ProbeApp {
    async fn new() -> Self {
        let workspace = ProbeWorkspace::new();
        let schema = workspace.root.path().join("schema.toml");
        let config = workspace.root.path().join("wayfinder.toml");
        let data = workspace.root.path().join("data");
        std::fs::write(&schema, PROBE_SCHEMA).expect("write coverage probe schema");
        std::fs::write(&config, "strict_params = true\n").expect("write coverage probe config");
        std::fs::create_dir_all(&data).expect("create coverage probe data directory");
        let app = crate::app_with_config(&schema, &data, &config)
            .expect("build hermetic coverage probe app");
        let probe = Self {
            app,
            _workspace: workspace,
        };
        let (status, body) = probe
            .request(Method::POST, "content/update?commit=true", Some(PROBE_DOCS))
            .await;
        assert_eq!(status, StatusCode::OK, "seed coverage probe corpus: {body}");
        let (status, body) = probe
            .request(
                Method::POST,
                "content/update?commit=true",
                Some(HL_SNIPPETS_PROBE_DOCS),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "seed coverage probe hl.snippets corpus: {body}"
        );
        let (status, body) = probe
            .request(
                Method::POST,
                "content/update?commit=true",
                Some(HL_FRAGSIZE_PROBE_DOCS),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "seed coverage probe hl.fragsize corpus: {body}"
        );
        probe
    }

    async fn request(&self, method: Method, path: &str, body: Option<&str>) -> (StatusCode, Value) {
        let uri = if path.starts_with("/solr/") {
            path.to_string()
        } else if path.starts_with("content/") {
            format!("/solr/{path}")
        } else {
            format!("/solr/content/{path}")
        };
        let mut request = Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let response = self
            .app
            .clone()
            .oneshot(
                request
                    .body(body.map_or_else(Body::empty, |body| Body::from(body.to_owned())))
                    .expect("build probe request"),
            )
            .await
            .expect("coverage probe transport");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read coverage probe response")
            .to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "coverage probe returned non-JSON response: {e}; body={}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, body)
    }

    async fn ok(&self, path: &str) -> bool {
        self.request(Method::GET, path, None).await.0 == StatusCode::OK
    }

    async fn response(&self, path: &str) -> Option<Value> {
        let (status, body) = self.request(Method::GET, path, None).await;
        (status == StatusCode::OK).then_some(body)
    }

    async fn has(&self, path: &str, pointer: &str) -> bool {
        self.response(path)
            .await
            .is_some_and(|body| body.pointer(pointer).is_some())
    }

    async fn number(&self, path: &str, pointer: &str) -> Option<u64> {
        self.response(path)
            .await
            .and_then(|body| body.pointer(pointer).and_then(Value::as_u64))
    }

    async fn response_ids(&self, path: &str) -> Option<Vec<String>> {
        self.response(path).await.and_then(|body| {
            body.pointer("/response/docs")?
                .as_array()?
                .iter()
                .map(|doc| doc.get("id")?.as_str().map(str::to_owned))
                .collect()
        })
    }

    async fn response_docs(&self, path: &str) -> Option<Vec<Value>> {
        self.response(path)
            .await
            .and_then(|body| body.pointer("/response/docs")?.as_array().cloned())
    }

    async fn wait_for_num_found(&self, path: &str, expected: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if self.number(path, "/response/numFound").await == Some(expected) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Does `wildcard_path` (which must send `fl=*,score`) render *every* stored
/// field plus a numeric `score`, at `pointer`?
///
/// Issue #188: asserting only that `/response/docs/0/score` exists is a
/// false-positive green, because `score` is the one member of `fl=*,score` that
/// a literal-name `fl` allowlist already understood -- `*` matched no field, so
/// every real field was dropped and the item still read covered. The
/// discriminating question is what `*` expanded to, and the reference answer for
/// that is the *same request with no `fl` at all*: `fl=*` is exactly the
/// `fl`-absent field set (`solr-ref/responses/select_all.json`), so
/// `baseline_path` supplies the expectation instead of a hardcoded field list
/// that would drift from `PROBE_SCHEMA`.
///
/// Compared in order, not as sets, because ordered equality is free here -- but
/// be clear about what that buys: it pins only that the *two responses agree
/// with each other*, since both sides come from the implementation under test.
/// It is not an ordering oracle. A symmetric fault that permuted doc keys the
/// same way on both requests (e.g. reversing the declared-field iteration order
/// in `CoreIndex::render_doc`) leaves this predicate reading covered and the
/// coverage fraction unmoved. Solr's actual key order is pinned by the
/// fixture-derived suites instead --
/// `tests/json_key_order.rs::select_fl_reversed_fixture_discriminates_input_order_from_fl_order`
/// (`fl` order is not doc key order), `tests/select_fl_wildcard.rs`'s
/// `select_fl_star_alone_keeps_solrs_doc_key_order` /
/// `select_fl_star_plus_score_puts_score_last` /
/// `select_fl_star_plus_score_puts_score_after_dynamic_fields`, and
/// `tests/search_api_preset.rs::preset_fl_star_plus_score_puts_score_last_after_every_dynamic_field`
/// -- all of which compare against captured Solr rather than against another
/// Wayfinder response. `score` is excluded from the comparison and checked
/// separately, since the baseline request cannot carry it (`fl=score` is what
/// turns scoring output on at all).
async fn renders_every_stored_field_plus_score(
    probe: &ProbeApp,
    wildcard_path: &str,
    baseline_path: &str,
    pointer: &str,
) -> bool {
    let Some(baseline) = probe.response(baseline_path).await else {
        return false;
    };
    let Some(expected) = baseline.pointer(pointer).and_then(Value::as_object) else {
        return false;
    };
    // Vacuity guard: two empty docs would compare equal, so an implementation
    // that dropped every field on both requests must not read covered.
    if expected.is_empty() {
        return false;
    }
    let Some(wildcard) = probe.response(wildcard_path).await else {
        return false;
    };
    let Some(actual) = wildcard.pointer(pointer).and_then(Value::as_object) else {
        return false;
    };
    if !actual.get("score").is_some_and(Value::is_number) {
        return false;
    }
    let actual_fields: Vec<(&String, &Value)> =
        actual.iter().filter(|(name, _)| *name != "score").collect();
    actual_fields == expected.iter().collect::<Vec<_>>()
}

/// Each probe is a request against the real strict router with an assertion on
/// its real JSON response. A handler, allowlist, or renderer regression changes
/// this result without a coverage-only Boolean to update.
async fn semantic_covered(probe: &ProbeApp, id: &str) -> bool {
    match id {
        "update.json-command-add-batch" => {
            let duplicate = r#"{"add":{"doc":{"id":"first","body":"first"}},"add":{"doc":{"id":"second","body":"second"}},"commit":{}}"#;
            let (status, _) = probe
                .request(Method::POST, "content/update?commit=true", Some(duplicate))
                .await;
            status == StatusCode::OK
                && probe
                    .number("select?q=id:first", "/response/numFound")
                    .await
                    == Some(1)
                && probe
                    .number("select?q=id:second", "/response/numFound")
                    .await
                    == Some(1)
        }
        "update.commitWithin" => {
            let (status, _) = probe
                .request(
                    Method::POST,
                    "content/update?commitWithin=1000",
                    Some(r#"[{"id":"coverage-commitwithin","body":"delayed visibility"}]"#),
                )
                .await;
            status == StatusCode::OK
                && probe
                    .number("select?q=id:coverage-commitwithin", "/response/numFound")
                    .await
                    == Some(0)
                && probe
                    .wait_for_num_found("select?q=id:coverage-commitwithin", 1)
                    .await
        }
        "request.omitHeader" => {
            let (status, body) = probe
                .request(Method::GET, "select?q=*:*&omitHeader=true", None)
                .await;
            status == StatusCode::OK && body.get("responseHeader").is_none()
        }
        "request.wt.json" => probe.has("select?q=*:*&wt=json", "/response").await,
        "request.json-nl.flat" => {
            let update = probe
                .request(Method::POST, "content/update?json.nl=flat", Some("[]"))
                .await
                .0
                == StatusCode::OK;
            let select = probe
                .has(
                    "select?q=*:*&facet=true&facet.field=category&json.nl=flat",
                    "/facet_counts/facet_fields/category",
                )
                .await;
            let mlt = probe.ok("mlt?q=id:doc1&mlt.fl=body&json.nl=flat").await;
            let admin_info = probe.ok("/solr/admin/info/system?json.nl=flat").await;
            let core_admin = probe.ok("content/admin/system?json.nl=flat").await;
            update && select && mlt && admin_info && core_admin
        }
        // Two legs, because neither alone carries the item.
        //
        // The mbeans leg keeps fidelity to the trace that motivates the
        // contract entry (`solr-ref/search-api/trace/00025.json`, which sends
        // `json.nl=map&json.nl=flat` in that order), but it cannot evidence
        // *which* value wins: `admin_mbeans` renders `solr-mbeans` as an
        // object unconditionally and has no `flat` named-list variant to
        // differ from, so it reads the same whether Wayfinder honours
        // first-value-wins, ignores `json.nl`, or mishandles the repetition.
        // What it does buy is that the item can no longer read as covered
        // purely because a route exists (which is how it flipped when #158
        // landed `GET /solr/{core}/admin/mbeans`). It stays
        // captured-equivalent rather than byte-identical by omitting
        // `stats=true`, which only controls a `stats` sub-object nested
        // *inside* `solr-mbeans`, not its presence or shape -- the sibling
        // `admin.mbeans.solr-mbeans` probe omits it for the same reason.
        //
        // The `/select` facet leg is what discriminates.
        // `JsonNl::from_params` (`src/facet.rs`) reads `json.nl` and
        // `render_buckets` emits an object for `map` and an alternating array
        // for `flat`, so with `map` sent first this pointer is an object under
        // first-value-wins and an array under last-value-wins, under "ignore
        // `json.nl`", and under any handling that drops the repeated key.
        //
        // Applying first-value-wins to `/select` is an inference from trace
        // `00025.json` plus `Params::get` (`src/params.rs`), which returns the
        // first value for a repeated key -- not from a `/select` capture of a
        // repeated `json.nl`, because no such capture exists. The inference
        // holds because the resolution is request-parsing behaviour that runs
        // before any endpoint-specific code, so it cannot vary by endpoint;
        // the same reasoning already backs `src/lib.rs`. Capturing repeated
        // `json.nl` against real Solr belongs to #153.
        //
        // ponytail: two mutants survive this probe, both by construction
        // rather than for want of a test.
        //
        // 1. Cutting the second `json.nl` value from the `/select` leg
        //    (`json.nl=map&json.nl=flat` -> `json.nl=map`). First-value-wins
        //    makes `f(map, flat)` and `f(map)` produce byte-identical
        //    responses, so no response-shape probe can tell a correctly
        //    resolved *repeated* `json.nl` from a request that only ever
        //    sent the winner.
        // 2. Cutting the query string off the mbeans leg. `admin_mbeans`
        //    reads only `stats`/`cat`/`key`; `json.nl` is merely allowlisted
        //    there, so its presence changes no byte of the response.
        //
        // Everything else is pinned: the value that wins, the shape it
        // produces, that `json.nl` is sent at all on the discriminating leg,
        // and both legs' presence in the conjunction.
        "request.json-nl.repeated-map-and-flat" => {
            let mbeans = probe
                .response("content/admin/mbeans?json.nl=map&json.nl=flat")
                .await
                .is_some_and(|body| body.get("solr-mbeans").is_some_and(Value::is_object));
            let facet = probe
                .response("select?q=*:*&facet=true&facet.field=category&json.nl=map&json.nl=flat")
                .await
                .is_some_and(|body| {
                    body.pointer("/facet_counts/facet_fields/category")
                        .is_some_and(Value::is_object)
                });
            mbeans && facet
        }
        "request.timezone.utc" => {
            probe.ok("select?q=*:*&TZ=UTC").await
                && probe.ok("mlt?q=id:doc1&mlt.fl=body&TZ=UTC").await
        }
        // Deliberately not the contract's literal captured value. The
        // `select.q.plain-query` entry records Search API Solr's *internal*
        // expanded Lucene syntax (`tm_X3b_en_body:(+"quick")^1 ...`) -- the
        // per-language field fan-out that module builds against a Solr-side
        // dynamic-field naming scheme Wayfinder does not host. No real client
        // ever sends that string as an opaque `q=`; it is query construction
        // detail, not wire semantics. What the entry actually asserts about
        // Wayfinder is "a plain single-term `q` finds the docs containing that
        // term", so numFound on `q=quick` is the behavioral stand-in, not a
        // corpus or probe shortcut.
        "select.q.plain-query" => {
            probe.number("select?q=quick", "/response/numFound").await == Some(2)
        }
        // Fixture-derived, per issue #147. `q={!edismax}quick+rocket` is one
        // unquoted clause (`+` is an ordinary term character mid-token in
        // Lucene's `_TERM_CHAR` set) analysing to two tokens, so the expected
        // count turns entirely on whether edismax builds a phrase or a boolean
        // OR for it. `solr-ref/responses/edismax_unquoted_multitoken.json`
        // (manifest row `edismax_unquoted_multitoken`, real `solr:9`) answers
        // that with `numFound=6` over the capture corpus's ten docs -- every
        // doc carrying *either* token, and zero of them carrying the two
        // adjacent -- so it is the OR reading, and the phrase reading is ruled
        // out on that corpus rather than merely thought unlikely. The
        // `_TERM_CHAR` step is captured too:
        // `solr-ref/responses/edismax_unquoted_multitoken_debug.json`, the same
        // request with `debugQuery=true`, parses to a *single*
        // `DisjunctionMaxQuery(((title:quick title:rocket) | (body:quick
        // body:rocket)))` -- one clause spanning both tokens (two clauses would
        // give two disjunctions), each field's pair a SHOULD, not a
        // `PhraseQuery`. That capture has no manifest row: `debugQuery` output
        // is not stable enough for the differential harness to GET verbatim, so
        // `solr-ref/capture.sh` carries its curl command as a comment instead.
        // Applied to `PROBE_DOCS`, where "quick" and "rocket" both occur only in
        // doc1 and doc2, OR gives 2 (the phrase reading would give 0, since
        // neither doc has them adjacent). What this probe therefore cannot catch:
        // doc1 and doc2 each carry **both** terms, so an AND reading also gives
        // 2. `PROBE_DOCS` is a phrase-vs-not regression net only, never evidence
        // about OR-vs-AND; only the capture corpus separates those (its six
        // matches include docs carrying just one term, so AND would be 0). Both
        // readings agreeing here is why the count alone is not the provenance --
        // the two fixtures above are, and `tests/edismax_capture_provenance.rs`
        // requires this comment to keep naming them. This value was previously
        // the speculative `Some(2)` written in `bb44cc4` (#105) for an entry
        // that could not pass then; it now traces to the captures, which is
        // what CLAUDE.md requires.
        "select.q.local-params-edismax.and" => {
            probe
                .number(
                    "select?q=%7B!edismax%7Dquick%2Brocket",
                    "/response/numFound",
                )
                .await
                == Some(2)
        }
        "select.q.local-params-edismax.or" => {
            probe
                .number(
                    "select?q=%7B!edismax%7Dquick%20rocket",
                    "/response/numFound",
                )
                .await
                == Some(2)
        }
        "select.q.local-params-edismax.single-term" => {
            probe
                .number("select?q=%7B!edismax%7Dquick", "/response/numFound")
                .await
                == Some(2)
        }
        "select.q.match-all" => probe
            .number("select?q=*:*&rows=100", "/response/numFound")
            .await
            .is_some_and(|count| count >= 24),
        "select.pagination.start-and-rows" => {
            let first_page = probe.response_ids("select?q=*:*&start=0&rows=10").await;
            let second_page = probe.response_ids("select?q=*:*&start=1&rows=1").await;
            probe
                .number("select?q=*:*&start=0&rows=10", "/response/numFound")
                .await
                .is_some_and(|count| count >= 24)
                && first_page.as_ref().is_some_and(|ids| ids.len() == 10)
                && second_page.as_ref().is_some_and(|ids| ids.len() == 1)
                && first_page
                    .zip(second_page)
                    .is_some_and(|(first, second)| first[1] == second[0])
        }
        "select.rows.zero" => {
            probe
                .number("select?q=*:*&rows=0", "/response/numFound")
                .await
                .is_some_and(|count| count >= 24)
                && probe.response_ids("select?q=*:*&rows=0").await == Some(Vec::new())
        }
        "select.fl.wildcard-plus-score" => {
            renders_every_stored_field_plus_score(
                probe,
                "select?q=quick&fl=*,score",
                "select?q=quick",
                "/response/docs/0",
            )
            .await
        }
        "select.fq.string" => {
            probe
                .number("select?q=*:*&fq=category:animals", "/response/numFound")
                .await
                == Some(1)
        }
        "select.fq.range" => {
            probe
                .number(
                    "select?q=body:quick&fq=rating:%5B3%20TO%20*%5D",
                    "/response/numFound",
                )
                .await
                == Some(1)
        }
        "select.fq.boolean" => {
            probe
                .number("select?q=*:*&fq=featured:true", "/response/numFound")
                .await
                == Some(2)
        }
        "select.fq.multi-value-or" => {
            probe
                .number(
                    "select?q=*:*&fq=(category:animals%20category:garden)",
                    "/response/numFound",
                )
                .await
                == Some(2)
        }
        "select.sort.integer" => {
            probe
                .response_ids("select?q=body:quick&sort=rating%20desc")
                .await
                == Some(vec!["doc1".to_string(), "doc2".to_string()])
        }
        "select.sort.string" => {
            probe
                .response_ids("select?q=body:quick&sort=category%20asc")
                .await
                == Some(vec!["doc1".to_string(), "doc2".to_string()])
        }
        "select.sort.date" => {
            probe
                .response_ids("select?q=body:quick&sort=created%20asc")
                .await
                == Some(vec!["doc2".to_string(), "doc1".to_string()])
        }
        "select.highlight.enabled" => {
            probe
                .has("select?q=quick&hl=true&hl.fl=body", "/highlighting/doc1")
                .await
        }
        "select.highlight.wildcard-fields" => {
            probe
                .has("select?q=quick&hl=true&hl.fl=*", "/highlighting/doc1/body")
                .await
        }
        "select.highlight.require-field-match" => {
            probe.ok("select?q=quick&hl.requireFieldMatch=false").await
        }
        // Every captured exchange sends `hl.snippets=3` (contract entry
        // `select.highlight.snippets`, variant "three"), so that is what this
        // probe asks for, against the `hl-snippets-gizmo` doc whose `body`
        // holds three well-separated "gizmo" hits. Do not swap this back to
        // `hl.snippets=1`, which any implementation passes whether or not the
        // cap means anything: only a doc with more than one possible fragment
        // discriminates a real cap from a single-fragment ceiling (issue #103,
        // `CoreIndex::highlight_field`'s mask-and-resnippet loop).
        "select.highlight.snippets" => {
            probe
                .response("select?q=gizmo&hl=true&hl.fl=body&hl.snippets=3")
                .await
                .and_then(|body| {
                    body.pointer("/highlighting/hl-snippets-gizmo/body")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                })
                == Some(3)
        }
        // Two requests, because the captured shape and the discriminating
        // shape are not the same request. `hl.fragsize=0` with no `hl.method`
        // is what the captured traffic sends (contract variant
        // "zero-whole-field"), and fixtures `hl_fragsize_zero_whole_field.json`
        // / `hl_fragsize_zero_whole_field_method_original.json` (issue #104)
        // now pin exactly what real Solr returns for it: the entire field,
        // unfragmented, as a single snippet -- for both the default
        // `hl.method` and `hl.method=original`. So that half asserts the
        // returned snippet equals `HL_FRAGSIZE_PROBE_DOCS`'s whole seeded
        // body (with the match wrapped in `<em>`), not just that some
        // snippet came back. Truncation is only observable under
        // `hl.method=original` with a nonzero budget (finding 54,
        // `src/highlight.rs` module docs; fixture
        // `hl_fragsize_truncated.json`), so the second half asks for a
        // 10-char budget over `doc1`'s "quick brown fox rocket" and requires
        // the snippet to actually come back shorter than the untruncated
        // field -- otherwise an implementation that dropped `hl.fragsize` on
        // the floor entirely would still score this covered.
        "select.highlight.fragsize" => {
            let expected_whole_field = concat!(
                "<em>wexford</em> prototype notes from the engineering standup this morning. ",
                "the team reviewed the roadmap for the next quarter and discussed several ",
                "open risks around supply chain timing. afterwards everyone broke for lunch ",
                "and reconvened at two in the afternoon to continue the planning session for ",
                "the rest of the week."
            );
            let whole_field = probe
                .response("select?q=wexford&hl=true&hl.fl=body&hl.fragsize=0")
                .await
                .and_then(|body| {
                    body.pointer("/highlighting/hl-fragsize-long/body/0")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|snippet| snippet == expected_whole_field);
            let truncated = probe
                .response("select?q=quick&hl=true&hl.fl=body&hl.method=original&hl.fragsize=10")
                .await
                .and_then(|body| {
                    body.pointer("/highlighting/doc1/body/0")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|snippet| {
                    snippet.contains("<em>quick</em>") && !snippet.contains("rocket")
                });
            whole_field && truncated
        }
        "select.highlight.merge-contiguous" => {
            probe.ok("select?q=quick&hl.mergeContiguous=false").await
        }
        "select.highlight.custom-markers" => {
            let (status, body) = probe
                .request(
                    Method::GET,
                    "select?q=quick&hl=true&hl.fl=body&hl.simple.pre=%5BOPEN%5D&hl.simple.post=%5B%2FCLOSE%5D",
                    None,
                )
                .await;
            status == StatusCode::OK
                && body
                    .pointer("/highlighting/doc1/body/0")
                    .and_then(Value::as_str)
                    .is_some_and(|snippet| snippet.contains("[OPEN]"))
        }
        "select.facet.field" => {
            probe
                .has(
                    "select?q=*:*&facet=true&facet.field=category",
                    "/facet_counts/facet_fields/category",
                )
                .await
        }
        "select.facet.local-key" => {
            probe
                .has(
                    "select?q=*:*&facet=true&facet.field=%7B!key=kind%7Dcategory",
                    "/facet_counts/facet_fields/kind",
                )
                .await
        }
        "select.facet.per-field-missing" => {
            // Presence of the `facet_fields` container proves nothing here --
            // it is there without the override too (issue #162's tightening).
            // The observable effect of `f.category.facet.missing=true` is the
            // trailing `null` key in the flat counts array, so require that.
            probe
                .response(
                    "select?q=*:*&facet=true&facet.field=category&f.category.facet.missing=true",
                )
                .await
                .and_then(|body| {
                    body.pointer("/facet_counts/facet_fields/category")?
                        .as_array()
                        .map(|counts| counts.iter().any(Value::is_null))
                })
                .unwrap_or(false)
        }
        "select.facet.sort-limit-mincount" => {
            let sorted_limited = probe
                .response(
                    "select?q=*:*&facet=true&facet.field=category&facet.sort=count&facet.limit=3&facet.mincount=1",
                )
                .await
                .and_then(|body| body.pointer("/facet_counts/facet_fields/category").cloned());
            let mincount_filtered = probe
                .response(
                    "select?q=*:*&facet=true&facet.field=category&facet.sort=count&facet.limit=5&facet.mincount=4",
                )
                .await
                .and_then(|body| body.pointer("/facet_counts/facet_fields/category").cloned());
            sorted_limited
                == Some(serde_json::json!([
                    "cooking",
                    6,
                    "astronomy",
                    5,
                    "gardening",
                    5
                ]))
                && mincount_filtered
                    == Some(serde_json::json!([
                        "cooking",
                        6,
                        "astronomy",
                        5,
                        "gardening",
                        5
                    ]))
        }
        "select.facet.global-missing" => probe
            .response("select?q=id:facet&facet=true&facet.field=category&facet.missing=false")
            .await
            .and_then(|body| body.pointer("/facet_counts/facet_fields/category").cloned())
            .is_some_and(|buckets| {
                buckets.as_array().is_some_and(|items| {
                    items.len() % 2 == 0 && items.iter().step_by(2).all(|key| !key.is_null())
                })
            }),
        "select.spellcheck.enable" => {
            probe
                .has("select?q=quick&spellcheck=true", "/spellcheck")
                .await
        }
        "select.spellcheck.query" => {
            probe
                .has(
                    "select?q=quick&spellcheck=true&spellcheck.q=qwick",
                    "/spellcheck/suggestions",
                )
                .await
        }
        "select.spellcheck.dictionaries" => {
            probe
                .has(
                    "select?q=quick&spellcheck=true&spellcheck.dictionary=en",
                    "/spellcheck/suggestions",
                )
                .await
        }
        "select.spellcheck.collate" => {
            probe
                .has(
                    "select?q=quick&spellcheck=true&spellcheck.collate=true",
                    "/spellcheck/collations",
                )
                .await
        }
        "mlt.base-lookup" => probe
            .response("mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1")
            .await
            .is_some_and(|body| body.pointer("/response").is_some_and(Value::is_object)),
        "mlt.pagination.start-and-rows" => {
            let first_page = probe
                .response_ids(
                    "mlt?q=id:mlt11&mlt.fl=body&fl=id&mlt.mintf=1&mlt.mindf=1&start=0&rows=10",
                )
                .await;
            let second_page = probe
                .response_ids(
                    "mlt?q=id:mlt11&mlt.fl=body&fl=id&mlt.mintf=1&mlt.mindf=1&start=1&rows=1",
                )
                .await;
            first_page
                .zip(second_page)
                .is_some_and(|(first, second)| first.len() > 1 && second == vec![first[1].clone()])
        }
        // `mlt.mintf=1&mlt.mindf=1` are load-bearing, not decoration: at Solr's
        // defaults (mintf=2/mindf=5) this 20-doc corpus has no similar docs at
        // all (finding 64, `docs/solr-ref-findings.md`), so `/response/docs/0`
        // would not exist whatever `fl` did -- which is why this item read
        // uncovered before issue #188 for a reason unrelated to the wildcard.
        // The sibling `mlt.mintf`/`mlt.mindf` probes pin the same thresholds.
        "mlt.fl.wildcard-plus-score" => {
            renders_every_stored_field_plus_score(
                probe,
                "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=*,score",
                "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1",
                "/response/docs/0",
            )
            .await
        }
        // Three legs, because a 200 alone proves only that `fq` is
        // allowlisted, not that it filters (issue #141, finding 98). The
        // unfiltered leg establishes a non-empty similar-docs set; the
        // filtered leg (`category:animals` matches only `doc1`, which shares
        // no vocabulary with the astronomy cluster) must empty it; and the
        // seed doc must survive the same filter, since `fq` scopes
        // `response` only and never `match`.
        "mlt.filters" => {
            let unfiltered = probe
                .number(
                    "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1",
                    "/response/numFound",
                )
                .await;
            let filtered = probe
                .response("mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:animals")
                .await;
            unfiltered.is_some_and(|count| count > 0)
                && filtered.is_some_and(|body| {
                    body.pointer("/response/numFound") == Some(&Value::from(0))
                        && body.pointer("/match/docs/0/id")
                            == Some(&Value::String("mlt11".to_string()))
                })
        }
        "mlt.mintf" => {
            let loose = probe
                .number(
                    "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1",
                    "/response/numFound",
                )
                .await;
            let strict = probe
                .number(
                    "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=2&mlt.mindf=1",
                    "/response/numFound",
                )
                .await;
            loose.is_some_and(|count| count > 0) && strict == Some(0)
        }
        "mlt.mindf" => {
            let loose = probe
                .number(
                    "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1",
                    "/response/numFound",
                )
                .await;
            let strict = probe
                .number(
                    "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=5",
                    "/response/numFound",
                )
                .await;
            loose.is_some_and(|count| count > 0) && strict == Some(0)
        }
        "mlt.maxqt" => {
            let uncapped = probe
                .number(
                    "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxqt=100",
                    "/response/numFound",
                )
                .await;
            let capped = probe
                .number(
                    "mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxqt=2",
                    "/response/numFound",
                )
                .await;
            uncapped.is_some_and(|count| count > 0) && capped == Some(0)
        }
        "mlt.maxntp" => probe.ok("mlt?q=id:mlt11&mlt.fl=body&mlt.maxntp=2000").await,
        "mlt.boost" => {
            let unboosted = probe
                .response_docs(
                    "mlt?q=id:mlt1&mlt.fl=body&fl=id,score&mlt.mintf=1&mlt.mindf=1&mlt.boost=false",
                )
                .await;
            let boosted = probe
                .response_docs(
                    "mlt?q=id:mlt1&mlt.fl=body&fl=id,score&mlt.mintf=1&mlt.mindf=1&mlt.boost=true",
                )
                .await;
            unboosted
                .zip(boosted)
                .is_some_and(|(plain, weighted)| !plain.is_empty() && plain != weighted)
        }
        // Both params, each against the shape it actually changes (issue
        // #141, findings 99/100): `mlt.match.include=false` must drop the
        // `match` key outright where the explicit-`true` request keeps one
        // (sent explicitly, not omitted, so a gate keyed on the param's mere
        // presence rather than its value reads as uncovered here),
        // and `mlt.match.offset=1` must seed from the *second* `q` hit --
        // `q=category:astronomy` resolves `mlt11`..`mlt15`, so offset 0 and
        // offset 1 name different docs and `match.start` echoes the offset.
        // A bare 200 would read the same whether either param were honoured
        // or merely allowlisted.
        "mlt.match-include-and-offset" => {
            let included = probe
                .response("mlt?q=id:mlt11&mlt.fl=body&mlt.match.include=true&mlt.match.offset=0")
                .await
                .is_some_and(|body| {
                    body.pointer("/match/docs/0/id") == Some(&Value::String("mlt11".to_string()))
                        && body.pointer("/match/start") == Some(&Value::from(0))
                });
            let excluded = probe
                .response("mlt?q=id:mlt11&mlt.fl=body&mlt.match.include=false")
                .await
                .is_some_and(|body| body.get("match").is_none() && body.get("response").is_some());
            let offset = probe
                .response("mlt?q=category:astronomy&mlt.fl=body&mlt.match.offset=1")
                .await
                .is_some_and(|body| {
                    body.pointer("/match/docs/0/id") == Some(&Value::String("mlt12".to_string()))
                        && body.pointer("/match/start") == Some(&Value::from(1))
                });
            included && excluded && offset
        }
        "mlt.interesting-terms-none" => {
            let (status, body) = probe
                .request(
                    Method::GET,
                    "mlt?q=id:doc1&mlt.fl=body&mlt.interestingTerms=none",
                    None,
                )
                .await;
            status == StatusCode::OK && body.get("interestingTerms").is_none()
        }
        "admin.mbeans.stats" => probe.ok("content/admin/mbeans?stats=true").await,
        "terms.enumeration" => probe.ok("content/terms?terms=true&terms.fl=body").await,
        _ => panic!("unrecognised Search API semantic denominator item: {id}"),
    }
}

async fn response_field_covered(probe: &ProbeApp, id: &str) -> bool {
    match id {
        "select.response.numFound" => probe
            .number("select?q=*:*", "/response/numFound")
            .await
            .is_some_and(|count| count >= 24),
        "select.response.docs" => probe
            .response_docs("select?q=*:*&rows=10")
            .await
            .is_some_and(|docs| {
                docs.len() == 10
                    && docs
                        .iter()
                        .all(|doc| doc.is_object() && doc.get("id").is_some_and(Value::is_string))
            }),
        "select.response.docs.score" => probe
            .response_docs("select?q=body:quick&fl=id,score")
            .await
            .is_some_and(|docs| {
                docs.len() == 2
                    && docs.iter().all(|doc| {
                        doc.get("id").is_some_and(Value::is_string)
                            && doc.get("score").is_some_and(Value::is_number)
                    })
            }),
        "select.highlighting" => probe
            .response("select?q=quick&hl=true&hl.fl=body")
            .await
            .and_then(|body| body.pointer("/highlighting/doc1/body/0").cloned())
            .is_some_and(|snippet| snippet.as_str().is_some_and(|text| !text.is_empty())),
        "select.facet_counts" => probe
            .response("select?q=id:facet&facet=true&facet.field=category")
            .await
            .and_then(|body| body.get("facet_counts").cloned())
            .is_some_and(|counts| {
                counts.is_object()
                    && counts.get("facet_fields").is_some_and(Value::is_object)
                    && counts.get("facet_queries").is_some_and(Value::is_object)
            }),
        "select.facet_counts.facet_fields" => probe
            .response("select?q=id:facet&facet=true&facet.field=category")
            .await
            .and_then(|body| body.pointer("/facet_counts/facet_fields/category").cloned())
            .is_some_and(|buckets| {
                buckets.as_array().is_some_and(|items| {
                    items.len() >= 2
                        && items.len() % 2 == 0
                        && items.iter().step_by(2).all(Value::is_string)
                        && items.iter().skip(1).step_by(2).all(Value::is_u64)
                })
            }),
        "select.spellcheck.suggestions" => probe
            .response("select?q=quick&spellcheck=true")
            .await
            .and_then(|body| body.pointer("/spellcheck/suggestions").cloned())
            .is_some_and(|value| value.is_object()),
        "select.spellcheck.collations" => probe
            .response("select?q=quick&spellcheck=true")
            .await
            .and_then(|body| body.pointer("/spellcheck/collations").cloned())
            .is_some_and(|value| value.is_array()),
        "mlt.response" => probe
            .response("mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1")
            .await
            .and_then(|body| body.get("response").cloned())
            .is_some_and(|response| {
                response.get("numFound").is_some_and(Value::is_u64)
                    && response.get("docs").is_some_and(Value::is_array)
            }),
        "admin.info-system.lucene.solr-spec-version" => probe
            .response("/solr/admin/info/system")
            .await
            .and_then(|body| body.pointer("/lucene/solr-spec-version").cloned())
            .is_some_and(|version| {
                version
                    .as_str()
                    .is_some_and(|text| text.split('.').count() == 3)
            }),
        "admin.system.core.schema" => probe
            .response("content/admin/system")
            .await
            .and_then(|body| body.pointer("/core/schema").cloned())
            .is_some_and(|value| value.is_string()),
        // The one real consumer is `isPartOfSchema('fieldTypes', ...)`, an
        // `in_array()` over the entries' `name`s -- so an empty array, or
        // entries without a usable name, is nothing a client can act on.
        "schema.fieldtypes.fieldTypes" => probe
            .response("content/schema/fieldtypes")
            .await
            .and_then(|body| body.get("fieldTypes").cloned())
            .is_some_and(|value| {
                value.as_array().is_some_and(|entries| {
                    !entries.is_empty()
                        && entries.iter().all(|entry| {
                            entry
                                .get("name")
                                .and_then(Value::as_str)
                                .is_some_and(|name| !name.is_empty())
                        })
                })
            }),
        // `SearchApiSolrBackend::viewSettings` reads `index.numDocs` and
        // nothing else out of the Luke response (`getLuke()` itself is
        // `SolrConnectorPluginBase::getLuke()`, which only fetches), so that
        // leaf -- not the container -- is what coverage means.
        "admin.luke.index" => probe
            .response("content/admin/luke")
            .await
            .and_then(|body| body.pointer("/index/numDocs").cloned())
            .is_some_and(|value| value.is_u64()),
        "admin.mbeans.solr-mbeans" => probe
            .response("content/admin/mbeans")
            .await
            .and_then(|body| body.get("solr-mbeans").cloned())
            .is_some_and(|value| value.is_object()),
        // A client reads term/frequency pairs out of `terms.<field>`, so the
        // probe has to ask for a real field -- `terms=true` with no `terms.fl`
        // is documented (see `terms` in `src/lib.rs`) to return the hollow
        // `{"terms":{}}`, which proves nothing. Same field as the sibling
        // `terms.enumeration` request-semantic probe -- the two now issue
        // byte-identical requests and differ only in what they assert about
        // the response.
        "terms.terms" => probe
            .response("content/terms?terms=true&terms.fl=body")
            .await
            .and_then(|body| body.get("terms").cloned())
            .is_some_and(|value| {
                value.as_object().is_some_and(|fields| {
                    fields.values().any(|entries| {
                        entries.as_array().is_some_and(|pairs| {
                            !pairs.is_empty()
                                && pairs.len() % 2 == 0
                                && pairs
                                    .iter()
                                    .step_by(2)
                                    .all(|term| term.as_str().is_some_and(|text| !text.is_empty()))
                                && pairs.iter().skip(1).step_by(2).all(Value::is_u64)
                        })
                    })
                })
            }),
        _ => panic!("unrecognised Search API response denominator item: {id}"),
    }
}

fn bucket(items: Vec<ReportedItem>) -> Bucket {
    let covered = items.iter().filter(|item| item.covered).count();
    let total = items.len();
    let mut uncovered_items = items
        .iter()
        .filter(|item| !item.covered)
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    uncovered_items.sort();
    // Contract ordering remains the published deterministic item order; only
    // the convenience uncovered list is lexical for stable backlog diffs.
    Bucket {
        items,
        covered,
        uncovered: total - covered,
        total,
        fraction: format!("{covered}/{total}"),
        uncovered_items,
    }
}

/// Produces the stable JSON value used by `wayfinder coverage --format json`.
pub async fn report() -> serde_json::Value {
    let contract = contract();
    let probe = ProbeApp::new().await;
    let endpoints = bucket(
        contract
            .endpoints
            .iter()
            .map(|item| ReportedItem {
                id: item.id.clone(),
                covered: endpoint_covered(&item.id),
                trace: item.trace.clone(),
                parameters: Vec::new(),
                consumer: None,
                evidence: vec![Evidence {
                    kind: "route",
                    source: "src/lib.rs::search_api_routes!".to_string(),
                }],
            })
            .collect(),
    );
    let mut semantic_items = Vec::with_capacity(contract.request_semantics.len());
    for item in &contract.request_semantics {
        semantic_items.push(ReportedItem {
            id: item.id.clone(),
            covered: semantic_covered(&probe, &item.id).await,
            trace: item.trace.clone(),
            parameters: item.parameters.clone(),
            consumer: None,
            evidence: vec![Evidence {
                kind: "runtime-probe",
                source: "strict routed handler plus rendered JSON".to_string(),
            }],
        });
    }
    let request_semantics = bucket(semantic_items);
    let mut response_items = Vec::with_capacity(contract.response_fields.len());
    for item in &contract.response_fields {
        response_items.push(ReportedItem {
            id: item.id.clone(),
            covered: response_field_covered(&probe, &item.id).await,
            trace: item.trace.clone(),
            parameters: Vec::new(),
            consumer: Some(item.consumer.clone()),
            evidence: vec![Evidence {
                kind: "runtime-probe",
                source: "strict routed handler plus rendered JSON".to_string(),
            }],
        });
    }
    let response_fields = bucket(response_items);
    let covered = endpoints.covered + request_semantics.covered + response_fields.covered;
    let total = endpoints.total + request_semantics.total + response_fields.total;
    serde_json::to_value(Report {
        traces: contract.traces,
        endpoints,
        request_semantics,
        response_fields,
        overall: Totals {
            covered,
            uncovered: total - covered,
            total,
            fraction: format!("{covered}/{total}"),
        },
    })
    .expect("coverage report is serializable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::RawQuery;
    use axum::routing::get;

    #[test]
    fn contract_rejects_manual_coverage_classifications() {
        let mut contract: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
        contract["request_semantics"][0]["covered"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Contract>(contract).is_err());
    }

    // Issue #162: `admin.luke.index`, `terms.terms`, and
    // `schema.fieldtypes.fieldTypes` each check only that a container exists
    // (`is_object()`/`is_array()`), so an empty container counts as covered.
    // None of the three real handlers can be coaxed into emitting a genuinely
    // hollow container through `ProbeApp`'s real router -- `admin/luke`
    // always populates `index.numDocs` (0 is still a present u64),
    // `schema/fieldtypes` always lists Wayfinder's built-in types, and
    // `terms.terms`'s own probe request already exercises real term/count
    // data. So this builds a second, throwaway `ProbeApp` around a stub
    // router that serves exactly the hollow shape described in the issue at
    // the same paths `response_field_covered`'s match arms request, and
    // drives the real (private) predicate function against it -- the same
    // function `report()` calls, not a reimplementation of its logic.
    async fn hollow_index_container() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"index": {}}))
    }

    async fn hollow_terms_container() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"terms": {}}))
    }

    async fn hollow_terms_field_with_no_terms() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"terms": {"body": []}}))
    }

    async fn hollow_fieldtypes_container() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"fieldTypes": []}))
    }

    fn hollow_probe() -> ProbeApp {
        let app = Router::new()
            .route("/solr/content/admin/luke", get(hollow_index_container))
            .route("/solr/content/terms", get(hollow_terms_container))
            .route(
                "/solr/content/schema/fieldtypes",
                get(hollow_fieldtypes_container),
            );
        ProbeApp {
            app,
            _workspace: ProbeWorkspace::new(),
        }
    }

    // A hollow container is not the only way a leaf can be present but
    // useless: the same client code breaks if the leaf is there with the
    // wrong JSON type. `numDocs` as a string, term frequencies as strings,
    // and a field type whose `name` is the empty string are all shapes a
    // container-existence (or mere `is_some()`) check would wave through.
    async fn wrong_type_index_num_docs() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"index": {"numDocs": "12"}}))
    }

    async fn wrong_type_term_counts() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"terms": {"body": ["quick", "2"]}}))
    }

    async fn wrong_type_fieldtype_name() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"fieldTypes": [{"name": ""}]}))
    }

    fn wrong_type_probe() -> ProbeApp {
        let app = Router::new()
            .route("/solr/content/admin/luke", get(wrong_type_index_num_docs))
            .route("/solr/content/terms", get(wrong_type_term_counts))
            .route(
                "/solr/content/schema/fieldtypes",
                get(wrong_type_fieldtype_name),
            );
        ProbeApp {
            app,
            _workspace: ProbeWorkspace::new(),
        }
    }

    fn hollow_terms_with_empty_field_probe() -> ProbeApp {
        let app = Router::new().route("/solr/content/terms", get(hollow_terms_field_with_no_terms));
        ProbeApp {
            app,
            _workspace: ProbeWorkspace::new(),
        }
    }

    #[tokio::test]
    async fn admin_luke_index_probe_rejects_a_hollow_index_container() {
        let probe = hollow_probe();
        assert!(
            !response_field_covered(&probe, "admin.luke.index").await,
            "admin.luke.index must require the real leaf its client consumer reads \
             (index.numDocs as a u64), not merely that `index` is an object -- \
             an empty `{{}}` must not count as covered"
        );
    }

    #[tokio::test]
    async fn terms_terms_probe_rejects_a_hollow_terms_container() {
        let probe = hollow_probe();
        assert!(
            !response_field_covered(&probe, "terms.terms").await,
            "terms.terms must require at least one non-empty term/frequency pair, \
             not merely that `terms` is an object -- an empty `{{}}` must not count \
             as covered"
        );
    }

    #[tokio::test]
    async fn terms_terms_probe_rejects_a_field_with_no_terms() {
        let probe = hollow_terms_with_empty_field_probe();
        assert!(
            !response_field_covered(&probe, "terms.terms").await,
            "terms.terms must require a non-empty term/frequency pair -- a `terms` \
             object whose only field key maps to an empty array (the shape real \
             `terms=true` with no matching terms produces) must not count as covered"
        );
    }

    #[tokio::test]
    async fn schema_fieldtypes_fieldtypes_probe_rejects_a_hollow_array() {
        let probe = hollow_probe();
        assert!(
            !response_field_covered(&probe, "schema.fieldtypes.fieldTypes").await,
            "schema.fieldtypes.fieldTypes must require a non-empty name list, not \
             merely that `fieldTypes` is an array -- an empty `[]` must not count \
             as covered"
        );
    }

    #[tokio::test]
    async fn admin_luke_index_probe_rejects_a_string_num_docs() {
        let probe = wrong_type_probe();
        assert!(
            !response_field_covered(&probe, "admin.luke.index").await,
            "admin.luke.index must require `index.numDocs` to be a JSON number -- \
             a present-but-stringly `\"12\"` is a type regression its consumer \
             cannot use, so a mere presence check must not count it as covered"
        );
    }

    #[tokio::test]
    async fn terms_terms_probe_rejects_string_frequencies() {
        let probe = wrong_type_probe();
        assert!(
            !response_field_covered(&probe, "terms.terms").await,
            "terms.terms must require the frequency half of each term/frequency \
             pair to be a JSON number -- `[\"quick\", \"2\"]` is a type regression \
             that must not count as covered"
        );
    }

    #[tokio::test]
    async fn schema_fieldtypes_fieldtypes_probe_rejects_an_empty_name() {
        let probe = wrong_type_probe();
        assert!(
            !response_field_covered(&probe, "schema.fieldtypes.fieldTypes").await,
            "schema.fieldtypes.fieldTypes must require each entry's `name` to be a \
             non-empty string -- an entry naming itself `\"\"` is nothing \
             `isPartOfSchema()` can match, so it must not count as covered"
        );
    }

    // Issue #167: `request.json-nl.repeated-map-and-flat`'s probe
    // (`semantic_covered`, src/coverage.rs, the
    // `"request.json-nl.repeated-map-and-flat" => probe.ok(...)` arm) only
    // asserts an HTTP 200 on
    // `content/admin/mbeans?json.nl=flat&json.nl=map`. It flipped to covered
    // as a side effect of #158 routing `GET /solr/{core}/admin/mbeans` at
    // all, with nothing verifying `solr-mbeans` is even present, let alone
    // shaped as an object -- the shape the trace (`00025.json`) settled on.
    // Same class of bug as #162 (an existence/200 check standing in for a
    // shape check), same stub-router fix pattern: the real handler cannot be
    // coaxed into emitting a non-object `solr-mbeans` (`admin_mbeans` in
    // `src/lib.rs` builds it unconditionally, regardless of `json.nl` or
    // `stats`), so this drives the real (private) `semantic_covered`
    // function against a throwaway stub router serving the wrong shape at
    // the same path `content/admin/mbeans` resolves to
    // (`/solr/content/admin/mbeans`).
    //
    // The probe now also has a `/select` facet leg, which is the half that
    // actually discriminates first-value-wins. Its failing shapes are
    // stubbed the same way, at `/solr/content/select`: the real app cannot
    // serve them without `json.nl` handling itself regressing, which is what
    // `repeated_map_and_flat_stays_covered_against_the_real_seeded_app`
    // (`tests/search_api_coverage.rs`) guards instead.
    async fn mbeans_response_missing_solr_mbeans() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"responseHeader": {"status": 0, "QTime": 0}}))
    }

    async fn mbeans_response_solr_mbeans_array() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"solr-mbeans": []}))
    }

    async fn mbeans_response_solr_mbeans_null() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"solr-mbeans": null}))
    }

    async fn mbeans_response_solr_mbeans_object() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"solr-mbeans": {"CORE": {}, "UPDATE": {}}}))
    }

    // The probe is a conjunction of an mbeans leg and a `/select` facet leg,
    // so a stub router serving only one path leaves the other leg 404ing and
    // every stub assertion passes vacuously. Each stub therefore serves both
    // paths, holding one leg at the shape the probe wants while the other
    // carries the defect under test.
    async fn select_facet_category_object() -> axum::Json<Value> {
        axum::Json(
            serde_json::json!({"facet_counts": {"facet_fields": {"category": {"animals": 1}}}}),
        )
    }

    async fn select_facet_category_flat_array() -> axum::Json<Value> {
        axum::Json(
            serde_json::json!({"facet_counts": {"facet_fields": {"category": ["animals", 1]}}}),
        )
    }

    async fn select_facet_category_missing() -> axum::Json<Value> {
        axum::Json(serde_json::json!({"facet_counts": {"facet_fields": {}}}))
    }

    fn repeated_json_nl_probe(
        mbeans: axum::routing::MethodRouter,
        select: axum::routing::MethodRouter,
    ) -> ProbeApp {
        let app = Router::new()
            .route("/solr/content/admin/mbeans", mbeans)
            .route("/solr/content/select", select);
        ProbeApp {
            app,
            _workspace: ProbeWorkspace::new(),
        }
    }

    fn mbeans_missing_probe() -> ProbeApp {
        repeated_json_nl_probe(
            get(mbeans_response_missing_solr_mbeans),
            get(select_facet_category_object),
        )
    }

    fn mbeans_array_probe() -> ProbeApp {
        repeated_json_nl_probe(
            get(mbeans_response_solr_mbeans_array),
            get(select_facet_category_object),
        )
    }

    fn mbeans_null_probe() -> ProbeApp {
        repeated_json_nl_probe(
            get(mbeans_response_solr_mbeans_null),
            get(select_facet_category_object),
        )
    }

    fn mbeans_object_probe() -> ProbeApp {
        repeated_json_nl_probe(
            get(mbeans_response_solr_mbeans_object),
            get(select_facet_category_object),
        )
    }

    fn facet_flat_array_probe() -> ProbeApp {
        repeated_json_nl_probe(
            get(mbeans_response_solr_mbeans_object),
            get(select_facet_category_flat_array),
        )
    }

    fn facet_missing_probe() -> ProbeApp {
        repeated_json_nl_probe(
            get(mbeans_response_solr_mbeans_object),
            get(select_facet_category_missing),
        )
    }

    #[tokio::test]
    async fn repeated_map_and_flat_probe_rejects_a_response_missing_solr_mbeans() {
        let probe = mbeans_missing_probe();
        assert!(
            !semantic_covered(&probe, "request.json-nl.repeated-map-and-flat").await,
            "request.json-nl.repeated-map-and-flat must require `solr-mbeans` to \
             be present in the response, not merely that the request 200s -- a \
             200 with no `solr-mbeans` key at all must not count as covered"
        );
    }

    #[tokio::test]
    async fn repeated_map_and_flat_probe_rejects_a_non_object_solr_mbeans_array() {
        let probe = mbeans_array_probe();
        assert!(
            !semantic_covered(&probe, "request.json-nl.repeated-map-and-flat").await,
            "request.json-nl.repeated-map-and-flat must require `solr-mbeans` to \
             be a JSON object -- the shape the trace settled on -- not merely \
             present; a JSON array must not count as covered"
        );
    }

    #[tokio::test]
    async fn repeated_map_and_flat_probe_rejects_a_null_solr_mbeans() {
        let probe = mbeans_null_probe();
        assert!(
            !semantic_covered(&probe, "request.json-nl.repeated-map-and-flat").await,
            "request.json-nl.repeated-map-and-flat must require `solr-mbeans` to \
             be a JSON object, not merely present -- JSON null must not count as \
             covered"
        );
    }

    #[tokio::test]
    async fn repeated_map_and_flat_probe_rejects_a_flat_array_category_facet() {
        let probe = facet_flat_array_probe();
        assert!(
            !semantic_covered(&probe, "request.json-nl.repeated-map-and-flat").await,
            "request.json-nl.repeated-map-and-flat must require the \
             `/select` facet leg to render `category` as an object -- the \
             `json.nl=map` shape, because `map` is the first of the repeated \
             values. An alternating flat array is what last-value-wins, \
             ignoring `json.nl`, or dropping the repeated key would each \
             produce, and must not count as covered"
        );
    }

    #[tokio::test]
    async fn repeated_map_and_flat_probe_rejects_a_missing_category_facet() {
        let probe = facet_missing_probe();
        assert!(
            !semantic_covered(&probe, "request.json-nl.repeated-map-and-flat").await,
            "request.json-nl.repeated-map-and-flat must require the \
             `/select` facet leg to actually produce \
             `facet_counts/facet_fields/category` -- a 200 with no bucket \
             list to shape proves nothing about how the repeated `json.nl` \
             resolved, and must not count as covered"
        );
    }

    #[tokio::test]
    async fn repeated_map_and_flat_probe_accepts_an_object_solr_mbeans_and_object_facet() {
        let probe = mbeans_object_probe();
        assert!(
            semantic_covered(&probe, "request.json-nl.repeated-map-and-flat").await,
            "request.json-nl.repeated-map-and-flat must still count a 200 \
             response whose `solr-mbeans` is a genuine JSON object, alongside \
             an object-shaped `category` facet, as covered -- the tightened \
             check must not reject the shapes it is supposed to require"
        );
    }

    // Issue #140: `select.facet.per-field-missing`'s probe asserts that
    // `f.category.facet.missing=true` produces the trailing `null` bucket key
    // in the flat counts array. The weaker predicate it must not degrade back
    // to is `!counts.is_empty()` -- the `category` facet is non-empty against
    // the seeded corpus whether or not the per-field override is honoured at
    // all, so an emptiness check would report the item covered on an
    // implementation that ignores `f.<field>.facet.missing` entirely.
    //
    // The real app cannot serve that shape at this path once the feature is
    // in, so, following the #162/#167 stubs above, this drives the real
    // (private) `semantic_covered` against a throwaway router serving
    // `/solr/content/select` directly. Both the rejecting and the accepting
    // case are pinned: without the accepting one a path or pointer typo would
    // make the rejection pass vacuously.
    //
    // These stubs are deliberately query-*sensitive*, which is the lesson
    // #167's sibling comment above records the hard way. A stub that matches
    // on path alone pins the probe's predicate and nothing about its request,
    // so the probe could ask for the global `facet.missing=true` -- or for
    // nothing at all -- and every stub assertion would still pass. That is not
    // hypothetical here: with a path-only stub, swapping this probe's query to
    // the global param leaves the whole suite green, and so does reverting the
    // feature in `src/facet.rs` on top of it, because the global was already
    // implemented. The coverage artifact would then keep reporting
    // `select.facet.per-field-missing` covered against an implementation that
    // ignores `f.<field>.facet.missing` entirely -- exactly the green lie this
    // file's probes exist to prevent.
    fn counts_with_null_bucket() -> axum::Json<Value> {
        axum::Json(serde_json::json!({
            "facet_counts": {"facet_fields": {"category": ["animals", 2, "tools", 1, null, 3]}}
        }))
    }

    fn counts_without_null_bucket() -> axum::Json<Value> {
        axum::Json(serde_json::json!({
            "facet_counts": {"facet_fields": {"category": ["animals", 2, "tools", 1]}}
        }))
    }

    /// Whether the probe's raw query carries `pair` as a whole `key=value`
    /// segment. Whole-segment, not substring: `facet.missing=true` must not
    /// also match `f.category.facet.missing=true`, or the global-only stub
    /// below would be unable to tell the two params apart -- which is the
    /// distinction it exists to draw.
    fn query_carries(query: Option<&str>, pair: &str) -> bool {
        query
            .unwrap_or_default()
            .split('&')
            .any(|segment| segment == pair)
    }

    /// Models a correct implementation: the null bucket appears if and only if
    /// the *per-field* override asked for it.
    async fn select_honouring_the_per_field_override(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        if query_carries(query.as_deref(), "f.category.facet.missing=true") {
            counts_with_null_bucket()
        } else {
            counts_without_null_bucket()
        }
    }

    /// Models the pre-#140 implementation: the global `facet.missing` works,
    /// the per-field override is ignored. The probe must read this as
    /// *uncovered*, which it can only do by sending the per-field param and
    /// not the global one.
    async fn select_honouring_only_the_global_missing(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        if query_carries(query.as_deref(), "facet.missing=true") {
            counts_with_null_bucket()
        } else {
            counts_without_null_bucket()
        }
    }

    /// Models an implementation that honours neither, regardless of query.
    async fn select_ignoring_missing_entirely() -> axum::Json<Value> {
        counts_without_null_bucket()
    }

    fn select_only_probe(select: axum::routing::MethodRouter) -> ProbeApp {
        ProbeApp {
            app: Router::new().route("/solr/content/select", select),
            _workspace: ProbeWorkspace::new(),
        }
    }

    #[tokio::test]
    async fn per_field_missing_probe_rejects_counts_with_no_null_bucket() {
        let probe = select_only_probe(get(select_ignoring_missing_entirely));
        assert!(
            !semantic_covered(&probe, "select.facet.per-field-missing").await,
            "select.facet.per-field-missing must require the trailing `null` \
             bucket key that `f.category.facet.missing=true` adds, not merely a \
             non-empty `category` counts array -- the array is non-empty \
             without the override too, so an emptiness check would call an \
             implementation that ignores the per-field override covered"
        );
    }

    #[tokio::test]
    async fn per_field_missing_probe_rejects_an_empty_counts_array() {
        let probe = select_only_probe(get(select_facet_category_missing));
        assert!(
            !semantic_covered(&probe, "select.facet.per-field-missing").await,
            "select.facet.per-field-missing must not count a response with no \
             `facet_counts/facet_fields/category` at all as covered"
        );
    }

    /// The request-side guard, and the one that makes this probe evidence of
    /// anything: against a server that honours the *global* `facet.missing`
    /// but ignores the per-field override -- i.e. Wayfinder immediately
    /// before this issue -- the item must read uncovered. A probe whose query
    /// asks for the global param (or omits the per-field one) passes here
    /// only because the global was already implemented, so this is what
    /// stops the coverage artifact certifying an unimplemented feature.
    #[tokio::test]
    async fn per_field_missing_probe_rejects_a_server_that_honours_only_the_global() {
        let probe = select_only_probe(get(select_honouring_only_the_global_missing));
        assert!(
            !semantic_covered(&probe, "select.facet.per-field-missing").await,
            "select.facet.per-field-missing must probe with \
             `f.category.facet.missing=true` and *not* the global \
             `facet.missing=true` -- against a server implementing only the \
             global, the item must read uncovered. If this fails, the probe's \
             query is asking for the wrong param and the item would report \
             covered with `f.<field>.facet.missing` unimplemented"
        );
    }

    #[tokio::test]
    async fn per_field_missing_probe_accepts_counts_with_the_null_bucket() {
        let probe = select_only_probe(get(select_honouring_the_per_field_override));
        assert!(
            semantic_covered(&probe, "select.facet.per-field-missing").await,
            "select.facet.per-field-missing must still count the shape it is \
             supposed to require -- a flat counts array carrying the trailing \
             `null` bucket, served in response to the per-field override -- as \
             covered; if this fails the rejections above are passing vacuously \
             (wrong path, wrong pointer, or a query that no longer sends \
             `f.category.facet.missing=true`)"
        );
    }

    // ---- issue #188: the `fl=*` probes were false-positive greens ----------
    //
    // `select.fl.wildcard-plus-score` asserted only that
    // `/response/docs/0/score` exists. `score` is the *one* member of
    // `fl=*,score` that pre-#188 `render_doc` understood -- `*` was matched as
    // a literal field name, which no schema field has, so every real field was
    // dropped and the probe read the item as covered against an implementation
    // with no wildcard support at all. Same class of false green as #162/#167,
    // and the reason the coverage artifact certified an unimplemented feature.
    //
    // `mlt.fl.wildcard-plus-score` read *uncovered*, but incidentally rather
    // than for the right reason: its query
    // (`mlt?q=id:mlt11&mlt.fl=body&fl=*,score`) omitted `mlt.mintf`/`mlt.mindf`,
    // and real Solr's defaults (mintf=2/mindf=5) return no similar docs at all
    // against a 20-doc corpus (finding 64), so `/response/docs/0` did not
    // exist whatever `fl` did. Verified: `PROBE_DOCS` seeds exactly that
    // corpus, and the sibling `mlt.mintf`/`mlt.mindf` probes above pin
    // `numFound > 0` only with the thresholds loosened. So the request needs
    // fixing alongside the predicate, or the item would keep reading uncovered
    // after the wildcard landed.
    //
    // Both probes are therefore driven here against throwaway routers, in the
    // query-*sensitive* style #140 established below (`query_carries`): a stub
    // that matched on path alone would pin the predicate and nothing about the
    // request, so the probe could enumerate `fl=id,body,category,score`
    // literally -- which pre-#188 `render_doc` already answered correctly --
    // and every assertion would still pass.

    /// `doc1`'s *declared* fields as `PROBE_DOCS` seeds them, field by field in
    /// `PROBE_SCHEMA`'s `[[fields]]` declaration order. That order is the
    /// contract: real Solr renders doc keys in schema order, not `fl` order
    /// (`solr-ref/responses/select_fl_reversed.json`), and `fl=*` must produce
    /// every one of them (`select_all.json`, and
    /// `solr-ref/responses/mlt_fl_wildcard_score.json` for the `,score`
    /// composition).
    ///
    /// `doc1`'s dynamic-rule field `ss_sku` (matched by `PROBE_SCHEMA`'s `ss_*`
    /// rule) is deliberately absent. That rule exists so the *real-app* leg of
    /// the `fl=*,score` probes can tell a full wildcard expansion from a
    /// declared-fields-only one, since `render_doc` walks declared and dynamic
    /// fields in two separate loops. These stub handlers model `fl` semantics,
    /// not `render_doc`'s loop structure: a partial expansion is expressed by
    /// passing `render_probe_doc` a subset of these names as
    /// `wildcard_fields` (`&["id"]` in
    /// `select_wildcard_plus_score_probe_rejects_a_partial_wildcard_expansion`),
    /// which discriminates without needing a second field class. Expected and
    /// actual bodies on the stub leg both derive from this one list, so it
    /// stays self-consistent; adding `ss_sku` here would change nothing the
    /// probes assert.
    fn probe_stored_fields() -> Vec<(&'static str, Value)> {
        vec![
            ("id", serde_json::json!("doc1")),
            ("body", serde_json::json!("quick brown fox rocket")),
            ("category", serde_json::json!(["animals", "classic"])),
            ("rating", serde_json::json!(3)),
            ("created", serde_json::json!("2024-01-02T00:00:00Z")),
            ("featured", serde_json::json!("true")),
        ]
    }

    /// The raw `fl` value the probe's query carries, if any. Whole-segment, for
    /// the same reason `query_carries` below is: `fl=` must not also match
    /// `hl.fl=` or `mlt.fl=`, which every `/mlt` probe query also sends.
    fn fl_value(query: Option<&str>) -> Option<String> {
        query
            .unwrap_or_default()
            .split('&')
            .find_map(|segment| segment.strip_prefix("fl=").map(str::to_owned))
    }

    /// Renders one doc the way a server with the given capabilities would
    /// answer `fl`. `wildcard_fields` is what `*` expands to -- every stored
    /// field for a correct implementation, none at all for pre-#188
    /// `render_doc` (which matched `*` as a literal field name), or a subset
    /// for a partial expansion that forgets a whole class of field (e.g.
    /// `render_doc`'s separate dynamic-field loop). An absent `fl` always
    /// renders every stored field and no `score`, which is Solr's default.
    fn render_probe_doc(fl: Option<&str>, wildcard_fields: &[&str], honour_score: bool) -> Value {
        let requested: Vec<&str> = fl.map(|v| v.split(',').collect()).unwrap_or_default();
        let mut doc = serde_json::Map::new();
        for (name, value) in probe_stored_fields() {
            let wanted = match fl {
                None => true,
                Some(_) => {
                    requested.contains(&name)
                        || (requested.contains(&"*") && wildcard_fields.contains(&name))
                }
            };
            if wanted {
                doc.insert(name.to_string(), value);
            }
        }
        if honour_score && requested.contains(&"score") {
            doc.insert("score".to_string(), serde_json::json!(1.0));
        }
        Value::Object(doc)
    }

    fn probe_result_block(docs: Vec<Value>) -> Value {
        serde_json::json!({
            "numFound": docs.len(),
            "start": 0,
            "maxScore": 1.0,
            "numFoundExact": true,
            "docs": docs,
        })
    }

    fn probe_select_body(fl: Option<&str>, wildcard_fields: &[&str], score: bool) -> Value {
        serde_json::json!({
            "response": probe_result_block(vec![render_probe_doc(fl, wildcard_fields, score)]),
        })
    }

    /// The `/mlt` envelope: `match` (the seed doc) and `response` (the similar
    /// docs), both rendered through the same `fl` -- which is why the wildcard
    /// gap showed up on both blocks in `mlt_fl_wildcard_score.json`.
    /// `similar_docs` empty models real Solr's default mintf/mindf finding
    /// nothing similar.
    fn probe_mlt_body(
        fl: Option<&str>,
        wildcard_fields: &[&str],
        score: bool,
        similar: bool,
    ) -> Value {
        let doc = render_probe_doc(fl, wildcard_fields, score);
        let similar_docs = if similar {
            vec![doc.clone()]
        } else {
            Vec::new()
        };
        serde_json::json!({
            "match": probe_result_block(vec![doc]),
            "response": probe_result_block(similar_docs),
        })
    }

    fn all_probe_field_names() -> Vec<&'static str> {
        probe_stored_fields().into_iter().map(|(n, _)| n).collect()
    }

    /// Models a correct implementation: `*` expands to every stored field and
    /// composes with `score`.
    async fn select_expanding_the_wildcard_and_honouring_score(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        axum::Json(probe_select_body(
            fl_value(query.as_deref()).as_deref(),
            &all_probe_field_names(),
            true,
        ))
    }

    /// Models pre-#188 `render_doc`: `fl` is a literal-name allowlist, so a
    /// name it recognises works and `*` matches nothing -- while `score` is
    /// still honoured. This is the shape the old probe called covered.
    ///
    /// Also the request-side guard: it answers an *enumerated*
    /// `fl=id,body,category,...,score` completely, so a probe that stopped
    /// sending `*` would read covered against a server with no wildcard
    /// support at all.
    async fn select_treating_the_wildcard_as_a_literal_name(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        axum::Json(probe_select_body(
            fl_value(query.as_deref()).as_deref(),
            &[],
            true,
        ))
    }

    /// Models the other half being dropped: `*` expands, `score` does not.
    async fn select_expanding_the_wildcard_but_dropping_score(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        axum::Json(probe_select_body(
            fl_value(query.as_deref()).as_deref(),
            &all_probe_field_names(),
            false,
        ))
    }

    /// Models a partial expansion -- `*` reaches some stored fields but not
    /// all, which is what fixing only `render_doc`'s declared-`[[fields]]` loop
    /// and not its dynamic-field loop would look like.
    async fn select_expanding_the_wildcard_partially(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        axum::Json(probe_select_body(
            fl_value(query.as_deref()).as_deref(),
            &["id"],
            true,
        ))
    }

    fn mlt_only_probe(mlt: axum::routing::MethodRouter) -> ProbeApp {
        ProbeApp {
            app: Router::new().route("/solr/content/mlt", mlt),
            _workspace: ProbeWorkspace::new(),
        }
    }

    async fn mlt_expanding_the_wildcard_and_honouring_score(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        axum::Json(probe_mlt_body(
            fl_value(query.as_deref()).as_deref(),
            &all_probe_field_names(),
            true,
            true,
        ))
    }

    async fn mlt_treating_the_wildcard_as_a_literal_name(
        RawQuery(query): RawQuery,
    ) -> axum::Json<Value> {
        axum::Json(probe_mlt_body(
            fl_value(query.as_deref()).as_deref(),
            &[],
            true,
            true,
        ))
    }

    /// Models real Solr's *default* `mlt.mintf=2`/`mlt.mindf=5` against a
    /// 20-doc corpus: nothing is similar enough, so `response.docs` is empty
    /// however good the `fl` handling is (finding 64). Loosened thresholds are
    /// what make the similar-docs set non-empty, so the probe must send them --
    /// otherwise `mlt.fl.wildcard-plus-score` reads uncovered for a reason that
    /// has nothing to do with `fl`, which is exactly how it read before #188.
    async fn mlt_needing_loosened_thresholds(RawQuery(query): RawQuery) -> axum::Json<Value> {
        let loosened = query_carries(query.as_deref(), "mlt.mintf=1")
            && query_carries(query.as_deref(), "mlt.mindf=1");
        axum::Json(probe_mlt_body(
            fl_value(query.as_deref()).as_deref(),
            &all_probe_field_names(),
            true,
            loosened,
        ))
    }

    #[tokio::test]
    async fn select_wildcard_plus_score_probe_rejects_a_server_that_drops_the_wildcard() {
        let probe = select_only_probe(get(select_treating_the_wildcard_as_a_literal_name));
        assert!(
            !semantic_covered(&probe, "select.fl.wildcard-plus-score").await,
            "issue #188: select.fl.wildcard-plus-score must require the fields `*` expands to, \
             not just that `score` is present. Against a server that honours `score` and treats \
             `*` as a literal field name -- i.e. Wayfinder immediately before this issue -- the \
             item must read UNCOVERED. If this fails, the probe is still the false-positive green \
             the issue names, and the coverage artifact is certifying `fl=*` against an \
             implementation with no wildcard support at all"
        );
    }

    #[tokio::test]
    async fn select_wildcard_plus_score_probe_rejects_a_server_that_drops_score() {
        let probe = select_only_probe(get(select_expanding_the_wildcard_but_dropping_score));
        assert!(
            !semantic_covered(&probe, "select.fl.wildcard-plus-score").await,
            "tightening the wildcard half must not lose the `score` half: a server that expands \
             `*` but never emits `score` must still read uncovered"
        );
    }

    #[tokio::test]
    async fn select_wildcard_plus_score_probe_rejects_a_partial_wildcard_expansion() {
        let probe = select_only_probe(get(select_expanding_the_wildcard_partially));
        assert!(
            !semantic_covered(&probe, "select.fl.wildcard-plus-score").await,
            "`fl=*` is *every* stored field (`select_all.json`), so an expansion that reaches \
             only some of them -- what fixing `render_doc`'s declared-fields loop but not its \
             dynamic-fields loop would produce -- must read uncovered"
        );
    }

    #[tokio::test]
    async fn select_wildcard_plus_score_probe_accepts_a_server_honouring_both() {
        let probe = select_only_probe(get(select_expanding_the_wildcard_and_honouring_score));
        assert!(
            semantic_covered(&probe, "select.fl.wildcard-plus-score").await,
            "select.fl.wildcard-plus-score must still count the shape it is supposed to require \
             -- every stored field plus `score`, served in response to `fl=*,score` -- as \
             covered; if this fails the rejections above are passing vacuously (wrong path, wrong \
             field set, or a query that no longer sends `fl=*,score`)"
        );
    }

    #[tokio::test]
    async fn mlt_wildcard_plus_score_probe_rejects_a_server_that_drops_the_wildcard() {
        let probe = mlt_only_probe(get(mlt_treating_the_wildcard_as_a_literal_name));
        assert!(
            !semantic_covered(&probe, "mlt.fl.wildcard-plus-score").await,
            "the `/mlt` half of the same `render_doc` gap: a server returning similar docs that \
             carry `score` and nothing else must read uncovered"
        );
    }

    #[tokio::test]
    async fn mlt_wildcard_plus_score_probe_loosens_mintf_and_mindf() {
        let probe = mlt_only_probe(get(mlt_needing_loosened_thresholds));
        assert!(
            semantic_covered(&probe, "mlt.fl.wildcard-plus-score").await,
            "mlt.fl.wildcard-plus-score's query must send `mlt.mintf=1&mlt.mindf=1`. Against real \
             Solr's defaults this corpus has no similar docs at all (finding 64), so with the \
             thresholds left at their defaults the item reads uncovered whatever `fl` does -- \
             which is how it read before #188, for a reason unrelated to the wildcard. This stub \
             honours `fl=*,score` perfectly and only withholds the similar-docs set until the \
             thresholds are loosened"
        );
    }

    #[tokio::test]
    async fn mlt_wildcard_plus_score_probe_accepts_a_server_honouring_both() {
        let probe = mlt_only_probe(get(mlt_expanding_the_wildcard_and_honouring_score));
        assert!(
            semantic_covered(&probe, "mlt.fl.wildcard-plus-score").await,
            "the `/mlt` accepting case, so the two rejections above cannot pass vacuously"
        );
    }

    /// Both `fl=*` items must read covered against the *real* seeded app once
    /// `render_doc` understands the wildcard. This is the end-to-end check the
    /// stubs above cannot make: they pin what the predicate and the request
    /// must be, not that Wayfinder actually satisfies them.
    ///
    /// `mlt.fl.wildcard-plus-score` is the one that moves the numerator -- it
    /// was uncovered before #188 -- so `EXPECTED_FRACTION` in
    /// `tests/search_api_coverage.rs` goes up by one.
    #[tokio::test]
    async fn both_wildcard_plus_score_items_are_covered_against_the_real_seeded_app() {
        let probe = ProbeApp::new().await;
        for id in [
            "select.fl.wildcard-plus-score",
            "mlt.fl.wildcard-plus-score",
        ] {
            assert!(
                semantic_covered(&probe, id).await,
                "{id} must read covered against the real routed handlers once `render_doc` \
                 expands `fl=*` to every stored field and composes it with `score`"
            );
        }
    }

    /// `"select.highlight.snippets"` used to probe with `hl.snippets=1`, which
    /// is exactly `DEFAULT_SNIPPETS` in `src/highlight.rs`. Against `doc1.body`
    /// ("quick brown fox rocket") -- which contains "quick" exactly once --
    /// Tantivy's `SnippetGenerator` can only ever produce one snippet window
    /// for that query, so that probe passed identically whether or not
    /// `hl.snippets` was honored at all. Distinguishing the two needs a doc
    /// where more than one snippet is even possible.
    ///
    /// `HL_SNIPPETS_PROBE_DOCS` supplies that corpus: `hl-snippets-gizmo`'s
    /// `body` repeats a term unique to it ("gizmo") three times, each
    /// occurrence separated by 100+ chars of unrelated filler -- wide enough
    /// that a real multi-fragment highlighter would return three distinct,
    /// non-overlapping windows rather than merging them into one.
    ///
    /// Why 3 and not 1: before issue #103, `CoreIndex::highlight_field` could
    /// only ever return Tantivy's single best-scoring fragment
    /// (`select_best_fragment_combination` is private in
    /// `tantivy-0.26.1/src/snippet/mod.rs`), so `hl.snippets > 1` was a
    /// structural no-op. #103 lifted that by looping the single-fragment
    /// extraction against a progressively masked remainder of the source
    /// text, so `hl.snippets=3` against a doc with three well-separated
    /// occurrences of the query term returns all three. This equally catches
    /// the opposite regression -- a change that starts emitting some *other*
    /// number of snippets without anyone deciding to.
    #[tokio::test]
    async fn snippets_cap_is_distinguishable_from_default() {
        let probe = ProbeApp::new().await;

        let one = probe
            .response("select?q=gizmo&hl=true&hl.fl=body&hl.snippets=1")
            .await
            .expect("hl.snippets=1 select response");
        let one_snippets = one
            .pointer("/highlighting/hl-snippets-gizmo/body")
            .and_then(Value::as_array)
            .expect("hl.snippets=1 highlighting/hl-snippets-gizmo/body array");
        assert_eq!(
            one_snippets.len(),
            1,
            "hl.snippets=1 should cap to exactly one snippet, got {one_snippets:?}"
        );

        let three = probe
            .response("select?q=gizmo&hl=true&hl.fl=body&hl.snippets=3")
            .await
            .expect("hl.snippets=3 select response");
        let three_snippets = three
            .pointer("/highlighting/hl-snippets-gizmo/body")
            .and_then(Value::as_array)
            .expect("hl.snippets=3 highlighting/hl-snippets-gizmo/body array");
        assert_eq!(
            three_snippets.len(),
            3,
            "issue #103: hl.snippets=3 against a doc with three well-separated occurrences of \
             the query term must return all three snippets, not the pre-#103 single-fragment \
             ceiling. Got {three_snippets:?}"
        );
    }

    /// Issue #104: `hl.fragsize=0` must return the *entire* field as one
    /// unfragmented snippet (fixtures `hl_fragsize_zero_whole_field.json` /
    /// `hl_fragsize_zero_whole_field_method_original.json`), not a fragment
    /// truncated to some default budget. This calls `semantic_covered`
    /// directly (rather than only through `report()`) so the assertion names
    /// exactly which contract entry regressed, and exercises both `hl.method`
    /// paths this probe's `"select.highlight.fragsize"` arm now checks:
    /// default (`hl.method` unset, i.e. `unified`) is asserted here via the
    /// arm itself, and `hl.method=original` is asserted directly below
    /// against the same expectation.
    #[tokio::test]
    async fn fragsize_zero_returns_whole_field_not_a_fragment() {
        let probe = ProbeApp::new().await;

        assert!(
            semantic_covered(&probe, "select.highlight.fragsize").await,
            "select.highlight.fragsize probe must observe hl.fragsize=0 returning the whole \
             field (issue #104), not merely a presence check"
        );

        let expected_whole_field = concat!(
            "<em>wexford</em> prototype notes from the engineering standup this morning. ",
            "the team reviewed the roadmap for the next quarter and discussed several ",
            "open risks around supply chain timing. afterwards everyone broke for lunch ",
            "and reconvened at two in the afternoon to continue the planning session for ",
            "the rest of the week."
        );

        let unified = probe
            .response("select?q=wexford&hl=true&hl.fl=body&hl.fragsize=0")
            .await
            .expect("hl.fragsize=0 select response");
        assert_eq!(
            unified.pointer("/highlighting/hl-fragsize-long/body/0"),
            Some(&Value::String(expected_whole_field.to_string())),
            "default hl.method with hl.fragsize=0 must return the whole field unfragmented, \
             got {unified:?}"
        );

        let original = probe
            .response("select?q=wexford&hl=true&hl.fl=body&hl.method=original&hl.fragsize=0")
            .await
            .expect("hl.method=original&hl.fragsize=0 select response");
        assert_eq!(
            original.pointer("/highlighting/hl-fragsize-long/body/0"),
            Some(&Value::String(expected_whole_field.to_string())),
            "hl.method=original with hl.fragsize=0 must return the whole field unfragmented, \
             not fall back to DEFAULT_FRAGSIZE, got {original:?}"
        );
    }

    /// Pins the property the two tests above depend on: pure string math, no
    /// Tantivy involved, so a future edit to `HL_SNIPPETS_PROBE_DOCS` that
    /// shrinks the filler back below a snippet window (`TANTIVY_DEFAULT_MAX_CHARS`
    /// in `src/highlight.rs`, currently 150) fails here instead of silently
    /// making `snippets_cap_is_distinguishable_from_default` untestable-by-#103
    /// -- both `hl.snippets=1` and `hl.snippets=3` would then read `1` for a
    /// reason unrelated to the single-fragment ceiling.
    #[test]
    fn hl_snippets_probe_doc_gaps_exceed_a_snippet_window() {
        const TANTIVY_DEFAULT_MAX_CHARS: usize = 150;
        let doc: Value =
            serde_json::from_str(HL_SNIPPETS_PROBE_DOCS).expect("parse HL_SNIPPETS_PROBE_DOCS");
        let body = doc[0]["body"].as_str().expect("hl-snippets-gizmo body");
        let offsets: Vec<usize> = body
            .match_indices("gizmo")
            .map(|(offset, _)| offset)
            .collect();
        assert_eq!(
            offsets.len(),
            3,
            "expected exactly three \"gizmo\" occurrences, found {offsets:?}"
        );
        for pair in offsets.windows(2) {
            let gap = pair[1] - pair[0];
            assert!(
                gap > TANTIVY_DEFAULT_MAX_CHARS,
                "gizmo occurrences at {} and {} are only {gap} chars apart, \
                 not wider than a {TANTIVY_DEFAULT_MAX_CHARS}-char snippet window",
                pair[0],
                pair[1]
            );
        }
    }
}
