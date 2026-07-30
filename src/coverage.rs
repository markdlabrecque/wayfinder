//! Hermetic Search API coverage report for the frozen #55 capture.
//!
//! The production contract supplies the denominator and provenance only. The
//! report calculates classifications from the real router, strict allowlists,
//! and typed renderer/semantic capability surfaces below; it never deserializes
//! or reads a contract `covered` value.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{
    MLT_PARAMS, ROUTES, SELECT_PARAMS, UPDATE_PARAMS,
    update_command_parser_preserves_duplicate_keys,
};

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

fn contains_all(params: &[&str], needed: &[SemanticParameter]) -> bool {
    needed
        .iter()
        .all(|parameter| params.contains(&parameter.name.as_str()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SemanticBehavior {
    DuplicateAddBatch,
    CommitWithin,
    JsonWireFormat,
    JsonNlFlat,
    RepeatedJsonNl,
    PlainQuery,
    LocalParamsEdismax,
    Pagination,
    WildcardFields,
    FilterQuery,
    Sort,
    Highlight,
    HighlightWildcard,
    HighlightMarkers,
    Facet,
    FacetLocalKey,
    PerFieldFacetMissing,
    Spellcheck,
    MltLookup,
    MltTuning,
    MltFilters,
    MltInterestingTerms,
    Unimplemented,
}

impl SemanticBehavior {
    fn supported(self) -> bool {
        match self {
            Self::DuplicateAddBatch => update_command_parser_preserves_duplicate_keys(),
            Self::CommitWithin
            | Self::JsonWireFormat
            | Self::JsonNlFlat
            | Self::PlainQuery
            | Self::Pagination
            | Self::FilterQuery
            | Self::Sort
            | Self::Highlight
            | Self::HighlightMarkers
            | Self::Facet
            | Self::MltLookup
            | Self::MltTuning
            | Self::MltInterestingTerms => true,
            Self::RepeatedJsonNl
            | Self::LocalParamsEdismax
            | Self::WildcardFields
            | Self::HighlightWildcard
            | Self::FacetLocalKey
            | Self::PerFieldFacetMissing
            | Self::Spellcheck
            | Self::MltFilters
            | Self::Unimplemented => false,
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::DuplicateAddBatch => "src/lib.rs::UpdateCommandParser",
            Self::CommitWithin => "src/lib.rs::update -> CoreIndex::schedule_commit",
            Self::JsonWireFormat | Self::JsonNlFlat | Self::RepeatedJsonNl => {
                "src/lib.rs parameter allowlists + response writers"
            }
            Self::PlainQuery | Self::FilterQuery => "src/lib.rs::select -> CoreIndex::parse_query",
            Self::LocalParamsEdismax => "src/lib.rs::select local-param parser",
            Self::Pagination => "src/lib.rs::select/mlt pagination",
            Self::WildcardFields => "src/core_index.rs::render_doc wildcard projection",
            Self::Sort => "src/lib.rs::check_sort",
            Self::Highlight | Self::HighlightMarkers | Self::HighlightWildcard => {
                "src/highlight.rs::highlighting"
            }
            Self::Facet | Self::FacetLocalKey | Self::PerFieldFacetMissing => {
                "src/facet.rs::facet_counts"
            }
            Self::Spellcheck => "src/lib.rs::select spellcheck renderer",
            Self::MltLookup | Self::MltTuning | Self::MltFilters | Self::MltInterestingTerms => {
                "src/lib.rs::mlt -> CoreIndex::mlt_query"
            }
            Self::Unimplemented => "unrouted or unsupported protocol behavior",
        }
    }
}

/// Maps a denominator label to a concrete behavior, rather than treating a
/// parameter name as evidence that every variant of that name is supported.
fn semantic_behavior(id: &str) -> SemanticBehavior {
    match id {
        "update.json-command-add-batch" => SemanticBehavior::DuplicateAddBatch,
        "update.commitWithin" => SemanticBehavior::CommitWithin,
        "request.omitHeader"
        | "request.timezone.utc"
        | "admin.mbeans.stats"
        | "terms.enumeration" => SemanticBehavior::Unimplemented,
        "request.wt.json" => SemanticBehavior::JsonWireFormat,
        "request.json-nl.flat" => SemanticBehavior::JsonNlFlat,
        "request.json-nl.repeated-map-and-flat" => SemanticBehavior::RepeatedJsonNl,
        "select.q.plain-query" => SemanticBehavior::PlainQuery,
        "select.q.local-params-edismax" => SemanticBehavior::LocalParamsEdismax,
        "select.pagination.start-and-rows"
        | "select.rows.zero"
        | "mlt.pagination.start-and-rows" => SemanticBehavior::Pagination,
        "select.fl.wildcard-plus-score" | "mlt.fl.wildcard-plus-score" => {
            SemanticBehavior::WildcardFields
        }
        "select.fq.string"
        | "select.fq.range"
        | "select.fq.boolean"
        | "select.fq.multi-value-or" => SemanticBehavior::FilterQuery,
        "select.sort.integer" | "select.sort.string" | "select.sort.date" => SemanticBehavior::Sort,
        "select.highlight.enabled" | "select.highlight.snippets" | "select.highlight.fragsize" => {
            SemanticBehavior::Highlight
        }
        "select.highlight.wildcard-fields" => SemanticBehavior::HighlightWildcard,
        "select.highlight.require-field-match" | "select.highlight.merge-contiguous" => {
            SemanticBehavior::Unimplemented
        }
        "select.highlight.custom-markers" => SemanticBehavior::HighlightMarkers,
        "select.facet.field"
        | "select.facet.sort-limit-mincount"
        | "select.facet.global-missing" => SemanticBehavior::Facet,
        "select.facet.local-key" => SemanticBehavior::FacetLocalKey,
        "select.facet.per-field-missing" => SemanticBehavior::PerFieldFacetMissing,
        "select.spellcheck.enable"
        | "select.spellcheck.query"
        | "select.spellcheck.dictionaries"
        | "select.spellcheck.collate" => SemanticBehavior::Spellcheck,
        "mlt.base-lookup" => SemanticBehavior::MltLookup,
        "mlt.mintf" | "mlt.mindf" | "mlt.maxqt" | "mlt.boost" => SemanticBehavior::MltTuning,
        "mlt.filters" | "mlt.maxntp" | "mlt.match-include-and-offset" => {
            SemanticBehavior::MltFilters
        }
        "mlt.interesting-terms-none" => SemanticBehavior::MltInterestingTerms,
        _ => panic!("unrecognised Search API semantic denominator item: {id}"),
    }
}

fn semantic_allowlist(id: &str) -> &'static [&'static str] {
    if id.starts_with("update.") {
        UPDATE_PARAMS
    } else if id.starts_with("select.") {
        SELECT_PARAMS
    } else if id.starts_with("mlt.") {
        MLT_PARAMS
    } else if id == "request.wt.json" {
        // Every routed Search API endpoint accepts `wt` through one of these
        // real strict allowlists; unavailable endpoints do not turn this wire
        // format variant into an endpoint numerator.
        &["wt"]
    } else if id.starts_with("request.json-nl.") {
        &["json.nl"]
    } else {
        &[]
    }
}

fn semantic_covered(item: &SemanticItem) -> bool {
    let behavior = semantic_behavior(&item.id);
    contains_all(semantic_allowlist(&item.id), &item.parameters) && behavior.supported()
}

/// Client-consumed fields emitted by real response writers. Each variant is
/// used at its insertion site, so the coverage numerator cannot be made green
/// by a separate, hand-maintained support list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderedResponseField {
    SelectNumFound,
    SelectDocs,
    SelectDocsScore,
    SelectHighlighting,
    SelectFacetCounts,
    SelectFacetFields,
    MltResponse,
    AdminInfoSolrSpecVersion,
    CoreAdminSchema,
}

impl RenderedResponseField {
    const ALL: &[Self] = &[
        Self::SelectNumFound,
        Self::SelectDocs,
        Self::SelectDocsScore,
        Self::SelectHighlighting,
        Self::SelectFacetCounts,
        Self::SelectFacetFields,
        Self::MltResponse,
        Self::AdminInfoSolrSpecVersion,
        Self::CoreAdminSchema,
    ];

    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::SelectNumFound => "select.response.numFound",
            Self::SelectDocs => "select.response.docs",
            Self::SelectDocsScore => "select.response.docs.score",
            Self::SelectHighlighting => "select.highlighting",
            Self::SelectFacetCounts => "select.facet_counts",
            Self::SelectFacetFields => "select.facet_counts.facet_fields",
            Self::MltResponse => "mlt.response",
            Self::AdminInfoSolrSpecVersion => "admin.info-system.lucene.solr-spec-version",
            Self::CoreAdminSchema => "admin.system.core.schema",
        }
    }

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::SelectNumFound => "numFound",
            Self::SelectDocs => "docs",
            Self::SelectDocsScore => "score",
            Self::SelectHighlighting => "highlighting",
            Self::SelectFacetCounts => "facet_counts",
            Self::SelectFacetFields => "facet_fields",
            Self::MltResponse => "response",
            Self::AdminInfoSolrSpecVersion => "solr-spec-version",
            Self::CoreAdminSchema => "schema",
        }
    }
}

fn response_field(id: &str) -> Option<RenderedResponseField> {
    RenderedResponseField::ALL
        .iter()
        .copied()
        .find(|field| field.id() == id)
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
pub fn report() -> serde_json::Value {
    let contract = contract();
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
    let request_semantics = bucket(
        contract
            .request_semantics
            .iter()
            .map(|item| {
                let behavior = semantic_behavior(&item.id);
                ReportedItem {
                    id: item.id.clone(),
                    covered: semantic_covered(item),
                    trace: item.trace.clone(),
                    parameters: item.parameters.clone(),
                    consumer: None,
                    evidence: vec![Evidence {
                        kind: "strict-param",
                        source: behavior.source().to_string(),
                    }],
                }
            })
            .collect(),
    );
    let response_fields = bucket(
        contract
            .response_fields
            .iter()
            .map(|item| {
                let field = response_field(&item.id);
                ReportedItem {
                    id: item.id.clone(),
                    covered: field.is_some(),
                    trace: item.trace.clone(),
                    parameters: Vec::new(),
                    consumer: Some(item.consumer.clone()),
                    evidence: vec![Evidence {
                        kind: "rendered-response",
                        source: field
                            .map(|field| format!("src::RenderedResponseField::{field:?}"))
                            .unwrap_or_else(|| "no routed response renderer".to_string()),
                    }],
                }
            })
            .collect(),
    );
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
    fn rendered_response_surface_excludes_unimplemented_fields() {
        assert_eq!(
            response_field("mlt.response"),
            Some(RenderedResponseField::MltResponse)
        );
        assert_eq!(response_field("select.spellcheck.suggestions"), None);
    }

    #[test]
    fn duplicate_add_behavior_tracks_the_live_parser_surface() {
        assert!(!SemanticBehavior::DuplicateAddBatch.supported());
        assert!(!update_command_parser_preserves_duplicate_keys());
    }

    #[test]
    fn contract_rejects_manual_coverage_classifications() {
        let mut contract: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
        contract["request_semantics"][0]["covered"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Contract>(contract).is_err());
    }
}
