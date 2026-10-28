//! Run-to-run noise (issue #453): the same build is measured K times, the
//! spread per metric is stored (`bench/out/noise.json`), and a change only
//! counts as a win when it beats that spread. Lower scores are better.

mod support;

use support::fresh_scratch_dir;
use wayfinder_bench::noise::{Spread, Verdict, classify, load_noise, measure_noise, save_noise};

fn run(p50: f64, qps: f64) -> Vec<(String, f64)> {
    vec![("p50_ms".to_string(), p50), ("qps".to_string(), qps)]
}

#[test]
fn spread_per_metric_is_the_min_and_max_over_the_runs() {
    let runs = vec![
        run(12.0, 300.0),
        run(10.0, 340.0),
        run(11.0, 320.0),
        run(13.0, 310.0),
        run(10.5, 330.0),
    ];
    let spread = measure_noise(&runs).unwrap();
    let names: Vec<&str> = spread.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["p50_ms", "qps"]);
    assert_eq!((spread[0].1.min, spread[0].1.max), (10.0, 13.0));
    assert_eq!((spread[1].1.min, spread[1].1.max), (300.0, 340.0));
}

#[test]
fn measuring_noise_from_no_runs_is_an_error() {
    assert!(measure_noise(&[]).is_err());
}

#[test]
fn the_stored_spread_round_trips() {
    let path = fresh_scratch_dir("noise-rt").join("noise.json");
    assert_eq!(load_noise(&path).unwrap(), None);
    let spread = measure_noise(&[run(12.0, 300.0), run(10.0, 340.0)]).unwrap();
    save_noise(&path, &spread).unwrap();
    assert_eq!(load_noise(&path).unwrap(), Some(spread));
}

#[test]
fn a_corrupt_noise_file_is_an_error() {
    let path = fresh_scratch_dir("noise-corrupt").join("noise.json");
    std::fs::write(&path, "not json").unwrap();
    assert!(load_noise(&path).is_err());
}

#[test]
fn a_drop_bigger_than_the_spread_is_a_win() {
    let spread = Spread {
        min: 0.78,
        max: 0.82,
    }; // width 0.04
    assert_eq!(classify(0.80, 0.70, &spread), Verdict::Win);
}

#[test]
fn a_rise_bigger_than_the_spread_is_a_loss() {
    let spread = Spread {
        min: 0.78,
        max: 0.82,
    };
    assert_eq!(classify(0.80, 0.90, &spread), Verdict::Loss);
}

#[test]
fn a_change_within_the_spread_is_noise_in_both_directions() {
    let spread = Spread {
        min: 0.78,
        max: 0.82,
    };
    assert_eq!(classify(0.80, 0.77, &spread), Verdict::Noise);
    assert_eq!(classify(0.80, 0.83, &spread), Verdict::Noise);
    assert_eq!(classify(0.80, 0.80, &spread), Verdict::Noise);
}

#[test]
fn with_no_spread_any_improvement_is_a_win() {
    let spread = Spread {
        min: 0.80,
        max: 0.80,
    };
    assert_eq!(classify(0.80, 0.79, &spread), Verdict::Win);
    assert_eq!(classify(0.80, 0.80, &spread), Verdict::Noise);
}
