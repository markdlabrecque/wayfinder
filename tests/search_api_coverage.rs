//! Search API coverage denominator contract (#56).
//!
//! The production-owned derived contract is frozen from issue #55's 28 traces.
//! `wayfinder coverage --format json` must emit its complete, deterministic
//! denominator, live numerator evidence, explicit fractions, and all uncovered
//! IDs. The test independently reparses every request URL and body so a
//! hand-written or incomplete contract cannot pass.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

const CONTRACT: &str = include_str!("../coverage/search_api_coverage_contract.json");
const TRACE_DIR: &str = "solr-ref/search-api/trace";
const TRACE_MANIFEST: &str = "solr-ref/search-api/manifest.tsv";

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Contract {
    traces: Vec<Trace>,
    captured_parameters: Vec<CapturedParameter>,
    endpoints: Vec<Item>,
    request_semantics: Vec<SemanticItem>,
    response_fields: Vec<ResponseItem>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
struct Trace {
    file: String,
    seq: u64,
    method: String,
    endpoint: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct CapturedParameter {
    name: String,
    occurrences: Vec<Occurrence>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
struct Occurrence {
    value: String,
    trace: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
struct Item {
    id: String,
    trace: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct SemanticItem {
    id: String,
    trace: Vec<String>,
    parameters: Vec<SemanticParameter>,
    body_variants: Vec<BodyVariant>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
struct SemanticParameter {
    name: String,
    variant: String,
    values: Vec<String>,
    trace: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct BodyVariant {
    kind: String,
    trace: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ResponseItem {
    id: String,
    trace: Vec<String>,
    consumer: Consumer,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Consumer {
    source: String,
    symbol: String,
}

#[derive(Debug, Deserialize)]
struct Report {
    traces: Vec<Trace>,
    endpoints: Bucket,
    request_semantics: Bucket,
    response_fields: Bucket,
    overall: Totals,
}

#[derive(Debug, Deserialize)]
struct Bucket {
    items: Vec<ReportedItem>,
    covered: usize,
    uncovered: usize,
    total: usize,
    fraction: String,
    uncovered_items: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ReportedItem {
    id: String,
    covered: bool,
    trace: Vec<String>,
    #[serde(default)]
    parameters: Vec<SemanticParameter>,
    consumer: Option<Consumer>,
    evidence: Vec<Evidence>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Evidence {
    kind: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct Totals {
    covered: usize,
    uncovered: usize,
    total: usize,
    fraction: String,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn contract() -> Contract {
    serde_json::from_str(CONTRACT).expect("production coverage contract must be valid JSON")
}

fn normalized_endpoint(path: &str) -> String {
    let path = path
        .split('?')
        .next()
        .expect("trace URL has a path component");
    if path.starts_with("/solr/admin/") {
        path.to_owned()
    } else {
        let suffix = path
            .strip_prefix("/solr/search_api_capture/")
            .unwrap_or_else(|| panic!("trace URL must address the captured core: {path}"));
        format!("/solr/{{core}}/{suffix}")
    }
}

fn capture(trace: &Trace) -> Value {
    let path = root().join(TRACE_DIR).join(&trace.file);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn observed_parameters(traces: &[Trace]) -> BTreeMap<String, BTreeMap<String, BTreeSet<String>>> {
    let mut observed = BTreeMap::new();
    for trace in traces {
        let capture = capture(trace);
        let request = capture["request"]
            .as_object()
            .expect("trace request object");
        let path = request["path"].as_str().expect("trace request URL");
        let query = path.split_once('?').map_or("", |(_, query)| query);
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (raw_name, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
            let name = percent_decode(raw_name);
            let value = percent_decode(raw_value);
            observed
                .entry(name)
                .or_insert_with(BTreeMap::new)
                .entry(value)
                .or_insert_with(BTreeSet::new)
                .insert(trace.file.clone());
        }

        let body = request["body"].as_str().expect("trace request body string");
        if !body.is_empty() {
            // Parse every nonempty body. serde_json intentionally accepts the
            // duplicate-key object in 00001, which is why the raw-key guard
            // below is separate and keeps that variant uncovered.
            let _: Value = serde_json::from_str(body)
                .unwrap_or_else(|e| panic!("{} request body must be JSON: {e}", trace.file));
        }
    }
    observed
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

fn response_has(trace: &Trace, pointer: &str) -> bool {
    let capture = capture(trace);
    let body = capture["response"]["body"]
        .as_str()
        .expect("trace response body string");
    let response: Value = serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("{} response body must be JSON: {e}", trace.file));
    response.pointer(pointer).is_some()
}

fn expected_parameter_map(
    contract: &Contract,
) -> BTreeMap<String, BTreeMap<String, BTreeSet<String>>> {
    contract
        .captured_parameters
        .iter()
        .map(|parameter| {
            let values = parameter
                .occurrences
                .iter()
                .map(|occurrence| {
                    (
                        occurrence.value.clone(),
                        occurrence.trace.iter().cloned().collect::<BTreeSet<_>>(),
                    )
                })
                .collect();
            (parameter.name.clone(), values)
        })
        .collect()
}

fn assert_bucket(
    name: &str,
    actual: Bucket,
    expected: Vec<Item>,
    evidence_kind: &str,
    expected_parameters: Option<&[SemanticItem]>,
    expected_consumers: Option<&[ResponseItem]>,
    expected_uncovered: &[&str],
) -> (usize, usize) {
    let actual_items = actual
        .items
        .iter()
        .map(|item| Item {
            id: item.id.clone(),
            trace: item.trace.clone(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual_items, expected,
        "{name} IDs and classifications must be stable"
    );

    for item in &actual.items {
        assert!(
            item.evidence
                .iter()
                .any(|e| e.kind == evidence_kind && !e.source.is_empty()),
            "{name} item `{}` must cite live {evidence_kind} evidence",
            item.id
        );
        if let Some(semantic_items) = expected_parameters {
            let expected = semantic_items
                .iter()
                .find(|expected| expected.id == item.id)
                .expect("reported semantic item is in the contract");
            assert_eq!(
                item.parameters, expected.parameters,
                "semantic item `{}` must expose exact parameter/value/variant provenance",
                item.id
            );
        }
    }
    if let Some(response_items) = expected_consumers {
        for item in &actual.items {
            let expected = response_items
                .iter()
                .find(|expected| expected.id == item.id)
                .expect("reported response item is in the contract");
            assert_eq!(
                item.consumer.as_ref(),
                Some(&expected.consumer),
                "response item `{}` must retain audited client-consumption provenance",
                item.id
            );
        }
    }

    let mut uncovered_items = expected_uncovered
        .iter()
        .map(|id| (*id).to_string())
        .collect::<Vec<_>>();
    uncovered_items.sort();
    let uncovered = uncovered_items.len();
    let covered = expected.len() - uncovered;
    assert_eq!(actual.covered, covered, "{name} covered subtotal");
    assert_eq!(actual.uncovered, uncovered, "{name} uncovered subtotal");
    assert_eq!(actual.total, expected.len(), "{name} denominator subtotal");
    assert_eq!(
        actual.fraction,
        format!("{covered}/{}", expected.len()),
        "{name} fraction"
    );
    assert_eq!(
        actual.uncovered_items, uncovered_items,
        "{name} complete uncovered output"
    );
    (covered, uncovered)
}

#[tokio::test]
async fn frozen_capture_exhaustively_maps_all_urls_bodies_parameters_and_material_variants() {
    let contract = contract();
    assert_eq!(contract.traces.len(), 28, "#55 froze exactly 28 exchanges");

    let mut files = std::fs::read_dir(root().join(TRACE_DIR))
        .expect("read trace directory")
        .map(|entry| {
            entry
                .expect("read trace entry")
                .file_name()
                .into_string()
                .expect("UTF-8 trace name")
        })
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(
        files,
        contract
            .traces
            .iter()
            .map(|trace| trace.file.clone())
            .collect::<Vec<_>>()
    );

    let manifest =
        std::fs::read_to_string(root().join(TRACE_MANIFEST)).expect("read trace manifest");
    let rows = manifest
        .lines()
        .skip(1)
        .map(|line| line.split('\t').collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        rows.len(),
        28,
        "manifest must account for every frozen exchange"
    );
    for (trace, row) in contract.traces.iter().zip(rows) {
        let capture = capture(trace);
        assert_eq!(row[0], trace.file);
        assert_eq!(row[1], trace.seq.to_string());
        assert_eq!(row[2], trace.method);
        assert_eq!(normalized_endpoint(row[3]), trace.endpoint);
        assert_eq!(capture["seq"], trace.seq);
        assert_eq!(capture["request"]["method"], trace.method);
        assert_eq!(
            normalized_endpoint(capture["request"]["path"].as_str().expect("request path")),
            trace.endpoint
        );
    }

    let observed = observed_parameters(&contract.traces);
    assert_eq!(
        observed.len(),
        43,
        "every distinct captured request parameter name is counted"
    );
    assert_eq!(
        expected_parameter_map(&contract),
        observed,
        "parameter occurrence provenance must be exhaustive"
    );

    let mapped_names = contract
        .request_semantics
        .iter()
        .flat_map(|semantic| {
            semantic
                .parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        mapped_names,
        observed.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        "every captured parameter name must map to one or more semantic denominator items"
    );
    for semantic in &contract.request_semantics {
        assert!(
            !semantic.parameters.is_empty() || !semantic.body_variants.is_empty(),
            "semantic `{}` must have parameter or body provenance",
            semantic.id
        );
        for parameter in &semantic.parameters {
            assert!(
                !parameter.variant.is_empty(),
                "{} needs a material variant label",
                parameter.name
            );
            let occurrences = observed
                .get(&parameter.name)
                .unwrap_or_else(|| panic!("unknown captured parameter {}", parameter.name));
            assert!(
                !parameter.values.is_empty(),
                "semantic `{}` must bind variant `{}` to captured values",
                semantic.id,
                parameter.variant
            );
            for value in &parameter.values {
                let traces = occurrences.get(value).unwrap_or_else(|| {
                    panic!(
                        "semantic `{}` cites absent value {:?} for parameter {}",
                        semantic.id, value, parameter.name
                    )
                });
                assert!(
                    parameter.trace.iter().any(|trace| traces.contains(trace)),
                    "semantic `{}` cites {}/{value:?} but not one of its actual trace occurrences",
                    semantic.id,
                    parameter.name,
                );
            }
            for trace in &parameter.trace {
                assert!(
                    parameter
                        .values
                        .iter()
                        .any(|value| occurrences[value].contains(trace)),
                    "semantic `{}` cites {}/{} without one of its variant values",
                    semantic.id,
                    parameter.name,
                    trace,
                );
            }
        }
        let semantic_trace = semantic
            .parameters
            .iter()
            .flat_map(|parameter| parameter.trace.iter())
            .chain(
                semantic
                    .body_variants
                    .iter()
                    .flat_map(|variant| variant.trace.iter()),
            )
            .collect::<BTreeSet<_>>();
        assert_eq!(
            semantic.trace.iter().collect::<BTreeSet<_>>(),
            semantic_trace,
            "semantic `{}` must cite every parameter/body occurrence it describes",
            semantic.id,
        );
    }

    for required in [
        "select.q.local-params-edismax",
        "select.rows.zero",
        "select.highlight.require-field-match",
        "select.highlight.merge-contiguous",
        "select.spellcheck.dictionaries",
        "mlt.pagination.start-and-rows",
        "mlt.maxntp",
        "mlt.match-include-and-offset",
        "request.json-nl.repeated-map-and-flat",
    ] {
        assert!(
            contract
                .request_semantics
                .iter()
                .any(|item| item.id == required),
            "material variant `{required}` must remain visible"
        );
    }

    let update = contract
        .traces
        .iter()
        .find(|trace| trace.file == "00001.json")
        .expect("update trace");
    let update_capture = capture(update);
    let update_body = update_capture["request"]["body"]
        .as_str()
        .expect("update body");
    assert!(
        update_body.match_indices("\"add\"").count() > 1,
        "00001 must retain duplicate JSON add keys"
    );
    let duplicate = contract
        .request_semantics
        .iter()
        .find(|item| item.id == "update.json-command-add-batch")
        .expect("duplicate-add semantic");
    assert!(
        wayfinder::coverage_report().await["request_semantics"]["uncovered_items"]
            .as_array()
            .expect("coverage report uncovered semantic list")
            .iter()
            .any(|item| item == "update.json-command-add-batch"),
        "duplicate-key adds remain uncovered until the Value parse path is replaced"
    );
    assert_eq!(
        duplicate.body_variants[0].kind,
        "json-object-duplicate-add-key"
    );
}

#[test]
fn json_nl_flat_semantic_has_complete_flat_value_provenance() {
    let contract = contract();
    let flat_occurrence = contract
        .captured_parameters
        .iter()
        .find(|parameter| parameter.name == "json.nl")
        .and_then(|parameter| {
            parameter
                .occurrences
                .iter()
                .find(|occurrence| occurrence.value == "flat")
        })
        .expect("captured json.nl=flat occurrence");
    let semantic = contract
        .request_semantics
        .iter()
        .find(|semantic| semantic.id == "request.json-nl.flat")
        .expect("json.nl flat semantic");
    assert_eq!(semantic.trace, flat_occurrence.trace);
    assert_eq!(semantic.parameters[0].trace, flat_occurrence.trace);
    assert!(
        semantic.trace.iter().any(|trace| trace == "00022.json"),
        "the MLT json.nl=flat exchange must be evaluated"
    );
}

#[test]
fn response_denominator_has_precise_search_api_solr_client_consumption_citations() {
    let contract = contract();
    // Corrected premise: responseHeader.status, response.start, response.maxScore,
    // and response.numFoundExact are emitted in traces but have no direct
    // search_api_solr 4.4.0 consumer in the captured paths, so they are not
    // denominator fields. The remaining 15 each carry a source+method citation.
    assert_eq!(contract.response_fields.len(), 15);
    for field in &contract.response_fields {
        assert!(
            field
                .consumer
                .source
                .starts_with("vendor/drupal/search_api_solr/src/")
        );
        assert!(field.consumer.source.ends_with(".php"));
        assert!(field.consumer.symbol.contains("::"));
        assert!(
            !field.trace.is_empty(),
            "{} needs trace provenance",
            field.id
        );
    }
    for absent in [
        "update.responseHeader.status",
        "select.response.start",
        "select.response.maxScore",
        "select.response.numFoundExact",
    ] {
        assert!(
            !contract
                .response_fields
                .iter()
                .any(|field| field.id == absent),
            "emitted-only field `{absent}` must not inflate client-consumed coverage"
        );
    }
    for (id, pointer) in [
        ("select.response.numFound", "/response/numFound"),
        ("select.response.docs", "/response/docs"),
        ("select.response.docs.score", "/response/docs/0/score"),
        ("select.highlighting", "/highlighting"),
        ("select.facet_counts", "/facet_counts"),
        (
            "select.facet_counts.facet_fields",
            "/facet_counts/facet_fields",
        ),
        ("select.spellcheck.suggestions", "/spellcheck/suggestions"),
        ("select.spellcheck.collations", "/spellcheck/collations"),
        ("mlt.response", "/response"),
        (
            "admin.info-system.lucene.solr-spec-version",
            "/lucene/solr-spec-version",
        ),
        ("admin.system.core.schema", "/core/schema"),
        ("schema.fieldtypes.fieldTypes", "/fieldTypes"),
        ("admin.luke.index", "/index"),
        ("admin.mbeans.solr-mbeans", "/solr-mbeans"),
        ("terms.terms", "/terms"),
    ] {
        let field = contract
            .response_fields
            .iter()
            .find(|field| field.id == id)
            .unwrap_or_else(|| panic!("missing response field {id}"));
        assert!(
            field.trace.iter().any(|file| {
                let trace = contract
                    .traces
                    .iter()
                    .find(|trace| trace.file == *file)
                    .expect("field provenance names a known trace");
                response_has(trace, pointer)
            }),
            "{id} must be emitted by a cited frozen response"
        );
    }
}

async fn strict_indexed_app() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let schema = dir.path().join("schema.toml");
    let config = dir.path().join("wayfinder.toml");
    let data = dir.path().join("data");
    std::fs::write(&schema, common::SCHEMA_TOML).expect("write schema");
    std::fs::write(&config, "strict_params = true\n").expect("write config");
    std::fs::create_dir_all(&data).expect("create data dir");
    let app = wayfinder::app_with_config(&schema, &data, &config).expect("build strict app");
    let (status, body) = common::post_docs(&app, &common::corpus()).await;
    assert_eq!(status, axum::http::StatusCode::OK, "index corpus: {body}");
    (app, dir)
}

#[tokio::test]
async fn classification_guards_exercise_real_router_strict_param_and_renderer_behavior() {
    let (app, _dir) = strict_indexed_app().await;

    let (status, body) = common::get(&app, "select?q=*:*&hl=true&hl.fl=body&fl=id,score").await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "implemented strict params must reach select: {body}"
    );
    assert!(body.pointer("/response/numFound").is_some());
    assert!(body.pointer("/response/docs/0/score").is_some());
    assert!(body.pointer("/highlighting").is_some());

    let (status, body) = common::get(&app, "select?q=*:*&hl.requireFieldMatch=false").await;
    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "unsupported strict parameter must be rejected: {body}"
    );
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(400)
    );

    let (status, body) =
        common::request_full(&app, "POST", "content/update?json.nl=flat", Some("[]")).await;
    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "update must preserve its strict-parameter surface: {body}"
    );

    let (status, body) =
        common::request_full(&app, "GET", "content/terms?terms=true&terms.fl=body", None).await;
    assert_eq!(
        status,
        axum::http::StatusCode::NOT_FOUND,
        "unrouted terms endpoint must remain uncovered: {body}"
    );

    let (status, body) = common::get(&app, "mlt?q=id:doc1&mlt.fl=body").await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "MLT route must render a response envelope: {body}"
    );
    assert!(
        body.pointer("/response").is_some_and(Value::is_object),
        "MLT response renderer must remain live: {body}"
    );

    let duplicate = r#"{"add":{"doc":{"id":"first","body":"first"}},"add":{"doc":{"id":"second","body":"second"}},"commit":{}}"#;
    let (status, body) =
        common::request_full(&app, "POST", "content/update?commit=true", Some(duplicate)).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "raw duplicate-key update is accepted by JSON parser: {body}"
    );
    let (_, first) = common::get(&app, "select?q=id:first").await;
    let (_, second) = common::get(&app, "select?q=id:second").await;
    assert_eq!(
        first.pointer("/response/numFound").and_then(Value::as_u64),
        Some(0),
        "Value parsing must currently lose the first duplicate add"
    );
    assert_eq!(
        second.pointer("/response/numFound").and_then(Value::as_u64),
        Some(1)
    );
}

#[test]
fn coverage_command_requires_complete_deterministic_contract_schema_and_output() {
    let expected = contract();
    let binary = env!("CARGO_BIN_EXE_wayfinder");
    eprintln!("coverage reporting command: {binary} coverage --format json");
    let first = Command::new(binary)
        .args(["coverage", "--format", "json"])
        .output()
        .expect("run coverage command");
    assert!(
        first.status.success(),
        "coverage command must not need a schema, network, or Docker: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = Command::new(binary)
        .args(["coverage", "--format", "json"])
        .output()
        .expect("rerun coverage command");
    assert!(second.status.success());
    assert_eq!(
        first.stdout, second.stdout,
        "coverage output must be byte-for-byte deterministic"
    );

    let report: Report = serde_json::from_slice(&first.stdout).unwrap_or_else(|e| {
        panic!(
            "coverage CLI lacks the required complete contract schema: {e}; stdout:\n{}",
            String::from_utf8_lossy(&first.stdout)
        )
    });
    eprintln!(
        "--- Search API coverage report ---\n{}",
        String::from_utf8_lossy(&first.stdout)
    );
    assert_eq!(
        report.traces, expected.traces,
        "all 28 traces must be reported"
    );
    let endpoint_items = expected.endpoints.clone();
    let semantic_items = expected
        .request_semantics
        .iter()
        .map(|item| Item {
            id: item.id.clone(),
            trace: item.trace.clone(),
        })
        .collect();
    let response_items = expected
        .response_fields
        .iter()
        .map(|item| Item {
            id: item.id.clone(),
            trace: item.trace.clone(),
        })
        .collect();
    let (ec, eu) = assert_bucket(
        "endpoints",
        report.endpoints,
        endpoint_items,
        "route",
        None,
        None,
        &[
            "GET /solr/{core}/schema/fieldtypes",
            "GET /solr/{core}/admin/luke",
            "GET /solr/{core}/admin/mbeans",
            "GET /solr/{core}/terms",
        ],
    );
    let (sc, su) = assert_bucket(
        "request semantics",
        report.request_semantics,
        semantic_items,
        "runtime-probe",
        Some(&expected.request_semantics),
        None,
        &[
            "update.json-command-add-batch",
            "request.omitHeader",
            "request.json-nl.flat",
            "request.json-nl.repeated-map-and-flat",
            "request.timezone.utc",
            "select.q.local-params-edismax",
            "select.highlight.wildcard-fields",
            "select.highlight.require-field-match",
            "select.highlight.merge-contiguous",
            "select.facet.local-key",
            "select.facet.per-field-missing",
            "select.spellcheck.enable",
            "select.spellcheck.query",
            "select.spellcheck.dictionaries",
            "select.spellcheck.collate",
            "mlt.fl.wildcard-plus-score",
            "mlt.filters",
            "mlt.maxntp",
            "mlt.match-include-and-offset",
            "admin.mbeans.stats",
            "terms.enumeration",
        ],
    );
    let (rc, ru) = assert_bucket(
        "response fields",
        report.response_fields,
        response_items,
        "runtime-probe",
        None,
        Some(&expected.response_fields),
        &[
            "select.spellcheck.suggestions",
            "select.spellcheck.collations",
            "schema.fieldtypes.fieldTypes",
            "admin.luke.index",
            "admin.mbeans.solr-mbeans",
            "terms.terms",
        ],
    );
    let covered = ec + sc + rc;
    let total = covered + eu + su + ru;
    assert_eq!(report.overall.covered, covered);
    assert_eq!(report.overall.uncovered, total - covered);
    assert_eq!(report.overall.total, total);
    assert_eq!(report.overall.fraction, format!("{covered}/{total}"));
    assert_eq!(
        report.overall.fraction, "41/72",
        "initial coverage fraction"
    );
}
