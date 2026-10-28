//! The one score the tuning loop chases (issue #452): a weighted geometric
//! mean of per-metric wf/solr ratios, below 1.0 when Wayfinder wins, with the
//! weights read from `bench/weights.tsv`.

use wayfinder_bench::score::{parse_weights, ratio, weighted_geomean};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn equal_weights_give_the_plain_geometric_mean() {
    let w = parse_weights("a\t1\nb\t1\n").unwrap();
    let s = weighted_geomean(&[("a", 0.5), ("b", 2.0)], &w).unwrap();
    assert!(close(s, 1.0), "{s}");
    let s = weighted_geomean(&[("a", 0.25), ("b", 1.0)], &w).unwrap();
    assert!(close(s, 0.5), "{s}");
}

#[test]
fn a_heavier_metric_pulls_the_score_toward_its_ratio() {
    // exp((3*ln 0.5 + 1*ln 2) / 4) = 0.5^(1/2)
    let w = parse_weights("fast\t3\nslow\t1\n").unwrap();
    let s = weighted_geomean(&[("fast", 0.5), ("slow", 2.0)], &w).unwrap();
    assert!(close(s, 0.5f64.sqrt()), "{s}");
}

#[test]
fn below_one_means_wayfinder_wins() {
    let w = parse_weights("a\t1\nb\t5\n").unwrap();
    assert!(weighted_geomean(&[("a", 0.9), ("b", 0.8)], &w).unwrap() < 1.0);
    assert!(weighted_geomean(&[("a", 1.1), ("b", 1.2)], &w).unwrap() > 1.0);
}

#[test]
fn throughput_ratio_is_inverted_so_higher_is_a_win() {
    // Lower-is-better: wf 5ms vs solr 10ms is 0.5.
    assert!(close(ratio(5.0, 10.0, false).unwrap(), 0.5));
    // Higher-is-better: wf 200 qps vs solr 100 qps is also 0.5.
    assert!(close(ratio(200.0, 100.0, true).unwrap(), 0.5));
}

#[test]
fn a_zero_measurement_is_an_error_not_nan_or_infinity() {
    assert!(ratio(0.0, 10.0, false).is_err());
    assert!(ratio(10.0, 0.0, false).is_err());
    assert!(ratio(0.0, 10.0, true).is_err());
    assert!(ratio(10.0, 0.0, true).is_err());
}

#[test]
fn a_weighted_metric_with_no_ratio_is_an_error_naming_it() {
    let w = parse_weights("a\t1\nmissing_one\t1\n").unwrap();
    let e = weighted_geomean(&[("a", 1.0)], &w).unwrap_err();
    assert!(e.contains("missing_one"), "{e}");
}

#[test]
fn a_measured_metric_with_no_weight_is_an_error_naming_it() {
    let w = parse_weights("a\t1\n").unwrap();
    let e = weighted_geomean(&[("a", 1.0), ("unweighted_one", 1.0)], &w).unwrap_err();
    assert!(e.contains("unweighted_one"), "{e}");
}

#[test]
fn a_non_positive_ratio_is_an_error() {
    let w = parse_weights("a\t1\n").unwrap();
    assert!(weighted_geomean(&[("a", 0.0)], &w).is_err());
    assert!(weighted_geomean(&[("a", -1.0)], &w).is_err());
}

#[test]
fn weights_file_skips_comments_and_blank_lines() {
    let w = parse_weights("# note\n\na\t2\n").unwrap();
    let s = weighted_geomean(&[("a", 0.5)], &w).unwrap();
    assert!(close(s, 0.5));
}

#[test]
fn bad_weights_are_rejected() {
    assert!(parse_weights("a\tlots\n").is_err());
    assert!(parse_weights("a\t0\n").is_err());
    assert!(parse_weights("a\t-1\n").is_err());
    assert!(parse_weights("a\n").is_err());
    assert!(parse_weights("a\t1\na\t2\n").is_err(), "duplicate metric");
}

#[test]
fn shipped_weights_file_parses_and_favours_tail_latency_and_cpu() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/weights.tsv");
    let text = std::fs::read_to_string(path).expect("bench/weights.tsv exists");
    let w = parse_weights(&text).expect("bench/weights.tsv parses");
    // Probe the weights through the score: a metric that counts more moves
    // the score further. Give p99 and cpu_ms_per_1k a ratio of 0.5 and every
    // other weighted metric 2.0; the key metrics must outweigh the rest.
    let names: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').next().unwrap().to_string())
        .collect();
    for key in ["p99", "cpu_ms_per_1k"] {
        assert!(names.iter().any(|n| n == key), "{key} is weighted");
    }
    assert!(names.len() > 4, "covers more than the old four metrics");
    let ratios: Vec<(&str, f64)> = names
        .iter()
        .map(|n| {
            let key = n == "p99" || n == "cpu_ms_per_1k";
            (n.as_str(), if key { 0.5 } else { 2.0 })
        })
        .collect();
    let k = names.len() as f64;
    let unweighted = (((k - 2.0) * 2f64.ln() + 2.0 * 0.5f64.ln()) / k).exp();
    let s = weighted_geomean(&ratios, &w).unwrap();
    assert!(s < unweighted, "key metrics count more: {s} vs {unweighted}");
}
