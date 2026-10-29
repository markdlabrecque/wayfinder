//! Scoring wiring (issues #452, #482): `score::ratios` turns the two engines'
//! result JSON into one Wayfinder/Solr ratio per metric. It pairs query, cold
//! and load rows by position, so a row one engine lacks must be an error, not
//! a silently shorter score.

use serde_json::{Value, json};
use wayfinder_bench::score::ratios;

fn query(name: &str, p50: f64) -> Value {
    json!({
        "query": name, "cold_ms": 10.0, "p50": p50, "p95": 20.0, "p99": 30.0,
        "cpu_ms_per_1k": 5.0, "repeat_p50": 1.0,
    })
}

fn cold(p50: f64) -> Value {
    json!({"p50": p50, "p95": 8.0, "p99": 9.0})
}

fn load(rps: f64, p50: f64) -> Value {
    json!({"rps": rps, "p50": p50, "p95": 6.0, "p99": 7.0})
}

/// One engine's result: two query shapes, two cold shapes, two load levels.
fn engine(p50: f64, cold_p50: f64, rps: f64, load_p50: f64, size_mb: f64) -> Value {
    json!({
        "queries": [query("term", p50), query("phrase", p50 * 2.0)],
        "cold": [cold(cold_p50), cold(cold_p50)],
        "load": [load(rps, load_p50), load(rps, load_p50)],
        "startup_ms": 100.0,
        "index_size_mb": size_mb,
        "mem_idle_mb": 50.0,
        "mem_post_index_mb": 60.0,
        "mem_peak_mb": 70.0,
        "index_docs_per_s": 1000.0,
        "add_to_searchable": {"p50_ms": 40.0, "p95_ms": 80.0},
    })
}

fn sample_wf() -> Value {
    engine(10.0, 4.0, 200.0, 3.0, 30.0)
}

fn sample_solr() -> Value {
    engine(20.0, 8.0, 100.0, 6.0, 60.0)
}

fn values(r: &[(&str, f64)], name: &str) -> Vec<f64> {
    r.iter()
        .filter(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .collect()
}

#[test]
fn each_metric_is_wayfinder_over_solr_and_throughput_is_inverted() {
    let r = ratios(&sample_wf(), &sample_solr()).expect("ratios");
    assert_eq!(values(&r, "p50"), vec![0.5, 0.5], "one per query shape");
    assert_eq!(values(&r, "first_ms"), vec![1.0, 1.0], "cold_ms feeds first_ms");
    assert_eq!(values(&r, "cold_p50"), vec![0.5, 0.5]);
    assert_eq!(values(&r, "load_p50"), vec![0.5, 0.5], "one per load level");
    assert_eq!(values(&r, "load_rps"), vec![0.5, 0.5], "solr/wf: 200 vs 100");
    assert_eq!(values(&r, "index_size_mb"), vec![0.5]);
    assert_eq!(values(&r, "index_docs_per_s"), vec![1.0]);
    assert_eq!(values(&r, "add_to_searchable_p50_ms"), vec![1.0]);
}

#[test]
fn query_rows_are_paired_in_order() {
    let mut wf = sample_wf();
    wf["queries"] = json!([query("term", 10.0), query("phrase", 40.0)]);
    let mut solr = sample_solr();
    solr["queries"] = json!([query("term", 20.0), query("phrase", 20.0)]);
    let r = ratios(&wf, &solr).expect("ratios");
    assert_eq!(values(&r, "p50"), vec![0.5, 2.0]);
}

#[test]
fn a_metric_renamed_in_the_results_is_an_error_naming_it() {
    let mut wf = sample_wf();
    let v = wf["queries"][1]["p95"].take();
    wf["queries"][1]["p95_ms"] = v;
    let err = ratios(&wf, &sample_solr()).unwrap_err();
    assert!(err.contains("p95"), "{err}");

    let mut solr = sample_solr();
    let v = solr["index_size_mb"].take();
    solr["index_size"] = v;
    let err = ratios(&sample_wf(), &solr).unwrap_err();
    assert!(err.contains("index_size_mb"), "{err}");
}

#[test]
fn a_row_missing_from_either_engine_is_an_error_not_a_skip() {
    for key in ["queries", "cold", "load"] {
        let mut short = sample_wf();
        short[key].as_array_mut().unwrap().pop();
        let err = ratios(&short, &sample_solr()).unwrap_err();
        assert!(err.contains(key), "wayfinder short on {key}: {err}");
        let err = ratios(&sample_wf(), &short).unwrap_err();
        assert!(err.contains(key), "solr short on {key}: {err}");
    }
}

#[test]
fn an_extra_row_on_one_engine_is_an_error_not_ignored() {
    for key in ["queries", "cold", "load"] {
        let mut long = sample_wf();
        let extra = long[key][0].clone();
        long[key].as_array_mut().unwrap().push(extra);
        let err = ratios(&long, &sample_solr()).unwrap_err();
        assert!(err.contains(key), "wayfinder long on {key}: {err}");
        let err = ratios(&sample_wf(), &long).unwrap_err();
        assert!(err.contains(key), "solr long on {key}: {err}");
    }
}

#[test]
fn a_section_missing_entirely_is_an_error() {
    let mut wf = sample_wf();
    wf.as_object_mut().unwrap().remove("load");
    assert!(ratios(&wf, &sample_solr()).is_err());
}

#[test]
fn a_zero_measurement_is_an_error_naming_the_metric() {
    let mut wf = sample_wf();
    wf["queries"][0]["p99"] = json!(0.0);
    let err = ratios(&wf, &sample_solr()).unwrap_err();
    assert!(err.contains("p99"), "{err}");
}
