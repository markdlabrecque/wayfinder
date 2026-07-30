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
"#;

const PROBE_DOCS: &str = r#"[
  {"id":"doc1","body":"quick brown fox rocket","category":["animals","classic"],"rating":3,"created":"2024-01-02T00:00:00Z","featured":"true"},
  {"id":"doc2","body":"quick fox rocket","category":["garden"],"rating":1,"created":"2024-01-01T00:00:00Z","featured":"false"},
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
  {"id":"mlt11","body":"astronomers observed a bright comet streaking across the night sky","category":["astronomy"],"rating":20,"created":"2024-02-11T00:00:00Z","featured":"m"},
  {"id":"mlt12","body":"the telescope revealed distant galaxies and bright stars","category":["astronomy"],"rating":21,"created":"2024-02-12T00:00:00Z","featured":"m"},
  {"id":"mlt13","body":"a lunar eclipse darkened the night sky for hours","category":["astronomy"],"rating":22,"created":"2024-02-13T00:00:00Z","featured":"m"},
  {"id":"mlt14","body":"scientists study the orbit of planets around distant stars","category":["astronomy"],"rating":23,"created":"2024-02-14T00:00:00Z","featured":"m"},
  {"id":"mlt15","body":"the night sky was clear enough to see the milky way","category":["astronomy"],"rating":24,"created":"2024-02-15T00:00:00Z","featured":"m"},
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
        "request.json-nl.repeated-map-and-flat" => {
            probe
                .ok("content/admin/mbeans?json.nl=flat&json.nl=map")
                .await
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
            probe
                .has("select?q=quick&fl=*,score", "/response/docs/0/score")
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
        // "zero-whole-field"), and no fixture pins what Solr returns for it,
        // so that half stays a presence check -- tracked as issue #104 (needs
        // a long-field capture.sh fixture to make whole-field-vs-fragmented
        // observable). Truncation is only observable
        // under `hl.method=original` (finding 54, `src/highlight.rs` module
        // docs; fixture `hl_fragsize_truncated.json`), so the second half
        // asks for a 10-char budget over `doc1`'s "quick brown fox rocket"
        // and requires the snippet to actually come back shorter than the
        // untruncated field -- otherwise an implementation that dropped
        // `hl.fragsize` on the floor entirely would still score this covered.
        "select.highlight.fragsize" => {
            let captured_shape = probe
                .has(
                    "select?q=quick&hl=true&hl.fl=body&hl.fragsize=0",
                    "/highlighting/doc1/body/0",
                )
                .await;
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
            captured_shape && truncated
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
            probe
                .has(
                    "select?q=*:*&facet=true&facet.field=category&f.category.facet.missing=true",
                    "/facet_counts/facet_fields",
                )
                .await
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
        "mlt.fl.wildcard-plus-score" => {
            probe
                .has(
                    "mlt?q=id:mlt11&mlt.fl=body&fl=*,score",
                    "/response/docs/0/score",
                )
                .await
        }
        "mlt.filters" => {
            probe
                .ok("mlt?q=id:mlt11&mlt.fl=body&fq=category:animals")
                .await
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
        "mlt.match-include-and-offset" => {
            probe
                .ok("mlt?q=id:doc1&mlt.fl=body&mlt.match.include=false&mlt.match.offset=0")
                .await
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
        "schema.fieldtypes.fieldTypes" => probe
            .response("content/schema/fieldtypes")
            .await
            .and_then(|body| body.get("fieldTypes").cloned())
            .is_some_and(|value| value.is_array()),
        "admin.luke.index" => probe
            .response("content/admin/luke")
            .await
            .and_then(|body| body.get("index").cloned())
            .is_some_and(|value| value.is_object()),
        "admin.mbeans.solr-mbeans" => probe
            .response("content/admin/mbeans")
            .await
            .and_then(|body| body.get("solr-mbeans").cloned())
            .is_some_and(|value| value.is_object()),
        "terms.terms" => probe
            .response("content/terms?terms=true")
            .await
            .and_then(|body| body.get("terms").cloned())
            .is_some_and(|value| value.is_object()),
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

    #[test]
    fn contract_rejects_manual_coverage_classifications() {
        let mut contract: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
        contract["request_semantics"][0]["covered"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Contract>(contract).is_err());
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
