//! Issue #234: the report-level caveat about an unmeasured 2M row applies
//! only when the rendered corpus is not literally 2,000,000 documents.

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const STALE_2M_NOTE: &str = "**\"Resident memory, 2M docs under query load\" is only ever populated by a run with a 2M-doc corpus**";
const PHASE_DELTA_NOTE: &str = "RSS increased by 100.0 MB between that sample and the 700.0 MB maximum sampled during query load";
const RSS_ATTRIBUTION_CAVEAT: &str =
    "The harness does not distinguish allocator-resident memory from mmap-backed index pages.";

fn render_report(corpus_size: u64) -> String {
    let unique = format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the epoch")
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::SeqCst),
    );
    let dir = std::env::temp_dir().join(format!("wayfinder-render-report-notes-{unique}"));
    fs::create_dir_all(&dir).expect("create temporary render-report directory");
    let solr_latencies = dir.join("solr-latencies.txt");
    let wayfinder_latencies = dir.join("wayfinder-latencies.txt");
    let report = dir.join("benchmarks.md");
    fs::write(&solr_latencies, "1\n").expect("write Solr latencies");
    fs::write(&wayfinder_latencies, "1\n").expect("write Wayfinder latencies");

    let output = Command::new(env!("CARGO_BIN_EXE_render_report"))
        .args(["1", "2", "3", "4", "5"])
        .arg(&solr_latencies)
        .args(["600", "700", "8", "9", "10"])
        .arg(&wayfinder_latencies)
        .arg(corpus_size.to_string())
        .arg(&report)
        .output()
        .expect("run render_report");
    assert!(
        output.status.success(),
        "render_report failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let rendered = fs::read_to_string(&report).expect("read rendered report");
    fs::remove_dir_all(dir).expect("remove temporary render-report directory");
    rendered
}

fn notes(rendered: &str) -> &str {
    rendered
        .split_once("## Notes\n")
        .map(|(_, notes)| notes)
        .expect("rendered report should have a Notes section")
}

#[test]
fn literal_2m_render_omits_the_not_measured_note_but_50k_retains_it() {
    let fifty_k_report = render_report(50_000);
    let fifty_k_notes = notes(&fifty_k_report);
    assert!(
        fifty_k_notes.contains(STALE_2M_NOTE),
        "a 50k render must retain the note explaining its unmeasured 2M row, got:\n{fifty_k_notes}"
    );

    let two_m_report = render_report(2_000_000);
    let two_m_notes = notes(&two_m_report);
    assert!(
        !two_m_notes.contains(STALE_2M_NOTE),
        "a literal 2M render must omit the stale note explaining an unmeasured 2M row, got:\n{two_m_notes}"
    );
    assert!(
        two_m_notes.contains(PHASE_DELTA_NOTE),
        "the 2M note must describe a measured phase-to-phase RSS increase without attributing it causally to queries, got:\n{two_m_notes}"
    );
    assert!(
        two_m_notes.contains(RSS_ATTRIBUTION_CAVEAT),
        "the 2M note must retain the allocator-versus-mmap attribution caveat, got:\n{two_m_notes}"
    );
    assert!(
        !two_m_notes.contains("query load added"),
        "the 2M note must not causally attribute the RSS increase to query load, got:\n{two_m_notes}"
    );
}
