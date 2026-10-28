//! Keeping the wf-bench machine steady (issue #455): engines and the load
//! generator on separate fixed cores, machine and engine versions recorded in
//! every result, and a refusal (or warning with `--force`) on a busy machine.
//! The new seam is `wayfinder_bench::machine`, pure functions over text and
//! numbers so none of this needs Docker.

use serde_json::json;
use wayfinder_bench::machine::{
    EnvInfo, check_busy, env_json, parse_cpu_list, parse_cpu_model, parse_loadavg,
    parse_solr_version, validate_cores,
};

#[test]
fn cpu_lists_expand_ranges_and_commas() {
    assert_eq!(parse_cpu_list("2-5").unwrap(), vec![2, 3, 4, 5]);
    assert_eq!(parse_cpu_list("0,1,6-7").unwrap(), vec![0, 1, 6, 7]);
    assert!(parse_cpu_list("").is_err());
    assert!(parse_cpu_list("5-2").is_err());
    assert!(parse_cpu_list("a").is_err());
}

#[test]
fn engines_and_driver_must_not_share_a_core() {
    assert!(validate_cores("2-5", "0-1").is_ok());
    let err = validate_cores("2-5", "5,6").unwrap_err();
    assert!(
        err.contains('5'),
        "error should name the shared core: {err}"
    );
}

#[test]
fn loadavg_reads_the_one_minute_figure() {
    assert_eq!(parse_loadavg("0.52 0.40 0.30 1/234 5678\n"), Some(0.52));
    assert_eq!(parse_loadavg("garbage"), None);
}

#[test]
fn busy_machine_is_refused_unless_forced_then_warned() {
    assert_eq!(check_busy(0.2, 1.0, false), Ok(None));
    assert!(check_busy(3.5, 1.0, false).is_err());
    let warning = check_busy(3.5, 1.0, true).unwrap().expect("a warning");
    assert!(
        warning.contains("3.5"),
        "warning should show the load: {warning}"
    );
}

#[test]
fn cpu_model_comes_from_proc_cpuinfo() {
    let cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\n\
                   model name\t: Intel(R) Core(TM) i7-1185G7 @ 3.00GHz\ncpu MHz\t\t: 1200.0\n";
    assert_eq!(
        parse_cpu_model(cpuinfo).as_deref(),
        Some("Intel(R) Core(TM) i7-1185G7 @ 3.00GHz")
    );
    assert_eq!(parse_cpu_model("nothing useful"), None);
}

#[test]
fn solr_version_comes_from_the_system_info_response() {
    let info = r#"{"lucene":{"solr-spec-version":"9.7.0","lucene-spec-version":"9.11.1"}}"#;
    assert_eq!(parse_solr_version(info).as_deref(), Some("9.7.0"));
    assert_eq!(parse_solr_version("{}"), None);
}

#[test]
fn result_records_machine_cores_and_both_engine_versions() {
    let info = EnvInfo {
        kernel: "7.2.8".into(),
        cpu_model: "Test CPU".into(),
        engine_cpuset: "2-5".into(),
        driver_cpuset: "0-1".into(),
        wayfinder_git_sha: "a0ebc3c".into(),
        solr_version: "9.7.0".into(),
        solr_image: "solr@sha256:abc".into(),
        solr_heap: "2g".into(),
    };
    let v = env_json(&info);
    assert_eq!(v["machine"]["kernel"], json!("7.2.8"));
    assert_eq!(v["machine"]["cpu_model"], json!("Test CPU"));
    assert_eq!(v["cores"]["engines"], json!("2-5"));
    assert_eq!(v["cores"]["driver"], json!("0-1"));
    assert_eq!(v["wayfinder"]["git_sha"], json!("a0ebc3c"));
    assert_eq!(v["solr"]["version"], json!("9.7.0"));
    assert_eq!(v["solr"]["image"], json!("solr@sha256:abc"));
    assert_eq!(v["solr"]["heap"], json!("2g"));
}
