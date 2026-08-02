//! Issue #234: the report-level caveat about an unmeasured 2M row applies
//! only when the rendered corpus is not literally 2,000,000 documents.

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const STALE_2M_NOTE: &str = "**\"Resident memory, 2M docs under query load\" is only ever populated by a run with a 2M-doc corpus**";
const PHASE_DELTA_NOTE: &str =
    "RSS increased by 100.0 MB between that sample and the later maximum of 700.0 MB";
const RSS_ATTRIBUTION_CAVEAT: &str =
    "The harness does not distinguish allocator-resident memory from mmap-backed index pages.";
const STARTUP_TARGET_OUTCOME: &str =
    "Wayfinder met the PRD's <50 MB startup-idle resident-memory target at 40.0 MB.";

// Issue #251: `render_report` now takes 18 positional args, not 16 -- each
// engine's cold-latency-file argument sits immediately after that engine's
// (now warm) latencies file, per correction 2's approved contract:
//   solr: ... <solr_index_mb> <solr_latencies_warm> <solr_latencies_cold>
//   wf:   ... <wf_index_mb>   <wf_latencies_warm>   <wf_latencies_cold>
//   then <corpus_size> <out_markdown_path>
//
// The warm and cold sample values are deliberately distinct and
// non-degenerate (not both "1\n"): a helper where both rows render the
// same p95 would hide a wiring bug where the implementor passes the warm
// file twice instead of a real cold file.
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
    let solr_latencies_warm = dir.join("solr-latencies-warm.txt");
    let solr_latencies_cold = dir.join("solr-latencies-cold.txt");
    let wayfinder_latencies_warm = dir.join("wayfinder-latencies-warm.txt");
    let wayfinder_latencies_cold = dir.join("wayfinder-latencies-cold.txt");
    let report = dir.join("benchmarks.md");
    fs::write(&solr_latencies_warm, "1\n").expect("write Solr warm latencies");
    fs::write(&solr_latencies_cold, "99\n").expect("write Solr cold latencies");
    fs::write(&wayfinder_latencies_warm, "0.5\n").expect("write Wayfinder warm latencies");
    fs::write(&wayfinder_latencies_cold, "77\n").expect("write Wayfinder cold latencies");

    let output = Command::new(env!("CARGO_BIN_EXE_render_report"))
        .args(["1", "2", "3", "4", "5", "6"])
        .arg(&solr_latencies_warm)
        .arg(&solr_latencies_cold)
        .args(["40", "600", "700", "8", "9", "10"])
        .arg(&wayfinder_latencies_warm)
        .arg(&wayfinder_latencies_cold)
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

// Issue #251: the two p95 rows must reflect the two distinct sample files
// this helper writes -- a warm-cache row and a cold-cache row with
// different, non-degenerate values (see `render_report`'s doc comment).
// This guards the helper itself: if the implementor's `render.sh`/
// `render_report` wiring (or a future edit to this helper) passed the warm
// latencies file in both the warm and cold argument slots, both rows would
// render "1.00 ms"/"0.50 ms" and this test would catch it, whereas a naive
// substring check for either number alone would not.
#[test]
fn warm_and_cold_p95_rows_render_the_distinct_sample_values_they_were_given() {
    let rendered = render_report(50_000);

    assert!(
        rendered.contains("1.00 ms") && rendered.contains("99.00 ms"),
        "expected Solr's warm p95 (1.00 ms) and cold p95 (99.00 ms) to both appear, distinctly, \
         got:\n{rendered}"
    );
    assert!(
        rendered.contains("0.50 ms") && rendered.contains("77.00 ms"),
        "expected Wayfinder's warm p95 (0.50 ms) and cold p95 (77.00 ms) to both appear, \
         distinctly, got:\n{rendered}"
    );

    let warm_row = rendered
        .lines()
        .find(|l| l.contains("p95 query latency, warm cache"))
        .unwrap_or_else(|| panic!("expected a warm-cache p95 row, got:\n{rendered}"));
    let cold_row = rendered
        .lines()
        .find(|l| l.contains("p95 query latency, cold cache"))
        .unwrap_or_else(|| panic!("expected a cold-cache p95 row, got:\n{rendered}"));
    assert_ne!(
        warm_row, cold_row,
        "the warm-cache and cold-cache p95 rows must not be identical -- a helper that fed \
         the same latencies file to both would hide a real wiring bug"
    );
    assert!(
        warm_row.contains("1.00 ms") && warm_row.contains("0.50 ms"),
        "the warm row must carry the warm sample values, got:\n{warm_row}"
    );
    assert!(
        cold_row.contains("99.00 ms") && cold_row.contains("77.00 ms"),
        "the cold row must carry the cold sample values, not the warm ones, got:\n{cold_row}"
    );
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
        "the 2M note must retain the measured post-index-to-later-maximum RSS delta without \
         attributing it to query load, got:\n{two_m_notes}"
    );
    assert!(
        !two_m_notes.contains("maximum sampled during query load"),
        "the 2M note must not claim the later RSS maximum was sampled during query load; the \
         harness only records it as a later sample, got:\n{two_m_notes}"
    );
    assert!(
        two_m_notes.contains(RSS_ATTRIBUTION_CAVEAT),
        "the 2M note must retain the allocator-versus-mmap attribution caveat, got:\n{two_m_notes}"
    );
    assert!(
        two_m_notes.contains(STARTUP_TARGET_OUTCOME),
        "the 2M note must report the startup-idle target outcome from the startup sample, got:\n{two_m_notes}"
    );
    assert!(
        !two_m_notes.contains("query load added"),
        "the 2M note must not causally attribute the RSS increase to query load, got:\n{two_m_notes}"
    );
}

// Issue #251: the rendered notes must carry the open "which target does
// `<= baseline` mean" statement -- the PRD does not say whether its p95
// target refers to the warm-cache or cold-cache row, the warm row compares
// a cached Solr against an uncached Wayfinder, and #251 tracks settling it.
// The notes must not pre-empt that open product decision by declaring
// either latency row's target met or missed.
const CACHE_TARGET_AMBIGUITY_NOTE: &str =
    "the PRD does not say which of the two p95 rows its `<= baseline` target refers to";
const WARM_ROW_CACHE_ASYMMETRY_NOTE: &str =
    "the warm row compares a cached Solr against an uncached Wayfinder";
const ISSUE_251_TRACKING_NOTE: &str = "#251 tracks settling it";

#[test]
fn notes_state_the_open_cache_target_question_without_declaring_either_target_met_or_missed() {
    let rendered = render_report(50_000);
    let rendered_notes = notes(&rendered);

    assert!(
        rendered_notes.contains(CACHE_TARGET_AMBIGUITY_NOTE),
        "expected the notes to state that the PRD does not say which p95 row its <= baseline \
         target means, got:\n{rendered_notes}"
    );
    assert!(
        rendered_notes.contains(WARM_ROW_CACHE_ASYMMETRY_NOTE),
        "expected the notes to state that the warm row compares a cached Solr with an \
         uncached Wayfinder, got:\n{rendered_notes}"
    );
    assert!(
        rendered_notes.contains(ISSUE_251_TRACKING_NOTE),
        "expected the notes to say #251 tracks settling the open cache-target question, got:\n\
         {rendered_notes}"
    );
    assert!(
        !rendered_notes.contains("met the PRD's") || !rendered_notes.contains("p95 query latency"),
        "the notes must not declare a p95 latency target met, got:\n{rendered_notes}"
    );
    assert!(
        !rendered_notes.contains("missed the PRD's")
            || !rendered_notes.contains("p95 query latency"),
        "the notes must not declare a p95 latency target missed, got:\n{rendered_notes}"
    );
}
