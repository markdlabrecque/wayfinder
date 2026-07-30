//! Issue #62 (defect 2): `bench/run.sh`'s schema add-field POST (around
//! lines 151-164) uses `curl -sSf`, so on a non-2xx response curl discards
//! the response body and exits non-zero before the script's own
//! `grep -q '"errors"'` check ever runs -- the operator sees only
//! `curl: (22) ... error: 400`, never *which* field Solr rejected.
//!
//! Interpretation / test seam (ambiguity flagged, see handoff): the spec
//! asks to "drop -f, capture status code separately, check status + body
//! explicitly" without naming an interface. This test assumes the fix
//! factors that check into a standalone function,
//! `check_schema_add_field_response(status, body)`, that:
//!
//!   - returns non-zero and prints `body` to stderr when `status` is not
//!     2xx, or when `body` contains a Solr-style `"errors"` key even on 200
//!   - returns 0 (no output) otherwise
//!
//! decoupled from the exact curl invocation, so the implementor is free to
//! capture status/body however they like (e.g. `curl -sS -o file -w
//! '%{http_code}'`) as long as they call this with the results. If this
//! contract doesn't fit, escalate rather than editing this test file.
//!
//! Today `run.sh` has no such function -- extraction fails with a clear
//! "missing behavior" message, which is the expected red state.

mod support;

use support::{extract_bash_function, fresh_scratch_dir, run_bash, run_sh_source};

#[test]
fn non_2xx_status_fails_and_surfaces_the_response_body() {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "check_schema_add_field_response").expect(
        "run.sh should define a `check_schema_add_field_response(status, body)` function \
         (issue #62 defect 2's fix target); it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("schema-check-4xx");
    let body = r#"{"error":{"msg":"unknown field type text_en","code":400}}"#;
    let script = format!("{func}\ncheck_schema_add_field_response '400' '{body}'\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        !out.status.success(),
        "a 400 response must be treated as a failure, not silently accepted"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown field type text_en"),
        "expected the actual Solr error body to be surfaced on stderr so an operator can see \
         which field was rejected, got: {stderr:?}"
    );
}

#[test]
fn http_200_with_errors_key_still_fails() {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "check_schema_add_field_response").expect(
        "run.sh should define a `check_schema_add_field_response(status, body)` function \
         (issue #62 defect 2's fix target); it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("schema-check-200-errors");
    let body = r#"{"responseHeader":{"status":400},"errors":[{"field":"category"}]}"#;
    let script = format!("{func}\ncheck_schema_add_field_response '200' '{body}'\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        !out.status.success(),
        "Solr can return HTTP 200 with an \"errors\" body on a rejected add-field; that must \
         still be treated as a failure"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("category"),
        "expected the errors body to be surfaced on stderr, got: {stderr:?}"
    );
}

#[test]
fn http_200_with_no_errors_succeeds() {
    let source = run_sh_source();
    let func = extract_bash_function(&source, "check_schema_add_field_response").expect(
        "run.sh should define a `check_schema_add_field_response(status, body)` function \
         (issue #62 defect 2's fix target); it does not exist in run.sh yet",
    );

    let dir = fresh_scratch_dir("schema-check-200-ok");
    let body = r#"{"responseHeader":{"status":0}}"#;
    let script = format!("{func}\ncheck_schema_add_field_response '200' '{body}'\n");
    let out = run_bash(&script, &dir, &[]);

    assert!(
        out.status.success(),
        "a clean 200 with no errors key must be treated as success; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
