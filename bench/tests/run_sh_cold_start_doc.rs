//! Issue #62 (defect 3): `bench/run.sh`'s `SOLR_COLD_MS` timer
//! (`wait_for_ping`, called right after `docker run -d`) starts *after*
//! `docker run -d` returns, so container-create time isn't counted -- cold
//! start is understated slightly.
//!
//! Per the issue, a full fix is optional here ("fix or document the
//! exclusion"); this test only requires the cheap option: a comment next
//! to the `docker run -d` call that names the exclusion, so a reader of
//! the script (or the rendered benchmark numbers) knows what's not being
//! measured. Currently absent -- red until either a comment is added or
//! the timer is moved to start before `docker run -d`.

mod support;

use support::run_sh_source;

/// Lines immediately preceding (and including) the `docker run -d ...
/// solr:9` invocation, where a caveat comment about the cold-start
/// timing window would live.
fn solr_container_start_context(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let run_idx = lines
        .iter()
        .position(|l| l.contains("docker run -d") && l.contains("solr:9"))
        .expect(
            "expected to find the `docker run -d ... solr:9` line in run.sh; \
             if this invocation moved, update this test's anchor",
        );
    let ctx_start = run_idx.saturating_sub(6);
    lines[ctx_start..=run_idx].join("\n")
}

#[test]
fn cold_start_container_create_exclusion_is_documented() {
    let source = run_sh_source();
    let ctx = solr_container_start_context(&source).to_lowercase();

    assert!(
        ctx.contains("cold") && ctx.contains("exclu"),
        "expected a comment near `docker run -d ... solr:9` (or the `SOLR_COLD_MS` \
         timer it feeds) documenting that cold-start timing excludes container-create \
         time -- SOLR_COLD_MS's clock (via wait_for_ping) starts only after `docker run -d` \
         returns, not before the container is created. Either add that comment, or start the \
         timer before `docker run -d` and remove the need for this exclusion note. Context \
         inspected:\n{ctx}"
    );
}
