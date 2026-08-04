//! `/wayfinder/{core}/update/extract` extractOnly route tests (issue #258).
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
//! What the suite covers, in file order: the seven in-scope success fixtures
//! plus the corrupt-PDF row (a recorded status divergence, not a match); the
//! `EXTRACT_PARAMS` allowlist under `strict_params`, `check_core`, and a
//! route-order guard that `/update` itself still works; multipart intake
//! errors; the resource budgets from the
//! `[extraction]` config section (`max_body_bytes` — including its exact
//! boundary and the request-wide accounting across parts —
//! `max_concurrency`, `max_inflight_uploads` (the issue #273 intake bound,
//! separate from the parse pool), `max_output_bytes`, `deadline_secs`) and the
//! route-level transport ceiling that bounds part headers the handler never
//! sees; and charset precedence (BOM > declared > detected), including the
//! 64 KiB detection window's boundary.

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use wayfinder::extract::ExtractionRuntime;

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

/// As [`build_app_with_config`], but keeps the `AppServer` long enough to
/// hand back the extraction pool the router actually admits against.
///
/// A sibling rather than a change to `build_app_with_config`'s signature:
/// every other test in this file uses that one and does not need the handle.
fn build_app_with_extraction(
    config: &str,
) -> anyhow::Result<(Router, Arc<ExtractionRuntime>, TempDir)> {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config_path = dir.path().join("wayfinder.toml");
    std::fs::write(&config_path, config).expect("write wayfinder.toml");

    let server = wayfinder::app_server_with_config(&schema_path, &data_dir, &config_path)?;
    let extraction = server.extraction();
    Ok((server.into_router(), extraction, dir))
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

// --- json.nl named-list shapes (issue #274) -------------------------------
//
// `EXTRACT_PARAMS` allowlists `json.nl` (consistent with the other routes);
// issue #274 closed the gap where the handler rendered `file_metadata` in
// the flat alternating array regardless of it. The captured ground truth
// (`extract_plain_text_json_nl_{map,arrarr,arrmap}.json`, finding 128) shows
// Solr honours `json.nl` here exactly as it does on the facet routes:
// `file_metadata` is a plain NamedList and reshapes per the param, while
// `responseHeader` (a `SimpleOrderedMap`) and `file` (a String value) are
// untouched. The flat baseline is `extract_plain_text_xml` (#171) above.

#[tokio::test]
async fn extract_plain_text_json_nl_map_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&resource.name=sample.txt&wt=json&json.nl=map",
        "sample.txt",
        "",
        "extract_plain_text_json_nl_map",
    )
    .await;
}

#[tokio::test]
async fn extract_plain_text_json_nl_arrarr_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&resource.name=sample.txt&wt=json&json.nl=arrarr",
        "sample.txt",
        "",
        "extract_plain_text_json_nl_arrarr",
    )
    .await;
}

#[tokio::test]
async fn extract_plain_text_json_nl_arrmap_matches_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&resource.name=sample.txt&wt=json&json.nl=arrmap",
        "sample.txt",
        "",
        "extract_plain_text_json_nl_arrmap",
    )
    .await;
}

/// Pins the handler's `file_metadata` JSON shape per `json.nl` value directly,
// independently of `normalize_extract`, so the failure is unambiguous while the
// handler still ignores the param (the pre-#274 state): every value currently
// renders flat. Each arm asserts the shape Solr's fixture dictates (finding
// 128): flat/arrarr/arrmap are arrays, map is an object.
#[tokio::test]
async fn extract_file_metadata_shape_follows_json_nl() {
    let (app, _dir) = default_app().await;
    let bytes = input_bytes("sample.txt");

    for (nl, expect_object) in [
        // (json.nl value, is file_metadata an object rather than an array?)
        ("flat", false),
        ("map", true),
        ("arrarr", false),
        ("arrmap", false),
    ] {
        let query = format!(
            "update/extract?extractOnly=true&resource.name=sample.txt&wt=json&json.nl={nl}"
        );
        let (status, v) = request_multipart(
            &app,
            &format!("{CORE}/{query}"),
            "file",
            "sample.txt",
            "",
            &bytes,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "json.nl={nl}: got {status}: {v}");
        let fm = &v["file_metadata"];
        assert_eq!(
            fm.is_object(),
            expect_object,
            "json.nl={nl}: file_metadata shape wrong, got {fm}"
        );
        // `responseHeader` is a `SimpleOrderedMap` and stays an object under
        // every json.nl; `file` is a String value, never reshaped.
        assert!(
            v["responseHeader"].is_object(),
            "json.nl={nl}: responseHeader must stay an object"
        );
        assert!(
            v["file"].is_string(),
            "json.nl={nl}: file must stay a string"
        );
    }
}

/// The captured flat baseline (`extract_plain_text_xml`, #171) must still
/// match with `json.nl` made explicit, so honouring the param does not
/// regress the default shape. `flat` is what the handler already produced
/// before #274, so this is a guard against a future change collapsing the
/// explicit-flat path onto something else.
#[tokio::test]
async fn extract_explicit_json_nl_flat_matches_the_flat_baseline_fixture() {
    assert_matches_fixture(
        "update/extract?extractOnly=true&resource.name=sample.txt&wt=json&json.nl=flat",
        "sample.txt",
        "",
        "extract_plain_text_xml",
    )
    .await;
}

/// `broken.pdf` (a `%PDF-` header with no xref/trailer) now reaches a real
/// parse: `lopdf::Document::load_mem` fails on the missing cross-reference
/// table, surfacing as `ExtractError::Parse` -> HTTP 500. That **retires the
/// former status divergence** (issue #294): when Wayfinder had no PDF
/// extractor the request never reached a parse attempt and answered 415
/// unsupported media type, but the captured `extract_corrupt_pdf.json` is a
/// 500, so the row was a recorded status divergence (PRD divergence 10).
/// Now Wayfinder can fail *inside* a PDF parser, the captured 500 is
/// reachable, and the divergence entry in `DIVERGENT_STATUS_MULTIPART` is
/// deleted rather than re-justified — exactly as the PRD predicted. The
/// differential harness (`extract_multipart_manifest_matches_captured_fixtures`)
/// diffs the full envelope; this route-level test pins the status and the
/// `NoParams` envelope shape directly so a regression here names itself
/// without reading the manifest.
#[tokio::test]
async fn broken_pdf_is_a_500_parse_failure_matching_the_capture() {
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
        "the captured fixture must still be a 500"
    );
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "broken.pdf must now reach a parse failure -> 500 (the PDF extractor \
         landed; the former 415 divergence is retired), got {status}: {actual}"
    );
    assert_eq!(
        actual["error"]["code"].as_i64(),
        Some(500),
        "body: {actual}"
    );
    assert_eq!(
        actual["responseHeader"]["status"].as_i64(),
        Some(500),
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
        .uri(format!("/wayfinder/{CORE}/update?commit=true"))
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
        "POST /wayfinder/{{core}}/update must still succeed regardless of the sibling /update/extract route"
    );
}

// --- extractOnly gating (spec item 3) ---------------------------------------
//
// #258 required `extractOnly=true` and 400d otherwise (PRD divergence 10).
// #259 retires that: `extractOnly` absent/false now takes the Solr-Cell
// indexing path. The positive indexing behaviour and its documented
// body/links divergence live in `tests/extract_index.rs`; the extractOnly
// response itself is still covered by the fixture-derived tests above.

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
// All gated through the `[extraction]` config section (spec item 9), so each
// test drives the budget it names from operator-visible configuration rather
// than from `ExtractLimits`'s defaults.

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

/// Boundary guard for `copy_counted`'s `consumed + len > max_bytes` test:
/// `max_body_bytes` is the largest body that is *accepted*, not the smallest
/// that is refused. Without this, relaxing the comparison to `>=` — refusing
/// a document of exactly the configured size — passes the whole suite.
///
/// The three sizes are asserted together so the pair of neighbours pins the
/// boundary rather than merely sampling near it: `max_body_bytes - 1` and
/// `max_body_bytes` are 200, `max_body_bytes + 1` is 413. Only the file
/// part's content is charged (part headers ride the route-level transport
/// ceiling instead), so the payload size is exactly the charged size.
///
/// The content is plain ASCII text well inside the default
/// `max_output_bytes`/`max_output_scalars`, so a 200 here is a real
/// extraction and not some other budget's 400 sneaking in — the assertions
/// check the extracted text came back, not just the status.
#[tokio::test]
async fn extract_body_exactly_at_max_body_bytes_is_accepted() {
    let config_toml = "[extraction]\nmax_body_bytes = 100\n";

    for (len, expected) in [
        (99_usize, StatusCode::OK),
        (100, StatusCode::OK),
        (101, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let (app, _dir) = build_app_with_config(Some(config_toml))
            .expect("extraction.max_body_bytes must be a valid config knob");
        let content = vec![b'a'; len];
        let (status, body) = request_multipart(
            &app,
            &format!("{CORE}/update/extract?extractOnly=true&extractFormat=text&wt=json"),
            "file",
            "boundary.txt",
            "",
            &content,
        )
        .await;
        assert_eq!(
            status, expected,
            "a {len}-byte document against max_body_bytes=100 must be {expected}, \
             got {status}: {body}"
        );
        if expected == StatusCode::OK {
            assert!(
                body["file"].as_str().unwrap_or_default().contains("aaa"),
                "an accepted body must actually be extracted, got: {body}"
            );
        } else {
            assert_eq!(body["error"]["code"].as_i64(), Some(413), "body: {body}");
        }
    }
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

/// The budget is *request-wide*, not per-part: two parts that each fit
/// individually must still 413 when their total does not.
///
/// The sibling test above cannot see this. Its five 100-byte parts each bust
/// a 40-byte cap on their own, so it passes identically under per-part
/// accounting — swapping `copy_counted`'s shared `consumed` for a per-call
/// local counter leaves it green. Here neither the 60-byte non-file part nor
/// the 60-byte file part exceeds `max_body_bytes = 100`; only their sum does,
/// so the assertion holds exactly when the counter is shared across parts.
#[tokio::test]
async fn extract_body_budget_is_shared_across_parts() {
    let config_toml = "[extraction]\nmax_body_bytes = 100\n";
    let (app, _dir) = build_app_with_config(Some(config_toml))
        .expect("extraction.max_body_bytes must be a valid config knob");

    let parts: Vec<(&str, &str, Vec<u8>)> = vec![
        ("meta", "", vec![b'm'; 60]),
        ("file", "a.txt", vec![b'a'; 60]),
    ];
    let (status, body) = request_multipart_with_raw_body(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&extractFormat=text&wt=json"),
        &multipart_parts(&parts),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "60 bytes of non-file field plus a 60-byte document is 120 bytes against a 100-byte \
         max_body_bytes and must 413; a per-part budget would extract it, got {status}: {body}"
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

/// An extraction admitted while the pool is at `max_concurrency` is rejected
/// (`TooBusy`), not queued — and the slot comes back afterwards.
///
/// Saturation here is deterministic, not a race: with
/// `max_concurrency = 1` the test holds the pool's only permit itself, via
/// `ExtractionRuntime::try_acquire_permit` on the very runtime the route
/// admits against, so zero slots remain while the request runs. No second
/// in-flight request, no sleeps, no timing tolerance.
///
/// The second half — drop the permit, repeat the request, expect `200` — is
/// what stops this passing trivially: without it a route that never extracts
/// anything would satisfy the `503` assertion just as well.
#[tokio::test]
async fn extract_concurrency_over_configured_max_concurrency_is_503() {
    let config_toml = "[extraction]\nmax_concurrency = 1\n";
    let (app, extraction, _dir) = build_app_with_extraction(config_toml)
        .expect("extraction.max_concurrency must be a valid config knob");

    let permit = extraction.try_acquire_permit();
    assert!(
        permit.is_some(),
        "the single configured extraction slot must be free before the test holds it"
    );

    let bytes = input_bytes("sample.txt");
    let url = format!("{CORE}/update/extract?extractOnly=true&wt=json");
    let (status, body) = request_multipart(&app, &url, "file", "sample.txt", "", &bytes).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "with extraction.max_concurrency=1 and its only slot held, an extraction must \
         503 (TooBusy), got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(503), "body: {body}");

    drop(permit);

    let (status, body) = request_multipart(&app, &url, "file", "sample.txt", "", &bytes).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "dropping the held permit must return the slot, so the same request now \
         succeeds, got {status}: {body}"
    );
}

/// Issue #273. `max_body_bytes × HTTP concurrency` was an unbounded RAM
/// multiplier: the parse permit (`max_concurrency`) is acquired *after* the
/// body is streamed in and read back resident, so it never capped intake —
/// the real resident ceiling was set by the connection count. The
/// in-flight-upload budget (`max_inflight_uploads`) fixes that: a separate
/// admission count acquired *before* any of the body is streamed.
///
/// Saturation is deterministic, not a race — the exact lesson from
/// [`extract_concurrency_over_configured_max_concurrency_is_503`], which
/// fired two overlapping requests and flunked ~1 run in 5 before
/// `6b88dcc` made saturation a fact. Here the test holds the single
/// configured in-flight slot itself, via `try_acquire_inflight` on the very
/// runtime the route admits against, so zero intake capacity remains while
/// the request runs. No second in-flight request, no sleeps, no timing
/// tolerance.
///
/// `max_concurrency` is left at its default, so the parse pool is
/// completely free during the 503 assertion — a 503 here can only mean the
/// *intake* budget bit, proving the two budgets are independent at the route
/// (not just at the runtime, which `tests/extraction.rs::
/// inflight_upload_budget_is_independent_of_the_parse_pool` covers).
///
/// The second half — drop the slot, repeat the request, expect `200` — is
/// what stops this passing trivially: without it a route that never enforces
/// intake would satisfy the `503` assertion just as well.
#[tokio::test]
async fn extract_inflight_uploads_over_configured_max_is_503() {
    let config_toml = "[extraction]\nmax_inflight_uploads = 1\n";
    let (app, extraction, _dir) = build_app_with_extraction(config_toml)
        .expect("extraction.max_inflight_uploads must be a valid config knob");

    let inflight = extraction.try_acquire_inflight();
    assert!(
        inflight.is_some(),
        "the single configured in-flight slot must be free before the test holds it"
    );

    let bytes = input_bytes("sample.txt");
    let url = format!("{CORE}/update/extract?extractOnly=true&wt=json");
    let (status, body) = request_multipart(&app, &url, "file", "sample.txt", "", &bytes).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "with extraction.max_inflight_uploads=1 and its only slot held, an upload must 503 \
         (intake saturated) even though the parse pool is free, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(503), "body: {body}");

    drop(inflight);

    let (status, body) = request_multipart(&app, &url, "file", "sample.txt", "", &bytes).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "dropping the held in-flight slot must return it, so the same upload now succeeds, \
         got {status}: {body}"
    );
}

/// Issue #273 edge. `max_inflight_uploads = 0` is the blunt shutoff: no
/// in-flight slot is ever available, so every upload is rejected at intake
/// with a 503 before any of the body is consumed. The parse pool is at its
/// default (free), so a 503 here can only be the intake budget — pinning the
/// documented `0 = reject all` behaviour at the route.
#[tokio::test]
async fn extract_inflight_uploads_zero_rejects_every_upload_at_intake() {
    let config_toml = "[extraction]\nmax_inflight_uploads = 0\n";
    let (app, _extraction, _dir) = build_app_with_extraction(config_toml)
        .expect("extraction.max_inflight_uploads = 0 must be a valid config knob");

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
        "with extraction.max_inflight_uploads=0 every upload must 503 at intake, \
         got {status}: {body}"
    );
    assert_eq!(body["error"]["code"].as_i64(), Some(503), "body: {body}");
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

/// Detection must not change its answer at the 64 KiB window boundary.
///
/// `resolve_charset` feeds only the first `CHARSET_DETECT_WINDOW`
/// (`64 * 1024`) bytes to `chardetng`, which answers `UTF-8` for ASCII-only
/// input while Tika labels the same input `ISO-8859-1`. The ASCII override
/// that reconciles the two must therefore scan *all* the bytes, not just the
/// window: judging ASCII-ness from the window alone flips the label from
/// `ISO-8859-1` to `UTF-8` at exactly 65,537 bytes — one byte past the
/// window — for a document whose content class never changed. That is a
/// wire-visible difference in `file_metadata`'s `Content-Encoding`, so this
/// test uploads `64 KiB + 1` bytes of pure ASCII, the first size at which
/// the two readings disagree.
///
/// The expected label comes from the ASCII `extract_plain_text_text`
/// fixture, per CLAUDE.md: crossing the window must produce the same label
/// Solr gave for a small ASCII document.
#[tokio::test]
async fn charset_ascii_past_the_detection_window_keeps_the_iso_8859_1_label() {
    let (app, _dir) = default_app().await;
    let bytes = vec![b'a'; 64 * 1024 + 1];
    let (status, actual) = request_multipart(
        &app,
        &format!("{CORE}/update/extract?extractOnly=true&extractFormat=text&wt=json"),
        "file",
        "big-ascii.txt",
        "",
        &bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {status}: {actual}");

    let content_encoding = |v: &Value| {
        v["file_metadata"].as_array().and_then(|arr| {
            arr.windows(2)
                .find(|w| w[0].as_str() == Some("Content-Encoding"))
                .and_then(|w| w[1].as_array().and_then(|vals| vals.last().cloned()))
        })
    };
    let expected = content_encoding(&fixture("extract_plain_text_text"))
        .expect("extract_plain_text_text fixture must have a Content-Encoding entry");
    assert_eq!(
        content_encoding(&actual).unwrap_or(Value::Null),
        expected,
        "an all-ASCII upload one byte past the 64 KiB detection window must keep the same \
         Content-Encoding a small ASCII document gets, got: {actual}"
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
