//! Expiring guard for the `hl.regex.*` descope (#353, PRD §5 Highlighting).
//!
//! `search_api_solr` 4.4.0's `setHighlighting()` never emits `hl.regex.pattern`,
//! `hl.regex.slop`, or `hl.regex.maxAnalyzedChars` because of an **inverted
//! inner guard** in the vendored source. The outer guard fires for any non-`gap`
//! fragmenter -- which means the fragmenter *is* `regex` -- but the inner guard
//! then tests the opposite and is always false, so the `setRegex*` calls it
//! guards never run:
//!
//! ```php
//! if ('gap' !== $highlighter['fragmenter']) {        // 4248: fragmenter != gap
//!   $hl->setFragmenter($highlighter['fragmenter']); // 4249: emits hl.fragmenter
//!   if ('regex' !== $highlighter['fragmenter']) {    // 4250: INVERTED -- always false
//!     $hl->setRegexPattern(...);                     // 4251: never reached
//!     ...
//!   }
//! }
//! ```
//!
//! Issue #353 implements `hl.fragmenter` (which the client DOES reach: the outer
//! guard fires and `setFragmenter('regex')` runs at line 4249) but deliberately
//! does NOT build `hl.regex.*`, because the inversion above makes those three
//! params unreachable client traffic. Wayfinder admits `hl.fragmenter` and falls
//! back to gap behaviour for `regex`; it does not admit `hl.regex.*`.
//!
//! Per CLAUDE.md's rule for deliberate skips, this file must fail the day the
//! evidence stops holding. When it goes red, the fix is **not** to weaken it --
//! it is to build `hl.regex.*` (the regex fragmenter over each value) now that
//! the client can actually send it, and then delete this file.
//!
//! Two evidence channels, mirroring `tests/version_write_descope_guard.rs`:
//!
//!   1. The inverted guard is still present in the vendored 4.4.0 source.
//!   2. No captured client request in `solr-ref/search-api/trace/` sends any
//!      `hl.regex.*` param.

use std::path::{Path, PathBuf};

use serde_json::Value;

const SOURCE: &str = include_str!(
    "../coverage/search_api_solr_4.4.0_source/src/Plugin/search_api/backend/SearchApiSolrBackend.php"
);
const TRACE_DIR: &str = "solr-ref/search-api/trace";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn trace_files() -> Vec<PathBuf> {
    let dir = root().join(TRACE_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

fn load(path: &Path) -> Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

// --- source channel: the inverted regex guard is still present -------------

/// The outer fragmenter guard. Fires for any non-`gap` fragmenter, so the
/// client reaches `setFragmenter` -- which is why #353 implements
/// `hl.fragmenter`. If this moves or changes shape, re-derive what the client
/// reaches before trusting the rest of this guard.
#[test]
fn source_still_reaches_setfragmenter_via_the_outer_guard() {
    assert!(
        SOURCE.contains("if ('gap' !== $highlighter['fragmenter'])"),
        "the outer fragmenter guard moved out of the vendored source; re-check whether \
         `hl.fragmenter` is still reached by the client before trusting the rest of this guard"
    );
    assert!(
        SOURCE.contains("setFragmenter"),
        "`setFragmenter` left the source; the `hl.fragmenter` implementation's premise moved"
    );
}

/// The bug itself. Reaching the inner test means the fragmenter *is* `regex`
/// (the outer guard already ruled out `gap`), so `'regex' !== fragmenter` is
/// always false and the `setRegex*` calls below it never run. The day upstream
/// flips this to `===` (or restructures it), this assertion fails and
/// `hl.regex.*` becomes real client traffic -- build the regex fragmenter and
/// delete this file.
#[test]
fn source_still_has_the_inverted_inner_regex_guard() {
    assert!(
        SOURCE.contains("if ('regex' !== $highlighter['fragmenter'])"),
        "the inverted regex guard is GONE from the vendored source. That inversion is the sole \
         reason `hl.regex.*` was descoped: with it gone, search_api_solr now reaches \
         setRegexPattern/setRegexSlop/setRegexMaxAnalyzedChars and `hl.regex.*` is real client \
         traffic. Build the regex fragmenter (issue #353), admit `hl.regex.pattern`/\
         `hl.regex.slop`/`hl.regex.maxAnalyzedChars` to SELECT_PARAMS, and delete this guard -- \
         do NOT weaken it."
    );
    // Positive control: the gated setters still exist in the source (the regex
    // API is wired up, just unreachable), so the assertion above is looking at
    // the right block rather than passing because the block was deleted.
    assert!(
        SOURCE.contains("setRegexPattern")
            && SOURCE.contains("setRegexSlop")
            && SOURCE.contains("setRegexMaxAnalyzedChars"),
        "one of the `setRegex*` setters left the source; the inversion guard's premise moved -- \
         re-derive what the client now reaches"
    );
}

// --- trace channel: no captured request sends hl.regex.* ------------------

/// No captured client request -- query string, headers, or body -- sends any
/// `hl.regex.*` param. This is the second half of the descope's premise: not
/// only does the source never emit them, no captured client ever sends them.
#[test]
fn no_trace_request_sends_any_hl_regex_param() {
    for file in trace_files() {
        let capture = load(&file);
        // Serialize the whole request so the scan covers the path (query
        // string), headers, and a JSON/form body uniformly.
        let request = serde_json::to_string(&capture["request"])
            .unwrap_or_else(|e| panic!("{}: serialize request: {e}", file.display()));
        assert!(
            !request.contains("hl.regex."),
            "{}: a captured request now sends an `hl.regex.*` param. The #353 descope rests on \
             the client never reaching the regex fragmenter, so that premise no longer holds -- \
             build `hl.regex.*` and delete this guard.",
            file.display()
        );
    }
}

/// Positive control for the request-side scan: the corpus DOES carry `hl.*`
/// highlighting traffic, so the scan's "no `hl.regex.`" claim is a real
/// asymmetry rather than a corpus that simply has no highlighting at all.
/// Without this, the request-side guard could stay permanently, falsely green
/// if highlighting were stripped from the traces upstream of capture.
#[test]
fn traces_do_carry_hl_highlighting_so_the_regex_absence_is_real() {
    let mut seen = false;
    for file in trace_files() {
        let capture = load(&file);
        let request = serde_json::to_string(&capture["request"]).unwrap_or_default();
        if request.contains("hl=true") || request.contains("hl.") {
            seen = true;
            break;
        }
    }
    assert!(
        seen,
        "no trace carries any `hl.*` highlighting param. The `hl.regex.*` absence claim is now \
         blind -- pick a new positive control or revisit the guard."
    );
}
