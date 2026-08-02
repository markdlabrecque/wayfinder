//! `/solr/{core}/update/extract` extractOnly route tests (issue #258).
//!
//! Every test in this file talks to the real HTTP route via
//! `common::request_multipart`/`common::request_multipart_raw`/
//! `common::request_multipart_with_raw_body`, so a green suite here proves
//! the whole stack — router wiring, param allowlist, `check_core`, multipart
//! intake, charset resolution, and budget enforcement — end to end, not just
//! `src/extract.rs`'s unit-level API.
//!
//! Expected values for the fixture-derived tests come from
//! `solr-ref/responses/*.json` via `common::fixture`, `common::diff::normalize`,
//! and `common::diff::normalize_extract`, never hand-typed, per CLAUDE.md's
//! compatibility contract.
//!
//! Every test here is expected to fail today (RED): the route
//! `/solr/{core}/update/extract` does not exist yet (`src/lib.rs`'s
//! `search_api_routes!` has no entry for it), so every request in this file
//! currently gets axum's unmatched-route 404 instead of the behaviour under
//! test. This is a deliberately *shared* failure reason across the whole
//! file — see the individual test comments for anything that would need a
//! second reason once the route exists.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use common::diff::{diff, normalize, normalize_extract};
use common::{fixture, request_multipart, request_multipart_with_raw_body};

const CORE: &str = "content";

fn extract_inputs_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("solr-ref/extract-inputs")
}

fn input_bytes(name: &str) -> Vec<u8> {
    let path = extract_inputs_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read extract input {}: {e}", path.display()))
}

fn build_app_with_config(config: Option<&str>) -> anyhow::Result<(Router, TempDir)> {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let app = match config {
        Some(toml) => {
            let config_path = dir.path().join("wayfinder.toml");
            std::fs::write(&config_path, toml).expect("write wayfinder.toml");
            wayfinder::app_with_config(&schema_path, &data_dir, &config_path)?
        }
        None => wayfinder::app(&schema_path, &data_dir)?,
    };
    Ok((app, dir))
}

async fn default_app() -> (Router, TempDir) {
    build_app_with_config(None).expect("app must build with default config")
}

// --- fixture-derived tests: all 7 in-scope success fixtures + the 500 -----
//
// One test per fixture (rather than a loop) so a failure names the exact
// fixture in the test name, matching this suite's convention elsewhere
// (`tests/admin_luke.rs`, etc.) and the pipeline's own preference for the
// implementor to see one red test per behaviour rather than one giant loop
// test whose first failure hides the rest.

async fn assert_matches_fixture(query: &str, part_input: &str, mime: &str, fixture_name: &str) {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes(part_input);
    let (status, actual) = request_multipart(
        &app,
        &format!("{CORE}/{query}"),
        "file",
        part_input,
        mime,
        &bytes,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "expected 200 for {fixture_name}, got {status}: {actual}"
    );

    let expected = fixture(fixture_name);
    let expected_n = normalize(expected);
    let actual_n = normalize(actual);
    let expected_n2 = normalize_extract(expected_n.value);
    let actual_n2 = normalize_extract(actual_n.value);
    let report = diff(&expected_n2.value, &actual_n2.value);
    assert!(
        report.diffs.is_empty(),
        "{fixture_name}: response must match the captured fixture after normalize()+\
         normalize_extract(), diffs: {:?}",
        report.diffs
    );
}

#[tokio::test]
async fn extract_plain_text_xml_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&resource.name=sample.txt&wt=json",
        "sample.txt",
        "",
        "extract_plain_text_xml",
    )
    .await;
}

#[tokio::test]
async fn extract_plain_text_text_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&extractFormat=text&resource.name=sample.txt&wt=json",
        "sample.txt",
        "",
        "extract_plain_text_text",
    )
    .await;
}

#[tokio::test]
async fn extract_html_only_xml_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&resource.name=sample.html&wt=json",
        "sample.html",
        "text/html",
        "extract_html_only_xml",
    )
    .await;
}

#[tokio::test]
async fn extract_html_only_text_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&extractFormat=text&resource.name=sample.html&wt=json",
        "sample.html",
        "text/html",
        "extract_html_only_text",
    )
    .await;
}

#[tokio::test]
async fn extract_latin1_text_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&extractFormat=text&resource.name=sample-latin1.txt&wt=json",
        "sample-latin1.txt",
        "",
        "extract_latin1_text",
    )
    .await;
}

#[tokio::test]
async fn extract_utf8_bom_text_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&extractFormat=text&resource.name=sample-utf8-bom.txt&wt=json",
        "sample-utf8-bom.txt",
        "",
        "extract_utf8_bom_text",
    )
    .await;
}

#[tokio::test]
async fn extract_declared_charset_text_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&extractFormat=text&resource.name=sample-latin1.txt&wt=json",
        "sample-latin1.txt",
        "text/plain; charset=ISO-8859-1",
        "extract_declared_charset_text",
    )
    .await;
}

/// The corrupt-PDF row is a **recorded status divergence**, not a fixture
/// match. `extract_corrupt_pdf.json` is Solr's Tika parsing a malformed PDF
/// and throwing, which is a 500. Wayfinder has no PDF extractor at all, so it
/// never reaches a parse attempt: the document is an unimplemented format and
/// the answer is 415. Recorded in `DIVERGENT_STATUS_MULTIPART` in
/// `tests/differential.rs` and as PRD ratified divergence 10; both retire when
/// a PDF extractor lands.
///
/// The captured 500 is still loaded here, so this test fails if the fixture's
/// status ever stops being the thing Wayfinder diverges from.
#[tokio::test]
async fn extract_corrupt_pdf_is_a_recorded_status_divergence() {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes("broken.pdf");
    let (status, actual) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&extractFormat=text&resource.name=broken.pdf&wt=json"),
        "file",
        "broken.pdf",
        "",
        &bytes,
    )
    .await;

    let captured = fixture("extract_corrupt_pdf");
    assert_eq!(
        captured["error"]["code"].as_i64(),
        Some(500),
        "the captured fixture must still be a 500, or this divergence entry is stale"
    );
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "a PDF must come back 415 (no PDF extractor exists), got {status}: {actual}"
    );
    assert_eq!(
        actual["error"]["code"].as_i64(),
        Some(415),
        "body: {actual}"
    );
    assert_eq!(
        actual["responseHeader"]["status"].as_i64(),
        Some(415),
        "the NoParams envelope still carries responseHeader, body: {actual}"
    );
    assert!(
        actual
            .get("responseHeader")
            .and_then(|h| h.get("params"))
            .is_none(),
        "an /update path never echoes params, got: {actual}"
    );
}

// --- route/param tests -----------------------------------------------------

/// `EXTRACT_PARAMS` allowlist under `strict_params=true` (spec item 1):
/// every documented param must be accepted, mirroring
/// `tests/admin_luke.rs::luke_strict_params_accepts_the_documented_solr_params`.
#[tokio::test]
async fn extract_strict_params_accepts_the_documented_params() {
    let (app, _dir) = build_app_with_config(Some("strict_params = true\n"))
        .expect("app must build with strict_params=true");
    let bytes = input_bytes("sample.txt");

    for query in [
        "update/extract?extractOnly=true&wt=json",
        "update/extract?extractOnly=true&extractFormat=text&wt=json",
        "update/extract?extractOnly=true&resource.name=sample.txt&wt=json",
        "update/extract?extractOnly=true&omitHeader=false&wt=json",
        "update/extract?extractOnly=true&wt=json&json.nl=flat",
    ] {
        let (status, body) = request_multipart(
            &app,
            &format!("{CORE}/{query}"),
            "file",
            "sample.txt",
            "",
            &bytes,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "strict_params=true must not reject a documented EXTRACT_PARAMS param ({query}), \
             got {status}: {body}"
        );
    }
}

/// Mutation guard: `strict_params=true` must still 400 an undocumented
/// param, same pattern as `luke_strict_params_rejects_an_unknown_param`.
#[tokio::test]
async fn extract_strict_params_rejects_an_unknown_param() {
    let (app, _dir) = build_app_with_config(Some("strict_params = true\n"))
        .expect("app must build with strict_params=true");
    let bytes = input_bytes("sample.txt");

    let (status, body) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json&bogus=1"),
        "file",
        "sample.txt",
        "",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unknown param must be rejected under strict_params, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_u64(), Some(400));
}

/// Mutation guard for the `check_core` call the spec requires this handler
/// to make, same pattern as `admin_luke.rs`'s `luke_unknown_core_is_a_json_404`.
#[tokio::test]
async fn extract_unknown_core_is_a_json_404() {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes("sample.txt");

    let (status, body) = request_multipart(
        &app,
        "nosuchcore/update/extract?extractOnly=true&wt=json",
        "file",
        "sample.txt",
        "",
        &bytes,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown core must 404, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(404), "body: {body}");
    assert!(
        body.get("responseHeader").is_some(),
        "NoParams envelope still carries responseHeader, got: {body}"
    );
    assert!(
        body.get("responseHeader")
            .and_then(|h| h.get("params"))
            .is_none(),
        "NoParams envelope must not echo params, got: {body}"
    );
}

/// Route-order regression guard the spec explicitly asks for: adding the new
/// `/update/extract` route must not disturb `/update` itself. This test
/// passes ALREADY, before any implementation for #258 exists — it is a
/// green regression guard, not a red test, included here so the same file
/// proves both "the new route exists" (red today) and "the old route
/// still works" (green today and must stay green).
#[tokio::test]
async fn plain_update_route_still_works_after_extract_route_exists() {
    let (app, _dir) = default_app().await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/solr/{CORE}/update?commit=true"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!([{"id": "doc-1", "body": "hello", "category": ["a"]}]).to_string(),
        ))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("request must not fail");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "POST /solr/{{core}}/update must still succeed regardless of the sibling /update/extract route"
    );
}

// --- extractOnly gating (spec item 3) ---------------------------------------

/// `extractOnly` absent must 400 in the `NoParams` envelope (ratified
/// divergence from Solr, which 200s and indexes — server-side indexing is
/// out of scope for this issue).
#[tokio::test]
async fn extract_without_extract_only_is_a_400() {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes("sample.txt");
    let (status, body) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?wt=json"),
        "file",
        "sample.txt",
        "",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "extractOnly absent must 400 (server-side indexing is out of scope), got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

/// `extractOnly=false` must also 400 — the gate checks the resolved boolean,
/// not merely the param's presence.
#[tokio::test]
async fn extract_with_extract_only_false_is_a_400() {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes("sample.txt");
    let (status, body) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=false&wt=json"),
        "file",
        "sample.txt",
        "",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "extractOnly=false must 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

// --- multipart intake errors (spec item 2; no fixture, per spec) -----------

#[tokio::test]
async fn extract_rejects_a_non_multipart_body() {
    let (app, _dir) = default_app().await;
    let (status, body) = request_multipart_with_raw_body(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        b"not multipart at all",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-multipart body must 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

#[tokio::test]
async fn extract_rejects_a_multipart_body_with_no_file_part() {
    let (app, _dir) = default_app().await;
    // A multipart body with a non-file part only (no filename) — still valid
    // multipart/form-data, but no file part for the handler to extract.
    let (status, body) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        "not-a-file-field",
        "",
        "",
        b"just some text",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a multipart body with no file part must 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

#[tokio::test]
async fn extract_rejects_a_malformed_multipart_envelope() {
    let (app, _dir) = default_app().await;
    // Correct content-type header, garbage body — malformed at the
    // multipart-parsing level rather than the "no file part" level above.
    let (status, body) = request_multipart_with_raw_body(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        b"--not-the-declared-boundary\r\nThis is not valid multipart body content.\r\n",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a malformed multipart envelope must 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

// --- budgets, mutation-tested through the HTTP route (spec item 1, 9) ------
//
// All four gated through a new `[extraction]` config section (spec item 9),
// which does not exist yet in `src/config.rs` — every test below currently
// fails at `build_app_with_config`'s `wayfinder::app_with_config` call with a
// TOML "unknown field" deserialization error (a legitimate, behaviour-level
// red result: the config section really doesn't exist, this isn't a typo in
// the test), not a compile error in this file.

#[tokio::test]
async fn extract_body_over_configured_max_body_bytes_is_413() {
    let config_toml = "[extraction]\nmax_body_bytes = 40\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.max_body_bytes must be a valid config knob");

    // sample.txt is 31 bytes; padding it past the configured 40-byte cap
    // proves the *configured* value gates the route, not axum's disabled
    // global DefaultBodyLimit letting anything through.
    let oversized = vec![b'x'; 128];
    let (status, body) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        "file",
        "big.txt",
        "",
        &oversized,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body over the configured extraction.max_body_bytes must 413 with the captured \
         BodyTooLarge envelope, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(413));
}

/// Builds a multipart body from `(name, filename, content)` triples.
/// `filename` empty means no `filename=` parameter, i.e. a non-file field.
fn multipart_parts(parts: &[(&str, &str, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, content) in parts {
        body.extend_from_slice(format!("--{}\r\n", common::MULTIPART_BOUNDARY).as_bytes());
        let disposition = if filename.is_empty() {
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n")
        } else {
            format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n")
        };
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{}--\r\n", common::MULTIPART_BOUNDARY).as_bytes());
    body
}

/// The hole the single-part 413 test walked straight past: parts the handler
/// *skips* are still parts the handler *read*. `next_field()` drains a
/// skipped field to completion, so a body made entirely of non-file fields
/// used to be consumed in full — at any length — and then answered
/// `400 MissingContentStream`. Their bytes must be charged to the same
/// request-wide budget as the document's.
#[tokio::test]
async fn extract_non_file_parts_are_charged_against_max_body_bytes() {
    let config_toml = "[extraction]\nmax_body_bytes = 40\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.max_body_bytes must be a valid config knob");

    // No file part anywhere: reaching the end of this body at all means it
    // was read in full. 5 x 100 bytes of content is well over the 40-byte cap.
    let parts: Vec<(&str, &str, Vec<u8>)> = (0..5)
        .map(|_| ("literal", "", vec![b'x'; 100]))
        .collect::<Vec<_>>();
    let (status, body) = request_multipart_with_raw_body(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        &multipart_parts(&parts),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "non-file parts over extraction.max_body_bytes must 413, not be drained and answered \
         400 MissingContentStream, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(413), "body: {body}");
}

/// The counter must not over-fire: a body of non-file fields that stays
/// *inside* the budget still reaches the normal "no file part" 400. Without
/// this, "413 everything" would pass the test above.
#[tokio::test]
async fn extract_small_non_file_parts_still_reach_missing_content_stream() {
    let config_toml = "[extraction]\nmax_body_bytes = 40\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.max_body_bytes must be a valid config knob");

    let parts: Vec<(&str, &str, Vec<u8>)> = vec![
        ("literal", "", b"short".to_vec()),
        ("literal", "", b"also short".to_vec()),
    ];
    let (status, body) = request_multipart_with_raw_body(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        &multipart_parts(&parts),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "15 bytes of non-file fields is inside a 40-byte budget and must still be a plain \
         missing-file-part 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400), "body: {body}");
}

/// The part the handler-side counter structurally cannot reach: multipart
/// part *headers* are consumed by the multipart reader before a field exists,
/// so no byte of them is ever offered to the handler. The route-level
/// `DefaultBodyLimit` (`ExtractLimits::route_body_ceiling`) is what bounds
/// them. Here the whole body is one part header and no content at all.
#[tokio::test]
async fn extract_oversized_part_headers_are_bounded_by_the_route_body_limit() {
    let config_toml = "[extraction]\nmax_body_bytes = 40\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.max_body_bytes must be a valid config knob");

    // 4 MiB of header, comfortably past the 1 MiB framing head-room the
    // ceiling allows above max_body_bytes, and zero bytes of part content.
    let huge_name = "n".repeat(4 * 1024 * 1024);
    let parts: Vec<(&str, &str, Vec<u8>)> = vec![(huge_name.as_str(), "big.txt", Vec::new())];
    let (status, body) = request_multipart_with_raw_body(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        &multipart_parts(&parts),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "an unbounded run of part-header bytes must be stopped by the route body limit, \
         got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(413), "body: {body}");
}

#[tokio::test]
async fn extract_concurrency_over_configured_max_concurrency_is_503() {
    // max_concurrency = 1: a second concurrent extraction request while the
    // first is in flight must be rejected (TooBusy), not queued. This test
    // does not attempt real inter-request synchronization (the harness has
    // no hook to pause an in-flight extraction); it pins the *shape* of the
    // 503 envelope by asserting the config is honoured for a single request
    // once the runtime is saturated by a fixed small value — the implementor
    // is expected to either satisfy this via a synchronization hook added to
    // the extraction runtime, or the reviewer/implementor may need to redesign
    // this specific test once ExtractionRuntime's concurrency-control surface
    // is wired to the route (flagged in the handoff: this is the one budget
    // test whose harness-level mechanism is not fully specified yet).
    let config_toml = "[extraction]\nmax_concurrency = 1\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.max_concurrency must be a valid config knob");

    let bytes = input_bytes("sample.txt");
    let app2 = app.clone();
    let bytes2 = bytes.clone();
    let req1 = tokio::spawn(async move {
        request_multipart(
            &app,
            &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
            "file",
            "sample.txt",
            "",
            &bytes,
        )
        .await
    });
    let req2 = tokio::spawn(async move {
        request_multipart(
            &app2,
            &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
            "file",
            "sample.txt",
            "",
            &bytes2,
        )
        .await
    });

    let (r1, r2) = tokio::join!(req1, req2);
    let (status1, body1) = r1.expect("task must not panic");
    let (status2, body2) = r2.expect("task must not panic");

    let statuses = [status1, status2];
    assert!(
        statuses.contains(&StatusCode::SERVICE_UNAVAILABLE),
        "with extraction.max_concurrency=1, at least one of two concurrent extractions must \
         503 (TooBusy), got {status1} ({body1}) and {status2} ({body2})"
    );
}

#[tokio::test]
async fn extract_output_over_configured_max_output_bytes_is_400() {
    let config_toml = "[extraction]\nmax_output_bytes = 8\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.max_output_bytes must be a valid config knob");

    let bytes = input_bytes("sample.txt");
    let (status, body) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&extractFormat=text&wt=json"),
        "file",
        "sample.txt",
        "",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "output over the configured extraction.max_output_bytes must 400 (OutputTooLarge), \
         got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(400));
}

#[tokio::test]
async fn extract_deadline_exceeded_is_503() {
    let config_toml = "[extraction]\ndeadline_secs = 0\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.deadline_secs must be a valid config knob");

    let bytes = input_bytes("sample.txt");
    let (status, body) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&wt=json"),
        "file",
        "sample.txt",
        "",
        &bytes,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a zero-second deadline must trip DeadlineExceeded (503), got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(503));
}

// --- charset precedence (spec item 4) ---------------------------------------

/// BOM beats declared charset: `sample-utf8-bom.txt` has a UTF-8 BOM;
/// declaring a conflicting charset on the part must not override it, and the
/// BOM itself must not appear in the output (consumed).
#[tokio::test]
async fn charset_bom_beats_declared_charset() {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes("sample-utf8-bom.txt");
    let (status, actual) = request_multipart(
        &app,
        &format!(
            "{CORE}/update/extract?extractOnly=true&extractFormat=text&resource.name=sample-utf8-bom.txt&wt=json"
        ),
        "file",
        "sample-utf8-bom.txt",
        "text/plain; charset=ISO-8859-1",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {status}: {actual}");

    let file = actual["file"].as_str().unwrap_or_default();
    assert!(
        !file.contains('\u{feff}'),
        "a resolved BOM must be consumed, not appear as U+FEFF in the value, got {file:?}"
    );

    // Same expected shape as the fixture's own BOM handling: compare against
    // extract_utf8_bom_text (no declared-charset row exists for the BOM
    // input, so this asserts the declared charset was ignored by comparing
    // the metadata's Content-Encoding, not the whole envelope).
    let expected = fixture("extract_utf8_bom_text");
    let expected_encoding = expected["file_metadata"]
        .as_array()
        .and_then(|arr| {
            arr.windows(2)
                .find(|w| w[0].as_str() == Some("Content-Encoding"))
                .map(|w| w[1].clone())
        })
        .expect("extract_utf8_bom_text fixture must have a Content-Encoding entry");
    let actual_encoding = actual["file_metadata"]
        .as_array()
        .and_then(|arr| {
            arr.windows(2)
                .find(|w| w[0].as_str() == Some("Content-Encoding"))
                .map(|w| w[1].clone())
        })
        .unwrap_or(Value::Null);
    assert_eq!(
        actual_encoding, expected_encoding,
        "declared charset=ISO-8859-1 must not override a UTF-8 BOM"
    );
}

/// Declared charset beats detection: `sample-latin1.txt` (no BOM) declared as
/// `windows-1252` must resolve to the `ISO-8859-1` label per the fixtures'
/// normalisation rule, not whatever chardetng would detect on its own.
#[tokio::test]
async fn charset_declared_beats_detection() {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes("sample-latin1.txt");
    let (status, actual) = request_multipart(
        &app,
        &format!(
            "{CORE}/update/extract?extractOnly=true&extractFormat=text&resource.name=sample-latin1.txt&wt=json"
        ),
        "file",
        "sample-latin1.txt",
        "text/plain; charset=windows-1252",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {status}: {actual}");

    let encoding = actual["file_metadata"]
        .as_array()
        .and_then(|arr| {
            arr.windows(2)
                .find(|w| w[0].as_str() == Some("Content-Encoding"))
                .map(|w| w[1].clone())
        })
        .unwrap_or(Value::Null);
    assert_eq!(
        encoding,
        json!(["ISO-8859-1", "ISO-8859-1"]),
        "windows-1252 must normalise to the ISO-8859-1 label, got {encoding:?}"
    );
}
