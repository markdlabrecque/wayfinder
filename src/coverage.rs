//! Hermetic Search API coverage report for the frozen #55 capture.
//!
//! The production contract supplies the denominator and provenance only. The
//! report calculates classifications from the real router, strict allowlists,
//! and typed renderer/semantic capability surfaces below; it never deserializes
//! or reads a contract `covered` value.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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

#[derive(Clone, Debug, Deserialize)]
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
        48,
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
    }
    for field in &contract.response_fields {
        assert!(
            field
                .trace
                .iter()
                .all(|trace| trace_names.contains(trace.as_str()))
        );
        assert!(
            field
                .consumer
                .source
                .starts_with("vendor/drupal/search_api_solr/src/")
        );
        assert!(field.consumer.symbol.contains("::"));
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
    root: PathBuf,
}

impl ProbeWorkspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "wayfinder-coverage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&root).expect("create hermetic coverage workspace");
        Self { root }
    }
}

impl Drop for ProbeWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
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
  {"id":"doc3","body":"slow turtle","category":["misc"],"rating":5,"created":"2024-01-03T00:00:00Z","featured":"true"}
]"#;

impl ProbeApp {
    async fn new() -> Self {
        let workspace = ProbeWorkspace::new();
        let schema = workspace.root.join("schema.toml");
        let config = workspace.root.join("wayfinder.toml");
        let data = workspace.root.join("data");
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

    async fn has(&self, path: &str, pointer: &str) -> bool {
        let (status, body) = self.request(Method::GET, path, None).await;
        status == StatusCode::OK && body.pointer(pointer).is_some()
    }

    async fn number(&self, path: &str, pointer: &str) -> Option<u64> {
        let (status, body) = self.request(Method::GET, path, None).await;
        (status == StatusCode::OK)
            .then(|| body.pointer(pointer).and_then(Value::as_u64))
            .flatten()
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
                && probe.number("select?q=id:first", "/response/numFound").await == Some(1)
                && probe.number("select?q=id:second", "/response/numFound").await == Some(1)
        }
        "update.commitWithin" => {
            probe
                .request(Method::POST, "content/update?commitWithin=1", Some("[]"))
                .await
                .0
                == StatusCode::OK
        }
        "request.omitHeader" => {
            let (status, body) = probe.request(Method::GET, "select?q=*:*&omitHeader=true", None).await;
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
        "request.json-nl.repeated-map-and-flat" => probe
            .ok("content/admin/mbeans?json.nl=flat&json.nl=map")
            .await,
        "request.timezone.utc" => probe.ok("select?q=*:*&TZ=UTC").await
            && probe.ok("mlt?q=id:doc1&mlt.fl=body&TZ=UTC").await,
        "select.q.plain-query" => probe.number("select?q=quick", "/response/numFound").await == Some(2),
        "select.q.local-params-edismax" => probe
            .number("select?q=%7B!edismax%7Dquick", "/response/numFound")
            .await
            == Some(2),
        "select.pagination.start-and-rows" => probe.has("select?q=*:*&start=0&rows=1", "/response/docs/0").await,
        "select.rows.zero" => probe.number("select?q=*:*&rows=0", "/response/numFound").await == Some(4)
            && !probe.has("select?q=*:*&rows=0", "/response/docs/0").await,
        "select.fl.wildcard-plus-score" => probe.has("select?q=quick&fl=*,score", "/response/docs/0/score").await,
        "select.fq.string" => probe.number("select?q=*:*&fq=category:animals", "/response/numFound").await == Some(1),
        "select.fq.range" => probe.number("select?q=*:*&fq=rating:%5B3%20TO%20*%5D", "/response/numFound").await == Some(2),
        "select.fq.boolean" => probe.number("select?q=*:*&fq=featured:true", "/response/numFound").await == Some(2),
        "select.fq.multi-value-or" => probe.number("select?q=*:*&fq=(category:animals%20category:garden)", "/response/numFound").await == Some(2),
        "select.sort.integer" => probe.has("select?q=*:*&sort=rating%20desc", "/response/docs/0").await,
        "select.sort.string" => probe.has("select?q=*:*&sort=category%20asc", "/response/docs/0").await,
        "select.sort.date" => probe.has("select?q=*:*&sort=created%20asc", "/response/docs/0").await,
        "select.highlight.enabled" => probe.has("select?q=quick&hl=true&hl.fl=body", "/highlighting/doc1").await,
        "select.highlight.wildcard-fields" => probe.has("select?q=quick&hl=true&hl.fl=*", "/highlighting/doc1/body").await,
        "select.highlight.require-field-match" => probe.ok("select?q=quick&hl.requireFieldMatch=false").await,
        "select.highlight.snippets" => probe.has("select?q=quick&hl=true&hl.fl=body&hl.snippets=1", "/highlighting/doc1/body/0").await,
        "select.highlight.fragsize" => probe.has("select?q=quick&hl=true&hl.fl=body&hl.fragsize=0", "/highlighting/doc1/body/0").await,
        "select.highlight.merge-contiguous" => probe.ok("select?q=quick&hl.mergeContiguous=false").await,
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
        "select.facet.field" => probe.has("select?q=*:*&facet=true&facet.field=category", "/facet_counts/facet_fields/category").await,
        "select.facet.local-key" => probe.has("select?q=*:*&facet=true&facet.field=%7B!key=kind%7Dcategory", "/facet_counts/facet_fields/kind").await,
        "select.facet.per-field-missing" => probe.has("select?q=*:*&facet=true&facet.field=category&f.category.facet.missing=true", "/facet_counts/facet_fields").await,
        "select.facet.sort-limit-mincount" => probe.has("select?q=*:*&facet=true&facet.field=category&facet.sort=count&facet.limit=1&facet.mincount=1", "/facet_counts/facet_fields/category").await,
        "select.facet.global-missing" => probe.has("select?q=*:*&facet=true&facet.field=category&facet.missing=false", "/facet_counts/facet_fields").await,
        "select.spellcheck.enable" => probe.has("select?q=quick&spellcheck=true", "/spellcheck").await,
        "select.spellcheck.query" => probe.has("select?q=quick&spellcheck=true&spellcheck.q=qwick", "/spellcheck/suggestions").await,
        "select.spellcheck.dictionaries" => probe.has("select?q=quick&spellcheck=true&spellcheck.dictionary=en", "/spellcheck/suggestions").await,
        "select.spellcheck.collate" => probe.has("select?q=quick&spellcheck=true&spellcheck.collate=true", "/spellcheck/collations").await,
        "mlt.base-lookup" => probe.has("mlt?q=id:doc1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1", "/response").await,
        "mlt.pagination.start-and-rows" => probe.has("mlt?q=id:doc1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&start=0&rows=1", "/response/docs").await,
        "mlt.fl.wildcard-plus-score" => probe.has("mlt?q=id:doc1&mlt.fl=body&fl=*,score", "/response/docs/0/score").await,
        "mlt.filters" => probe.ok("mlt?q=id:doc1&mlt.fl=body&fq=category:animals").await,
        "mlt.mintf" => probe.has("mlt?q=id:doc1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1", "/response/docs").await,
        "mlt.mindf" => probe.has("mlt?q=id:doc1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1", "/response/docs").await,
        "mlt.maxqt" => probe.has("mlt?q=id:doc1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxqt=1", "/response/docs").await,
        "mlt.maxntp" => probe.ok("mlt?q=id:doc1&mlt.fl=body&mlt.maxntp=2000").await,
        "mlt.boost" => probe.has("mlt?q=id:doc1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.boost=true", "/response/docs").await,
        "mlt.match-include-and-offset" => probe.ok("mlt?q=id:doc1&mlt.fl=body&mlt.match.include=false&mlt.match.offset=0").await,
        "mlt.interesting-terms-none" => {
            let (status, body) = probe.request(Method::GET, "mlt?q=id:doc1&mlt.fl=body&mlt.interestingTerms=none", None).await;
            status == StatusCode::OK && body.get("interestingTerms").is_none()
        }
        "admin.mbeans.stats" => probe.ok("content/admin/mbeans?stats=true").await,
        "terms.enumeration" => probe.ok("content/terms?terms=true&terms.fl=body").await,
        _ => panic!("unrecognised Search API semantic denominator item: {id}"),
    }
}

async fn response_field_covered(probe: &ProbeApp, id: &str) -> bool {
    match id {
        "select.response.numFound" => probe.has("select?q=*:*", "/response/numFound").await,
        "select.response.docs" => probe.has("select?q=*:*", "/response/docs").await,
        "select.response.docs.score" => {
            probe
                .has("select?q=quick&fl=id,score", "/response/docs/0/score")
                .await
        }
        "select.highlighting" => {
            probe
                .has("select?q=quick&hl=true&hl.fl=body", "/highlighting")
                .await
        }
        "select.facet_counts" => {
            probe
                .has(
                    "select?q=*:*&facet=true&facet.field=category",
                    "/facet_counts",
                )
                .await
        }
        "select.facet_counts.facet_fields" => {
            probe
                .has(
                    "select?q=*:*&facet=true&facet.field=category",
                    "/facet_counts/facet_fields",
                )
                .await
        }
        "select.spellcheck.suggestions" => {
            probe
                .has("select?q=quick&spellcheck=true", "/spellcheck/suggestions")
                .await
        }
        "select.spellcheck.collations" => {
            probe
                .has("select?q=quick&spellcheck=true", "/spellcheck/collations")
                .await
        }
        "mlt.response" => probe.has("mlt?q=id:doc1&mlt.fl=body", "/response").await,
        "admin.info-system.lucene.solr-spec-version" => {
            probe
                .has("/solr/admin/info/system", "/lucene/solr-spec-version")
                .await
        }
        "admin.system.core.schema" => probe.has("content/admin/system", "/core/schema").await,
        "schema.fieldtypes.fieldTypes" => {
            probe.has("content/schema/fieldtypes", "/fieldTypes").await
        }
        "admin.luke.index" => probe.has("content/admin/luke", "/index").await,
        "admin.mbeans.solr-mbeans" => probe.has("content/admin/mbeans", "/solr-mbeans").await,
        "terms.terms" => probe.has("content/terms?terms=true", "/terms").await,
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
}
