//! Endpoint provenance must cover every frozen capture exchange (#56).

use std::collections::BTreeMap;

use serde::Deserialize;

const CONTRACT: &str = include_str!("../coverage/search_api_coverage_contract.json");

#[derive(Deserialize)]
struct Contract {
    traces: Vec<Trace>,
    endpoints: Vec<Endpoint>,
}

#[derive(Deserialize)]
struct Trace {
    file: String,
    method: String,
    endpoint: String,
}

#[derive(Deserialize)]
struct Endpoint {
    id: String,
    trace: Vec<String>,
}

#[test]
fn each_endpoint_cites_every_frozen_exchange_with_its_method_and_shape() {
    let contract: Contract = serde_json::from_str(CONTRACT).expect("valid coverage contract");
    let expected = contract.traces.iter().fold(
        BTreeMap::<String, Vec<String>>::new(),
        |mut traces, trace| {
            traces
                .entry(format!("{} {}", trace.method, trace.endpoint))
                .or_default()
                .push(trace.file.clone());
            traces
        },
    );

    for endpoint in contract.endpoints {
        assert_eq!(
            endpoint.trace, expected[&endpoint.id],
            "endpoint `{}` must cite every frozen exchange with its method and normalized shape",
            endpoint.id
        );
    }
}
