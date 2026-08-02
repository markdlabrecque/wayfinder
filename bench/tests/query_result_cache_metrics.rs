//! Issue #251 round 2, item B: `run.sh`'s queryResultCache counter parse
//! (`query_result_cache_stat`) is broken against real Solr 9, verified live
//! by the round-2 reviewer against a real Solr 9 container, in two
//! independent ways:
//!
//!   1. It GETs `admin/mbeans?cat=CACHE&stats=true&wt=json` and treats
//!      `solr-mbeans` as a flat `[name, value, name, value, ...]` array to
//!      zip into a dict. Without `json.nl=map`, Solr instead renders
//!      `solr-mbeans` as a TYPE SIGNATURE (HTTP 200, but not even valid
//!      JSON); with `json.nl=map` it's a dict already, so the flat zip
//!      blows up on that shape instead. Either way this path never worked.
//!   2. Even given a fixed transport, the code builds the stats key as
//!      `CACHE.queryResultCache.<stat>`. Real Solr's key has a `.searcher`
//!      scope: `CACHE.searcher.queryResultCache.<stat>`. The code's
//!      fallback to the bare `<stat>` key masked this in prior (non-live)
//!      testing but is exactly the kind of silent-wrong-number risk this
//!      test exists to close off.
//!
//! Agreed fix target (round-2 reviewer's recommendation + live
//! verification): drop the mbeans endpoint entirely in favor of
//!
//!   `GET <base>/admin/metrics?group=core&prefix=CACHE.searcher.queryResultCache&wt=json`
//!
//! -- plain nested JSON, no NamedList flat/map ambiguity. This endpoint is
//! SERVER-level, not core-relative: the core shows up as a registry key
//! `solr.core.<core>` inside the top-level `metrics` object, so the parse
//! must select the right registry rather than assume there's only one.
//!
//! This test does NOT pin the URL as a literal string -- a guard like that
//! would only freeze whichever URL the implementor happens to type, exactly
//! the failure mode the round-2 reviewer called out in the round-1 tests.
//! Instead it pins the *parse* against a real, committed Solr 9 response
//! (`bench/tests/fixtures/solr9-metrics-queryresultcache.json`, captured
//! live against a 2M-doc `content` core -- see the handoff for the raw
//! values it encodes: `hits: 1`, `lookups: 3`, registry `solr.core.content`).
//!
//! The parse today lives inline in a bash heredoc inside `run.sh` and is
//! not callable in isolation, so it has zero coverage. This test requires
//! the implementor to extract it to an invocable script with exactly this
//! contract (named precisely so there's no room to guess wrong):
//!
//!   `python3 bench/query_result_cache_stat.py <core> <stat>`
//!
//!   -- reads the `admin/metrics?group=core&prefix=CACHE.searcher.queryResultCache&wt=json`
//!   response body on stdin, prints the integer counter value to stdout
//!   with a trailing newline, and exits 0.
//!
//!   On a missing `solr.core.<core>` registry, a missing
//!   `CACHE.searcher.queryResultCache` bean within it, or a missing
//!   `<stat>` key within that bean: a non-zero exit and a message on
//!   stderr naming what's missing -- never a silent `0`, matching the
//!   existing heredoc's `sys.exit("...")` failure style.
//!
//! `run.sh` (owned by the implementor) is expected to call this script in
//! place of the current heredoc, piping the metrics response to it on
//! stdin, for both `query_result_cache_hits` and `query_result_cache_lookups`.
//!
//! If this contract doesn't fit (different path, different argument order,
//! a different language), escalate rather than editing this file.
//!
//! Hermetic: no network, no Docker -- the fixture is a committed file, and
//! this test only ever pipes it to a local script over stdin.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture_json() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/solr9-metrics-queryresultcache.json");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()))
}

fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("query_result_cache_stat.py")
}

/// Runs `python3 bench/query_result_cache_stat.py <core> <stat>` with
/// `stdin_json` piped to stdin. Panics with a clear "missing behavior"
/// message (rather than a bare spawn error) if the script doesn't exist
/// yet -- that's the expected red state until the implementor extracts it
/// from run.sh's heredoc.
fn run_stat(core: &str, stat: &str, stdin_json: &str) -> std::process::Output {
    let script = script_path();
    assert!(
        script.exists(),
        "expected an invocable `python3 bench/query_result_cache_stat.py <core> <stat>` script, \
         reading the admin/metrics response body on stdin and printing the counter to stdout \
         (issue #251 round 2, item B), at {}; it does not exist yet -- run.sh's inline python3 \
         heredoc needs to be extracted to exactly this path and argument contract so the parse \
         is testable outside a bash heredoc",
        script.display()
    );

    let mut child = Command::new("python3")
        .arg(&script)
        .arg(core)
        .arg(stat)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn python3 bench/query_result_cache_stat.py");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(stdin_json.as_bytes())
        .expect("failed to write fixture JSON to the script's stdin");
    child
        .wait_with_output()
        .expect("failed to wait on query_result_cache_stat.py")
}

#[test]
fn parses_hits_from_the_searcher_scoped_key_under_the_matching_core_registry() {
    let out = run_stat("content", "hits", &fixture_json());
    assert!(
        out.status.success(),
        "expected a clean parse of `hits` from the fixture's `solr.core.content` registry \
         (CACHE.searcher.queryResultCache.hits); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "1",
        "the fixture's CACHE.searcher.queryResultCache.hits under solr.core.content is 1 \
         (live-verified against Solr 9), got stdout {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn parses_lookups_from_the_searcher_scoped_key_under_the_matching_core_registry() {
    let out = run_stat("content", "lookups", &fixture_json());
    assert!(
        out.status.success(),
        "expected a clean parse of `lookups` from the fixture's `solr.core.content` registry \
         (CACHE.searcher.queryResultCache.lookups); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "3",
        "the fixture's CACHE.searcher.queryResultCache.lookups under solr.core.content is 3 \
         (live-verified against Solr 9), got stdout {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn fails_loudly_not_silently_zero_when_the_core_registry_is_missing() {
    // The fixture only has a `solr.core.content` registry; asking for a
    // different core must not silently resolve to 0 (e.g. by falling
    // through to the first registry found, or defaulting on a KeyError).
    let out = run_stat("does-not-exist", "hits", &fixture_json());
    assert!(
        !out.status.success(),
        "a core with no matching `solr.core.<core>` registry in the metrics response must fail \
         loudly (non-zero exit), not silently print 0 -- got stdout {:?}, status {:?}",
        String::from_utf8_lossy(&out.stdout),
        out.status
    );
    assert_ne!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "0",
        "a missing registry must not resolve to a silent 0 on stdout even alongside a non-zero \
         exit"
    );
}

#[test]
fn fails_loudly_not_silently_zero_when_the_stat_key_is_missing() {
    let out = run_stat("content", "not_a_real_stat", &fixture_json());
    assert!(
        !out.status.success(),
        "a stat key absent from the CACHE.searcher.queryResultCache bean must fail loudly \
         (non-zero exit), not silently print 0 -- got stdout {:?}, status {:?}",
        String::from_utf8_lossy(&out.stdout),
        out.status
    );
    assert_ne!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "0",
        "a missing stat key must not resolve to a silent 0 on stdout even alongside a non-zero \
         exit"
    );
}
