//! Issue #243: startup RSS is a separate benchmark phase from both the
//! post-index/pre-query sample and the maximum observed while queries run.
//!
//! This is deliberately a source-order guard rather than a live benchmark:
//! `run.sh` has Docker, HTTP, and corpus-generation side effects, while the
//! required fact is that each engine captures its startup sample after its
//! health check and before indexing begins.

mod support;

use support::run_sh_source;

fn call_lines<'a>(source: &'a str, function: &str) -> Vec<(usize, &'a str)> {
    source
        .lines()
        .enumerate()
        .filter_map(|(line_no, line)| {
            let trimmed = line.trim_start();
            (trimmed.contains(function)
                && !trimmed.starts_with('#')
                && !trimmed.starts_with(&format!("{function}()"))
                && !trimmed.starts_with(&format!("function {function}")))
            .then_some((line_no, line))
        })
        .collect()
}

fn text_between_lines(source: &str, start: usize, end: usize) -> String {
    source
        .lines()
        .skip(start + 1)
        .take(end.saturating_sub(start + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn each_engine_samples_startup_memory_after_ping_before_indexing_and_keeps_post_index_samples() {
    let source = run_sh_source();
    let pings = call_lines(&source, "wait_for_ping");
    let indexing = call_lines(&source, "index_corpus");
    let schema_checks = call_lines(&source, "check_schema_add_field_response");

    assert!(
        pings.len() >= 2 && indexing.len() >= 2,
        "expected one wait_for_ping/index_corpus phase per engine; found pings={pings:?}, indexing={indexing:?}"
    );
    assert_eq!(
        schema_checks.len(),
        1,
        "expected exactly one Solr schema response validation; found {schema_checks:?}"
    );

    let wayfinder_startup_window = text_between_lines(&source, pings[0].0, indexing[0].0);
    assert!(
        wayfinder_startup_window.contains("WF_STARTUP_IDLE_KB=$(pids_rss_kb")
            && wayfinder_startup_window.contains("WF_STARTUP_IDLE_MB=$(awk"),
        "Wayfinder must assign WF_STARTUP_IDLE_MB from pids_rss_kb after wait_for_ping and before index_corpus; got:\n{wayfinder_startup_window}"
    );

    assert!(
        pings[1].0 < schema_checks[0].0 && schema_checks[0].0 < indexing[1].0,
        "Solr schema response validation must run after ping and before indexing; pings={pings:?}, schema_checks={schema_checks:?}, indexing={indexing:?}"
    );
    let solr_startup_window = text_between_lines(&source, schema_checks[0].0, indexing[1].0);
    assert!(
        solr_startup_window.contains("  exit 1\nfi\nSOLR_STARTUP_IDLE_MB=$(solr_mem_mb)"),
        "Solr must assign SOLR_STARTUP_IDLE_MB only after the schema-validation failure branch closes and before index_corpus; got:\n{solr_startup_window}"
    );

    let wayfinder_post_index = text_between_lines(&source, indexing[0].0, pings[1].0);
    assert!(
        wayfinder_post_index.contains("WF_POST_INDEX_MB"),
        "Wayfinder must keep a distinct WF_POST_INDEX_MB sample after indexing; got:\n{wayfinder_post_index}"
    );

    let solr_post_index = text_between_lines(&source, indexing[1].0, source.lines().count());
    assert!(
        solr_post_index.contains("SOLR_POST_INDEX_MB"),
        "Solr must keep a distinct SOLR_POST_INDEX_MB sample after indexing; got:\n{solr_post_index}"
    );
}

#[test]
fn render_report_receives_each_engine_memory_phases_in_order_then_corpus_and_output() {
    const CONTRACT: &str = r#""$BENCH_BIN/render_report" \
  "$SOLR_STARTUP_IDLE_MB" "$SOLR_POST_INDEX_MB" "$SOLR_LOAD_MB" "$SOLR_COLD_MS" "$SOLR_IMAGE_MB" "$SOLR_INDEX_MB" "$SOLR_LATENCIES" \
  "$WF_STARTUP_IDLE_MB" "$WF_POST_INDEX_MB" "$WF_LOAD_MB" "$WF_COLD_MS" "$WF_IMAGE_MB" "$WF_INDEX_MB" "$WF_LATENCIES" \
  "$SIZE" \
  "$ROOT/docs/benchmarks.md""#;

    let source = run_sh_source();
    assert_eq!(
        source.matches("\"$BENCH_BIN/render_report\"").count(),
        1,
        "expected exactly one render_report invocation"
    );
    assert!(
        source.contains(CONTRACT),
        "render_report must receive Solr startup/post-index/load, then Wayfinder startup/post-index/load, then corpus size and output path in its fixed positional contract"
    );
}
