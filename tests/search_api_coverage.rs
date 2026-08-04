//! Search API coverage denominator contract (#56).
//!
//! The production-owned derived contract is frozen from issue #55's 28 traces.
//! `wayfinder coverage --format json` must emit its complete, deterministic
//! denominator, live numerator evidence, explicit fractions, and all uncovered
//! IDs. The test independently reparses every request URL and body so a
//! hand-written or incomplete contract cannot pass.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// The overall coverage fraction every test in this file pins.
///
/// Single-sourced deliberately. Three separate tests assert it, and when it
/// used to be three separate literals every feature branch that moved the
/// numerator conflicted with every sibling in three places at once -- four of
/// the ten issues in the #139..#154 batch hit this, and one of them merged a
/// red `main` because a second literal had appeared while the branch was in
/// flight. Landing a feature now edits one line here plus the changelog in
/// `frozen_capture_derived_contract_matches_the_committed_denominator`.
///
/// Derive the new value from `cargo run -- coverage --format json`. Never
/// hand-compute it: the numerator has moved for reasons no ticket named more
/// than once in this repo's history.
const EXPECTED_FRACTION: &str = "75/75";

const CONTRACT: &str = include_str!("../coverage/search_api_coverage_contract.json");
const SOURCE_EVIDENCE: &str =
    include_str!("../coverage/search_api_solr_4.4.0_source_evidence.json");
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
    occurrences: Vec<Occurrence>,
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
    evidence: String,
}

#[derive(Debug, Deserialize)]
struct SourceEvidence {
    upstream: Upstream,
    snapshot_root: String,
    files: Vec<SourceFile>,
    citations: Vec<Citation>,
    exclusions: Vec<Exclusion>,
}

#[derive(Debug, Deserialize)]
struct Upstream {
    project: String,
    tag: String,
    archive_sha256: String,
}

#[derive(Debug, Deserialize)]
struct SourceFile {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Citation {
    id: String,
    source_path: String,
    symbol: String,
    line_start: usize,
    line_end: usize,
    source_sha256: String,
    excerpt_sha256: String,
    consumes: Vec<String>,
    excerpt: String,
}

#[derive(Debug, Deserialize)]
struct Exclusion {
    id: String,
    reason: String,
    evidence: String,
    required_expressions: Vec<String>,
    forbidden_expressions: Vec<String>,
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
    // `path` is the literal URL captured off a real `solr:9` wire exchange
    // (`solr-ref/search-api/trace/`), so the `/solr/` prefixes checked/stripped
    // here are real Solr's, not Wayfinder's — they must not be renamed. The
    // *returned* identifier, though, is compared against `trace.endpoint` from
    // `coverage/search_api_coverage_contract.json`, which `src/coverage.rs`
    // emits under Wayfinder's own `/wayfinder/{core}/...` vocabulary post-#325,
    // so the output side is renamed to match.
    if let Some(suffix) = path.strip_prefix("/solr/admin/") {
        format!("/wayfinder/admin/{suffix}")
    } else {
        let suffix = path
            .strip_prefix("/solr/search_api_capture/")
            .unwrap_or_else(|| panic!("trace URL must address the captured core: {path}"));
        format!("/wayfinder/{{core}}/{suffix}")
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
            // Parse every nonempty body. `serde_json` accepts the
            // duplicate-key object in 00001 but its `Value` map keeps only the
            // last `add`, which is why the raw-key guard below counts the key
            // occurrences in the raw text instead of trusting this parse.
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

fn source_file_paths(root: &Path) -> BTreeSet<String> {
    fn collect(root: &Path, directory: &Path, files: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(directory)
            .unwrap_or_else(|e| panic!("read source snapshot {}: {e}", directory.display()))
        {
            let entry = entry.expect("read source snapshot entry");
            let path = entry.path();
            let file_type = entry
                .file_type()
                .unwrap_or_else(|e| panic!("read source snapshot type {}: {e}", path.display()));
            if file_type.is_dir() {
                collect(root, &path, files);
            } else if file_type.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .expect("source snapshot entry under snapshot root")
                        .to_str()
                        .expect("source snapshot path is UTF-8")
                        .replace('\\', "/"),
                );
            } else {
                panic!(
                    "source snapshot must contain only regular files and directories: {}",
                    path.display()
                );
            }
        }
    }

    let mut files = BTreeSet::new();
    collect(root, root, &mut files);
    files
}

fn source_excerpt(source: &str, line_start: usize, line_end: usize) -> String {
    assert!(line_start > 0 && line_start <= line_end);
    let excerpt = source
        .split_inclusive('\n')
        .skip(line_start - 1)
        .take(line_end - line_start + 1)
        .collect::<String>();
    assert_eq!(
        excerpt.lines().count(),
        line_end - line_start + 1,
        "source range must be present in the immutable snapshot"
    );
    excerpt
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

fn semantic_parameter_map(
    contract: &Contract,
) -> BTreeMap<String, BTreeMap<String, BTreeSet<String>>> {
    let mut mapped = BTreeMap::new();
    for semantic in &contract.request_semantics {
        for parameter in &semantic.parameters {
            for occurrence in &parameter.occurrences {
                let traces = mapped
                    .entry(parameter.name.clone())
                    .or_insert_with(BTreeMap::new)
                    .entry(occurrence.value.clone())
                    .or_insert_with(BTreeSet::new);
                traces.extend(occurrence.trace.iter().cloned());
            }
        }
    }
    mapped
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
    assert_eq!(
        semantic_parameter_map(&contract),
        observed,
        "every captured parameter value and trace occurrence must be represented by a semantic class"
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
            assert_eq!(
                parameter.values.iter().collect::<BTreeSet<_>>(),
                parameter
                    .occurrences
                    .iter()
                    .map(|occurrence| &occurrence.value)
                    .collect::<BTreeSet<_>>(),
                "semantic `{}` must retain per-value occurrence provenance for {}",
                semantic.id,
                parameter.name,
            );
            assert_eq!(
                parameter.trace.iter().collect::<BTreeSet<_>>(),
                parameter
                    .occurrences
                    .iter()
                    .flat_map(|occurrence| occurrence.trace.iter())
                    .collect::<BTreeSet<_>>(),
                "semantic `{}` must retain exact trace provenance for {}",
                semantic.id,
                parameter.name,
            );
            for occurrence in &parameter.occurrences {
                let traces = occurrences.get(&occurrence.value).unwrap_or_else(|| {
                    panic!(
                        "semantic `{}` cites absent value {:?} for parameter {}",
                        semantic.id, occurrence.value, parameter.name
                    )
                });
                assert!(
                    occurrence.trace.iter().all(|trace| traces.contains(trace)),
                    "semantic `{}` cites absent occurrence for {}/{}",
                    semantic.id,
                    parameter.name,
                    occurrence.value,
                );
            }
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
        "select.q.local-params-edismax.and",
        "select.q.local-params-edismax.or",
        "select.q.local-params-edismax.single-term",
        "select.q.match-all",
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
    // Issue #154 replaced the `serde_json::Value` top-level parse (which
    // collapsed duplicate keys to the last occurrence) with a duplicate-
    // tolerant, order-preserving one, so this semantic is now covered.
    assert!(
        !wayfinder::coverage_report().await["request_semantics"]["uncovered_items"]
            .as_array()
            .expect("coverage report uncovered semantic list")
            .iter()
            .any(|item| item == "update.json-command-add-batch"),
        "duplicate-key adds are covered since issue #154; the probe must not regress"
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
        assert_eq!(
            field.consumer.source,
            "coverage/search_api_solr_4.4.0_source_evidence.json"
        );
        assert!(field.consumer.symbol.contains("::"));
        assert!(!field.consumer.evidence.is_empty());
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
            "admin.info-system.lucene.wayfinder-spec-version",
            "/lucene/solr-spec-version",
        ),
        ("admin.system.core.schema", "/core/schema"),
        ("schema.fieldtypes.fieldTypes", "/fieldTypes"),
        ("admin.luke.index", "/index"),
        ("admin.mbeans.wayfinder-mbeans", "/solr-mbeans"),
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

#[test]
fn client_consumption_snapshot_is_hash_pinned_complete_and_auditable() {
    let contract = contract();
    let evidence: SourceEvidence =
        serde_json::from_str(SOURCE_EVIDENCE).expect("valid Search API Solr source evidence");
    assert_eq!(
        evidence.upstream.project,
        "https://git.drupalcode.org/project/search_api_solr"
    );
    assert_eq!(evidence.upstream.tag, "4.4.0");
    assert_eq!(
        evidence.upstream.archive_sha256,
        "5cfcb17d7a325a01eb04f09ca12b6f0d3012ebe0fcfea431ee04a592507c0bce"
    );
    let is_sha256 =
        |digest: &str| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    let sha256 = |text: &str| format!("{:x}", Sha256::digest(text));
    assert!(is_sha256(&evidence.upstream.archive_sha256));
    let snapshot_relative = Path::new(&evidence.snapshot_root);
    assert!(
        snapshot_relative
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "source snapshot root must be a relative normal path"
    );
    let snapshot_root = root().join(snapshot_relative);
    // The three citation-bearing files (the only ones `citations` excerpts)
    // are pinned to these exact 4.4.0 hashes. Issue #368 vendored the full
    // module tree under the snapshot dir, so the manifest now pins ~400 files;
    // the rest are tamper-evident via the `source_file_paths == expected_files`
    // invariant checked below, not by this literal table.
    let pinned_citation_hashes = BTreeMap::from([
        (
            "src/Plugin/search_api/backend/SearchApiSolrBackend.php",
            "587ccd8f3fadb606b6968bc589fd6312e02c4a95e2ee502b380ca6a7241cd21d",
        ),
        (
            "src/SolrConnector/SolrConnectorPluginBase.php",
            "b55ec67468adda7f70061aa8151861c7f9a7c63e680b6c48c6a7379aa9617df0",
        ),
        (
            "src/SolrSpellcheckBackendTrait.php",
            "0238f9e32ecfbe3da160e1a58ad56adade38f3ed8cd27adfc1268cd6c5e53771",
        ),
    ]);
    let actual_hashes = evidence
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    for (path, hash) in pinned_citation_hashes {
        assert_eq!(
            actual_hashes.get(path),
            Some(&hash),
            "the citation-bearing source files must remain pinned to Search \
             API Solr 4.4.0: {path}",
        );
    }
    let expected_files = evidence
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(expected_files.len(), evidence.files.len());
    assert_eq!(source_file_paths(&snapshot_root), expected_files);
    let mut source_text = BTreeMap::new();
    for file in &evidence.files {
        // The snapshot is the full search_api_solr 4.4.0 module tree (#368),
        // not just `src/`; the normal-components check on the next line is the
        // guard against a path escaping the snapshot root.
        assert!(is_sha256(&file.sha256));
        assert!(
            Path::new(&file.path)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "source file path must be a relative normal path"
        );
        let source_path = snapshot_root.join(&file.path);
        let source = std::fs::read_to_string(&source_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", source_path.display()));
        assert_eq!(sha256(&source), file.sha256, "hash for {}", file.path);
        source_text.insert(file.path.clone(), source);
    }
    assert_eq!(
        evidence
            .citations
            .iter()
            .map(|citation| citation.id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        evidence.citations.len(),
        "citation IDs must be unique"
    );
    for citation in &evidence.citations {
        assert!(citation.source_path.starts_with("src/"));
        assert!(citation.line_start <= citation.line_end);
        assert!(is_sha256(&citation.source_sha256));
        assert!(is_sha256(&citation.excerpt_sha256));
        let source = source_text
            .get(&citation.source_path)
            .unwrap_or_else(|| panic!("missing snapshot source {}", citation.source_path));
        assert_eq!(sha256(source), citation.source_sha256);
        assert_eq!(
            citation.excerpt,
            source_excerpt(source, citation.line_start, citation.line_end),
            "{} must be an exact source range",
            citation.id
        );
        assert_eq!(sha256(&citation.excerpt), citation.excerpt_sha256);
        assert!(!citation.excerpt.is_empty());
    }

    let consumed = contract
        .response_fields
        .iter()
        .map(|field| field.id.as_str())
        .collect::<BTreeSet<_>>();
    let cited = evidence
        .citations
        .iter()
        .flat_map(|citation| citation.consumes.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        cited, consumed,
        "every denominator field has one source citation"
    );
    for field in &contract.response_fields {
        let citation = evidence
            .citations
            .iter()
            .find(|citation| citation.id == field.consumer.evidence)
            .unwrap_or_else(|| panic!("missing evidence {}", field.consumer.evidence));
        assert_eq!(citation.symbol, field.consumer.symbol);
        assert!(citation.consumes.contains(&field.id));
    }
    for (citation, needle) in [
        ("backend.extract-results", "getNumFound"),
        ("backend.highlighting", "['highlighting']"),
        ("backend.extract-facets", "getFacetSet"),
        ("spellcheck.suggestions", "COMPONENT_SPELLCHECK"),
        ("backend.search-spellcheck-collation", "getCollation"),
        ("connector.solr-version", "solr-spec-version"),
        ("connector.schema-version", "['core']['schema']"),
        ("backend.schema-field-types", "schema/"),
        ("backend.view-settings-luke", "['index']['numDocs']"),
        ("connector.stats-summary", "['solr-mbeans']"),
        ("backend.autocomplete-terms", "COMPONENT_TERMS"),
    ] {
        assert!(
            evidence
                .citations
                .iter()
                .find(|entry| entry.id == citation)
                .expect("required source excerpt")
                .excerpt
                .contains(needle),
            "{citation} must retain the client-consumption expression"
        );
    }
    let expected_exclusions = BTreeSet::from([
        "update.responseHeader.status",
        "select.response.start",
        "select.response.maxScore",
        "select.response.numFoundExact",
    ]);
    assert_eq!(
        evidence
            .exclusions
            .iter()
            .map(|exclusion| exclusion.id.as_str())
            .collect::<BTreeSet<_>>(),
        expected_exclusions,
        "every emitted-only exclusion needs source-audited evidence"
    );
    for exclusion in &evidence.exclusions {
        assert!(!exclusion.reason.is_empty());
        assert!(!exclusion.required_expressions.is_empty());
        assert!(!exclusion.forbidden_expressions.is_empty());
        let citation = evidence
            .citations
            .iter()
            .find(|citation| citation.id == exclusion.evidence)
            .unwrap_or_else(|| panic!("missing exclusion evidence {}", exclusion.evidence));
        for expression in &exclusion.required_expressions {
            assert!(
                citation.excerpt.contains(expression),
                "{} must retain required expression {expression:?}",
                exclusion.id
            );
        }
        for expression in &exclusion.forbidden_expressions {
            assert!(
                !citation.excerpt.contains(expression),
                "{} must not consume excluded expression {expression:?}",
                exclusion.id
            );
        }
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

    // This half of the guard needs *any* param `SELECT_PARAMS` does not
    // carry; it is about the strict-param rejection path, not about which
    // particular `hl.*` param is unimplemented. It walked through
    // `hl.requireFieldMatch` (issue #139 allowlisted it), then
    // `hl.maxAnalyzedChars` (issue #353 allowlisted it); both became 200s.
    // It now uses `hl.alternateField` -- a real Solr param Wayfinder has no
    // alternate-field highlighting to honour, so it is genuinely unsupported
    // and far out of scope. Move it again if it is ever admitted. The
    // assertions themselves are unchanged.
    let (status, body) = common::get(&app, "select?q=*:*&hl.alternateField=body").await;
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
        axum::http::StatusCode::OK,
        "update must accept json.nl=flat under strict params: {body}"
    );

    // Issue #155 landed the route this used to assert was absent. The guard's
    // point was never the 404 itself -- it was that the coverage classifier
    // reflects the *real* router, so a "covered" endpoint is one a request
    // actually reaches. It now guards the same property from the other side:
    // the route is wired (not a 404), it is reachable under
    // `strict_params = true` with only the params `TERMS_PARAMS` allows, and
    // it renders the `terms` block the `terms.terms` response-field probe
    // classifies as covered.
    let (status, body) =
        common::request_full(&app, "GET", "content/terms?terms=true&terms.fl=body", None).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "routed terms endpoint must be reachable under strict params: {body}"
    );
    assert!(
        body.pointer("/terms/body").is_some_and(Value::is_array),
        "terms renderer must produce the flat term/count array the coverage \
         probes classify as covered: {body}"
    );
    // The other half of "the classifier reflects the router": a param
    // `TERMS_PARAMS` deliberately omits must still 400 under strict params, so
    // no future coverage claim can rest on a param the handler silently
    // ignores. This used `terms.limit` until issue #308 implemented it (and
    // added it to `TERMS_PARAMS`), so it moves to `terms.mincount`, still
    // genuinely unimplemented and listed as such in `TERMS_PARAMS`' ponytail.
    let (status, body) = common::request_full(
        &app,
        "GET",
        "content/terms?terms=true&terms.fl=body&terms.mincount=1",
        None,
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "out-of-scope terms param must remain rejected under strict params: {body}"
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
    // Was `Some(0)` while `parse_update_commands` went through
    // `serde_json::Value`, whose object map collapsed the repeated `add` key
    // to the last occurrence. Issue #154 replaced that top-level parse with a
    // duplicate-tolerant, body-ordered one -- real Solr executes every
    // occurrence (`solr-ref/responses/update_repeated_add_batch.json` and its
    // corpus select, finding 96) -- so BOTH adds must now land. This is the
    // self-expiring guard for that bug: it went red the moment the bug was
    // fixed, which is what it was for.
    assert_eq!(
        first.pointer("/response/numFound").and_then(Value::as_u64),
        Some(1),
        "every repeated `add` must land, not just the last (issue #154)"
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
        &[],
    );
    let (sc, su) = assert_bucket(
        "request semantics",
        report.request_semantics,
        semantic_items,
        "runtime-probe",
        Some(&expected.request_semantics),
        None,
        &[
            // Issues #158/#153: `request.json-nl.repeated-map-and-flat`
            // requires both the captured mbeans route and a discriminating
            // `/select` facet shape. The first of repeated `map,flat` values
            // must win, as pinned by `select_json_nl_repeated_map_flat.json`;
            // route presence alone no longer covers this semantic.
            // Issue #143: "request.omitHeader" and "request.timezone.utc"
            // are resolved -- `omitHeader`/`TZ` are registered in
            // `SELECT_PARAMS`/`MLT_PARAMS` (so `strict_params = true` no
            // longer 400s the exact query shape `search_api_solr` sends on
            // essentially every request), and `/select`/`/mlt` now suppress
            // `responseHeader` when `omitHeader=true`, matching every
            // captured trace that sends it (`solr-ref/search-api/trace/00002`
            // through `00022`). Removed from this list rather than left, per
            // this file's own "self-expiring" convention.
            // Issue #141: "mlt.filters" and "mlt.match-include-and-offset"
            // are resolved -- `/mlt` now honours `fq` (filtering the
            // similar-docs set only, never the seed -- finding 98),
            // `mlt.match.include=false` (dropping the `match` key outright --
            // finding 100) and `mlt.match.offset` (seeding from the nth `q`
            // hit, echoed in `match.start` -- finding 99), with all three
            // registered in `MLT_PARAMS`. Both probes were tightened in the
            // same change from a bare 200 to the response shapes those params
            // actually alter. Removed from this list rather than left, per
            // this file's own "self-expiring" convention.
            //
            // Issue #188: "mlt.fl.wildcard-plus-score" is resolved --
            // `CoreIndex::render_doc` now expands `fl=*` to every stored field
            // (declared and dynamic) and composes it with `score`, on
            // `/select` and `/mlt` alike. Two things had to change together:
            // the predicate, which asserted only that `/response/docs/0/score`
            // existed and so read `select.fl.wildcard-plus-score` as a
            // false-positive green (same class as #162/#167), and the `/mlt`
            // probe's *request*, which omitted `mlt.mintf`/`mlt.mindf` and so
            // read uncovered purely because real Solr's defaults find no
            // similar docs in this corpus (finding 64) -- a reason unrelated
            // to `fl`. Removed from this list rather than left, per this
            // file's own "self-expiring" convention.
            //
            // Issue #189: "mlt.maxntp" is resolved -- `/mlt` now caps
            // analyzer-emitted seed tokens per stored field value before
            // noise-word filtering, matching Lucene's MoreLikeThis behavior.
            // Removed from this list rather than left, per this file's own
            // "self-expiring" convention.
            // Issue #140: "select.facet.per-field-missing" is resolved --
            // `f.<field>.facet.missing` now overrides the global for the field
            // it names (`src/facet.rs`), and `check_params` accepts the
            // `f.<field>.<param>` shape for the base params in
            // `PER_FIELD_PARAMS`, so the probe's query no longer 400s under
            // `strict_params = true`. Its predicate was tightened in the same
            // change to require the trailing `null` bucket rather than the
            // mere presence of the `facet_fields` container. Removed from this
            // list rather than left, per this file's own "self-expiring"
            // convention.
            // Issue #222 resolves all four spellcheck request semantics with
            // the captured empty envelope. See the 69/75 -> 75/75 history
            // below; the six items leave this uncovered list together.
            // "update.json-command-add-batch" left this list when issue #154
            // made the top-level command parse duplicate-key tolerant and
            // body-ordered; the probe's two `numFound == 1` checks now pass.
        ],
    );
    let (rc, ru) = assert_bucket(
        "response fields",
        report.response_fields,
        response_items,
        "runtime-probe",
        None,
        Some(&expected.response_fields),
        &[],
    );
    let covered = ec + sc + rc;
    let total = covered + eu + su + ru;
    assert_eq!(report.overall.covered, covered);
    assert_eq!(report.overall.uncovered, total - covered);
    assert_eq!(report.overall.total, total);
    assert_eq!(report.overall.fraction, format!("{covered}/{total}"));
    // 41/75 -> 42/75 when issue #103 landed real multi-snippet extraction and
    // `select.highlight.snippets` started passing its `hl.snippets=3` probe.
    // Issue #104 sharpened `select.highlight.fragsize`'s probe from a
    // presence-only check to asserting the real captured whole-field text for
    // `hl.fragsize=0`, and taught `src/highlight.rs` to produce it (finding
    // 81) in the same change -- so the entry stays covered and the fraction
    // stays 42/75.
    // 42/75 -> 45/75 when issue #137 landed inline `{!edismax qf='...'}`
    // local-params parsing in `q` (`src/local_params.rs`), flipping all three
    // `select.q.local-params-edismax.{and,or,single-term}` probes at once.
    // Denominator unchanged -- no new semantics, three previously-uncovered
    // ones now answered.
    // 45/75 -> 46/75 when issue #138 taught `facet.field` the `{!key=X}` local
    // key (`split_facet_key` in `src/facet.rs`), so the
    // `select.facet.local-key` probe's `{!key=kind}category` now answers a
    // `facet_counts.facet_fields.kind` bucket instead of a 400. Denominator
    // unchanged -- no new semantics, one previously-uncovered one now
    // answered.
    // 46/75 -> 48/75 when issue #156 implemented `GET
    // /wayfinder/{core}/schema/fieldtypes` (resolving #142 as In), flipping two
    // entries at once: the endpoint itself, now wired in
    // `search_api_routes!`, and the `schema.fieldtypes.fieldTypes` response
    // field, whose probe now gets an array derived from the live
    // `WayfinderSchema`. Denominator unchanged -- the endpoint and the field
    // were already in the contract, just unmet. The rest of Solr's Schema API
    // is deliberately absent from the contract (PRD section 5 parity
    // roadmap), so this does not open a `/schema/*` family.
    // 48/75 -> 50/75 when issue #157 implemented `GET /wayfinder/{core}/admin/luke`
    // (reversing the #57 descope for this endpoint), flipping two entries at
    // once: the endpoint itself, now wired in `search_api_routes!`, and the
    // `admin.luke.index` response field, whose probe now gets an `index{}`
    // object with five real figures --
    // `numDocs`/`maxDoc`/`deletedDocs`/`hasDeletions`/`segmentCount` -- read
    // off the live searcher. Denominator unchanged -- both entries were already in
    // the contract, just unmet. Lucene-identity keys in `index{}` and the
    // per-field `schema`/`index` flag strings stay omitted or placeheld
    // deliberately (PRD section 5 v2.75), which is why the endpoint carries no
    // `manifest.tsv` row and cannot be differentially diffed.
    // 50/75 -> 53/75 when issue #155 landed the TermsComponent endpoint
    // (`GET /wayfinder/{core}/terms`, `terms`/`terms.fl`, the inverted-index term
    // dictionary read in `CoreIndex::field_terms`), flipping three entries in
    // three different buckets at once: the `GET /wayfinder/{core}/terms` route, the
    // `terms.enumeration` request semantic, and the `terms.terms` response
    // field. Denominator unchanged -- no new contract items, three
    // previously-uncovered ones now answered.
    // 53/75 -> 57/75 when issue #158 landed `GET /wayfinder/{core}/admin/mbeans`:
    // the endpoint, `admin.mbeans.stats`, and `admin.mbeans.wayfinder-mbeans`
    // entries the ticket named, PLUS `request.json-nl.repeated-map-and-flat`
    // (not named by the ticket, but its probe is gated on the same route --
    // see the comment on the request-semantics uncovered list above).
    // Denominator unchanged -- four previously-uncovered items now answered.
    // 57/75 -> 59/75 when issue #143 registered `omitHeader`/`TZ` in
    // `SELECT_PARAMS`/`MLT_PARAMS` and taught `/select`/`/mlt` to suppress
    // `responseHeader` on `omitHeader=true`, flipping `request.omitHeader`
    // and `request.timezone.utc`. Denominator unchanged -- both were already
    // in the contract, just unmet.
    // 59/75 -> 62/75 when issue #139 taught `src/highlight.rs` to expand
    // `hl.fl=*` against the schema's highlightable (text-typed) fields
    // rather than 400ing on the literal field name `*`, and added
    // `hl.mergeContiguous`/`hl.requireFieldMatch` to `SELECT_PARAMS` so
    // `strict_params = true` stops rejecting them. Denominator unchanged --
    // `select.highlight.wildcard-fields`, `select.highlight.merge-contiguous`,
    // and `select.highlight.require-field-match` were already in the frozen
    // contract (added ahead of the implementation); three previously-
    // uncovered items now answered.
    // 62/75 -> 63/75 when issue #154 replaced `parse_update_commands`'s
    // `serde_json::Value` top-level parse -- whose object map collapsed a
    // repeated `add` key to the last occurrence -- with a duplicate-tolerant,
    // body-ordered one, so `update.json-command-add-batch`'s two
    // `numFound == 1` checks both pass. Denominator unchanged -- the semantic
    // was already in the contract, just unmet.
    // 63/75 -> 64/75 when issue #140 landed the `f.<field>.facet.missing`
    // per-field override (`PER_FIELD_PARAMS` + the `f.<field>.<param>` shape
    // in `check_params`, and the override resolution in `src/facet.rs`),
    // flipping `select.facet.per-field-missing` -- which had been failing on a
    // `strict_params` 400, not on the counts. Denominator unchanged -- the
    // semantic was already in the contract, just unmet.
    // 64/75 -> 66/75 when issue #141 wired `fq`, `mlt.match.include` and
    // `mlt.match.offset` into `/mlt` (plus `json.nl` through
    // `interestingTerms`) and registered them in `MLT_PARAMS`, flipping
    // `mlt.filters` and `mlt.match-include-and-offset`. Denominator unchanged
    // -- both were already in the frozen contract, just unmet. The issue's
    // other two entries did NOT flip and were descoped:
    // `mlt.fl.wildcard-plus-score` to #188 and `mlt.maxntp` to #189.
    // 66/75 -> 67/75 when issue #188 taught `CoreIndex::render_doc` that `*` in
    // `fl` is a wildcard over every stored field (declared and dynamic),
    // composable with named fields and with `score`, flipping
    // `mlt.fl.wildcard-plus-score` -- whose probe also had to start sending
    // `mlt.mintf=1&mlt.mindf=1`, since without them this corpus has no similar
    // docs at all and the item read uncovered for a reason unrelated to `fl`.
    // `select.fl.wildcard-plus-score` does NOT move the numerator: it was
    // already reading covered, falsely, on a `score`-presence-only predicate,
    // and #188 tightened that predicate to require the expanded fields while
    // making the implementation satisfy it. Denominator unchanged -- both
    // entries were already in the frozen contract.
    // 67/75 -> 68/75 when issue #153 added `json.nl` to `UPDATE_PARAMS`,
    // letting the existing strict `/update?json.nl=flat` probe complete.
    // The request semantic was already in the frozen contract; selection
    // rendering remains covered by the fixture-backed differential suite.
    // 68/75 -> 69/75 when issue #189 implemented `mlt.maxntp` by capping
    // analyzer-emitted seed tokens per stored field value before noise-word
    // filtering, flipping the existing `mlt.maxntp` probe. Denominator
    // unchanged -- the semantic was already in the frozen contract.
    // 69/75 -> 75/75 when issue #222 allowlisted the captured spellcheck
    // params and emitted its empty `suggestions: []`/`collations: []`
    // envelope, flipping four request semantics plus two response fields.
    // This is envelope compatibility only, not spelling correction: real
    // suggestion generation remains PRD v3.
    assert_eq!(
        report.overall.fraction, EXPECTED_FRACTION,
        "initial coverage fraction"
    );
}

/// Issue #162: `admin.luke.index`, `terms.terms`, and
/// `schema.fieldtypes.fieldTypes` each check only that a container exists
/// (`is_object()`/`is_array()`), not that a real client consumer could read
/// anything out of it. Tightening those three predicates to require their
/// real leaf (`index.numDocs` as a u64; a non-empty term/frequency pair; a
/// non-empty name list) must not drop the fraction below `EXPECTED_FRACTION` -- these
/// three items are covered *today*, against the real seeded corpus
/// (`ProbeApp::PROBE_DOCS`) driving the real routed handlers, and a
/// tightened probe must still see real, non-hollow data at each of them.
/// (The pinned figure has since moved with landed features -- see the inline
/// comment on the assertion -- but the guard's job is unchanged.)
///
/// This is deliberately a live regression guard rather than a red test:
/// `src/coverage.rs`'s own `#[cfg(test)]` unit tests already pin the failing
/// half (a hollow container must read as uncovered) with a stub router,
/// since the real app cannot be coaxed into emitting a genuinely hollow
/// container at any of these three paths. What *is* observable against the
/// real app, and worth pinning here, is the inverse hazard named by the
/// task: `terms.terms`'s current probe requests
/// `content/terms?terms=true` with no `terms.fl` (see the doc comment on
/// `src/lib.rs::terms`), which -- by that handler's own documented
/// contract -- returns the hollow `{"terms":{}}` right now. Tightening only
/// the assertion without also pointing the probe's request at a real field
/// (as its sibling `terms.enumeration` request-semantic probe already does
/// with `terms.fl=body`) flips `terms.terms` from covered to uncovered and
/// drops the fraction by one, i.e. one below the `EXPECTED_FRACTION` pinned
/// below. If
/// this test goes red, that is the tightening missing its matching request
/// fix, not a fixture to update.
#[tokio::test]
async fn hollow_container_response_fields_stay_covered_against_the_real_seeded_app() {
    let report = wayfinder::coverage_report().await;
    let items = report["response_fields"]["items"]
        .as_array()
        .expect("response_fields items array");
    for id in [
        "admin.luke.index",
        "terms.terms",
        "schema.fieldtypes.fieldTypes",
    ] {
        let item = items
            .iter()
            .find(|item| item["id"] == id)
            .unwrap_or_else(|| panic!("response_fields item `{id}` present in report"));
        assert_eq!(
            item["covered"],
            Value::Bool(true),
            "`{id}` must remain covered against the real seeded corpus once its \
             probe requires its real leaf value, got item: {item}"
        );
    }
    // The constant tracks landed features; the assertion's job is unchanged.
    // It went 57/75 -> 59/75 when issue #143 landed `omitHeader`/`TZ`, which
    // flipped exactly two *request_semantics* items to covered on their own
    // merits: `request.omitHeader` and `request.timezone.utc`, and 59/75 ->
    // 62/75 when issue #139 landed `hl.fl=*` expansion plus the
    // `hl.mergeContiguous`/`hl.requireFieldMatch` allowlist entries, which
    // flipped three more items in the *request_semantics* and
    // *select.highlight* buckets, and 62/75 -> 63/75 when issue #154 made
    // `/update`'s top-level command parse duplicate-key tolerant, flipping
    // `update.json-command-add-batch` -- another *request_semantics* item.
    // The
    // `response_fields` section this test actually guards is untouched at
    // 13/15 across all of those changes, which is the point -- bumping the
    // number for a feature that genuinely adds coverage elsewhere is correct;
    // bumping it because one of the three probes below stopped reaching real
    // data is the failure this guards, and that would show up as
    // `response_fields` dropping.
    // It went 63/75 -> 64/75 when issue #140 landed the
    // `f.<field>.facet.missing` per-field override, which flipped exactly one
    // further *request_semantics* item on its own merits:
    // `select.facet.per-field-missing`. `response_fields` is again untouched
    // across that change, at 13/15.
    // It went 64/75 -> 66/75 when issue #141 wired
    // `fq`/`mlt.match.include`/`mlt.match.offset` into `/mlt`, flipping
    // `mlt.filters` and `mlt.match-include-and-offset` -- two more
    // *request_semantics* items, `response_fields` still 13/15.
    assert_eq!(
        report["overall"]["fraction"],
        Value::String(EXPECTED_FRACTION.to_string()),
        "tightening the three hollow-container probes must not, by itself, \
         change the coverage fraction -- if it drops, a probe's request (not \
         just its assertion) needs to change to reach real data, see \
         `terms.terms` above"
    );
}

/// Issue #167: `request.json-nl.repeated-map-and-flat`'s probe once checked
/// only HTTP 200 on `content/admin/mbeans?json.nl=flat&json.nl=map`, the same
/// class of bug #162 fixed for three response-field probes. The current probe
/// requires object-shaped `wayfinder-mbeans` and a discriminating `/select` facet
/// leg using `json.nl=map&json.nl=flat`. That tightening must not drop the
/// fraction below `EXPECTED_FRACTION`: neither leg's *request* needs to change to reach
/// real data -- `admin_mbeans` in `src/lib.rs` builds `wayfinder-mbeans`
/// unconditionally, and the seeded probe corpus already facets `category` --
/// so the item stays covered against the real seeded app.
///
/// This is deliberately a live regression guard rather than a red test, for
/// the same reason #162's sibling guard is: `src/coverage.rs`'s own
/// `#[cfg(test)]` unit tests already pin the failing halves (a response
/// missing `wayfinder-mbeans` or shaping it as a non-object; a `category` facet
/// missing or rendered as `json.nl=flat`'s alternating array) with a stub
/// router, since the real app cannot be coaxed into those shapes at these
/// paths.
///
/// What this guard adds over the stubs is that it pins the probe's
/// *requests*, not just its predicate. The stub routers match on path, so
/// they stay green if the probe's query strings are stripped; the real app
/// does not -- drop `json.nl=map&json.nl=flat` from the `/select` leg and
/// `category` comes back as `json.nl`'s default flat array, uncovering the
/// item and dropping the fraction here.
#[tokio::test]
async fn repeated_map_and_flat_stays_covered_against_the_real_seeded_app() {
    let report = wayfinder::coverage_report().await;
    let items = report["request_semantics"]["items"]
        .as_array()
        .expect("request_semantics items array");
    let item = items
        .iter()
        .find(|item| item["id"] == "request.json-nl.repeated-map-and-flat")
        .expect("request.json-nl.repeated-map-and-flat item present in report");
    assert_eq!(
        item["covered"],
        Value::Bool(true),
        "request.json-nl.repeated-map-and-flat must remain covered against the \
         real seeded app once its probe requires `wayfinder-mbeans` to be a JSON \
         object, got item: {item}"
    );
    // As with the sibling guard above, the constant tracks landed features
    // and the assertion's job is unchanged: it went 57/75 -> 59/75 on
    // `origin/main` when issue #143 landed `omitHeader`/`TZ`, which flipped
    // `request.omitHeader` and `request.timezone.utc` on their own merits,
    // and 59/75 -> 62/75 when issue #139 landed `hl.fl=*` expansion plus the
    // `hl.mergeContiguous`/`hl.requireFieldMatch` allowlisting, and 62/75 ->
    // 63/75 when issue #154 made `/update`'s top-level command parse
    // duplicate-key tolerant. None of those changes is what moved it.
    // It went 63/75 -> 64/75 when issue #140 landed the
    // `f.<field>.facet.missing` per-field override, which flipped
    // `select.facet.per-field-missing` on its own merits. That is likewise not
    // this probe's doing.
    // It went 64/75 -> 66/75 when issue #141 wired
    // `fq`/`mlt.match.include`/`mlt.match.offset` into `/mlt`, flipping
    // `mlt.filters` and `mlt.match-include-and-offset`. Also not this probe's
    // doing.
    assert_eq!(
        report["overall"]["fraction"],
        Value::String(EXPECTED_FRACTION.to_string()),
        "tightening this probe's assertion must not, by itself, change the \
         coverage fraction -- the real handler already emits an object-shaped \
         `wayfinder-mbeans` regardless of `json.nl`, so nothing about the real \
         request needs to change to reach it"
    );
}
