//! Defence-in-depth for issue #39: a panic inside a handler must not drop
//! the connection silently. Today there is no `catch_panic`/`catch_unwind`
//! layer anywhere in `src/` (`grep -rn "catch_panic\|CatchPanicLayer\|catch_unwind" src/`
//! returns nothing), so a panicking request currently takes the whole
//! connection down with it instead of producing an HTTP response.
//!
//! This test exercises the *real* production router (`wayfinder::app`, via
//! `common::indexed_app`), not a hand-rolled minimal one, and asserts that a
//! request which panics the handler still comes back as a normal HTTP
//! response: status 500, body shaped like `WfError`'s envelope
//! (`src/error.rs`) rather than a dropped connection.
//!
//! ## How the panic is triggered
//!
//! `src/core_index.rs::parse_query` only special-cases a *whole* query
//! string of `*:*` as `AllQuery`. When `*:*` shows up as a sub-clause of a
//! larger boolean query (e.g. `*:* AND lazy`), the string falls through to
//! `tantivy`'s `QueryParser::parse_query`, which builds an `Exists` leaf for
//! `*:*` and calls `.expect("Exist query without a field isn't allowed")` in
//! `tantivy-query-grammar-0.26.0/src/user_input_ast.rs` — an unconditional
//! panic reachable with nothing but a crafted `q=`. That is the only known,
//! real (not manufactured) panic reachable from attacker-controlled input
//! anywhere in `src/` today (`grep -rn ".unwrap()\|.expect(\|panic!(" src/`
//! turns up nothing else reachable from a handler with untrusted input).
//!
//! ponytail (resolved by the implementor): issue #39's fix for the
//! query-parsing bug itself (`core_index::parse_query` learning to
//! rewrite/tolerate a `*:*` sub-clause) landed in the very same change as
//! this panic-catching layer. Once that fix landed, `*:* AND lazy` stopped
//! panicking at all, which would have made this test either fail outright
//! (it hard-asserts 500) or pass for the wrong reason. There is no other
//! known real, attacker-reachable panic path left in `src/`
//! (`grep -rn ".unwrap()\|.expect(\|panic!(" src/` turns up nothing else
//! reachable from a handler with untrusted input) to swap in instead.
//!
//! Resolution: the implementor added a test-only route,
//! `GET /solr/{core}/__test_panic__`, gated behind a `test-support` Cargo
//! feature that is off by default and enabled only by this crate's own
//! `[dev-dependencies]` entry in `Cargo.toml` — so a normal `cargo build` or
//! `cargo build --release` never compiles it in, and production gains no new
//! attack surface. The route panics unconditionally and is wired through the
//! same router/middleware stack as every real handler, so this test still
//! proves the *real* production `CatchPanicLayer` catches a *real* panic,
//! independent of any single query-parsing bug's lifecycle.
//!
//! ## Why `tokio::spawn` instead of `common::get`
//!
//! `Router::oneshot` runs the handler on the calling task. A raw `.await` on
//! it would let a handler panic unwind straight through this test's own
//! async fn, which is not a meaningful "the server returned some response"
//! failure signal — it would just abort the test task, same as any other
//! panicking test. Production code does not have this problem because
//! `axum::serve` spawns each connection (and each request, within hyper's
//! per-request task model) on its own task, so a handler panic there is
//! contained to that task rather than to the whole server process. Spawning
//! the request onto its own `tokio::task` here mirrors that boundary: today,
//! the spawned task panics and `JoinHandle::await` reports it as a caught
//! `JoinError`, proving the *task* boundary contains the panic but nothing
//! in `src/` yet turns that into an HTTP response. Once the implementor adds
//! a `catch_panic`-style layer, the panic is caught even earlier (inside the
//! router/service, before it would ever unwind the task), and the spawned
//! task resolves to `Ok(response)` with a 500 Solr error envelope.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{CORE, indexed_app};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn panic_in_handler_is_caught_and_returns_solr_error_envelope() {
    let (app, _dir) = indexed_app().await;

    // A deliberate, unconditional panic via the `test-support`-gated debug
    // route (see module doc) — not the `*:*` sub-clause bug, which issue #39
    // also fixes in this same change and so no longer panics.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/solr/{CORE}/__test_panic__"))
        .body(Body::empty())
        .unwrap();

    // Spawn onto its own task so a handler panic is contained the same way
    // a real per-request task boundary would contain it (see module doc) —
    // this lets the test assert on the *outcome* (response vs. dropped
    // connection) instead of the test process itself panicking.
    let joined = tokio::spawn(async move { app.oneshot(req).await }).await;

    let response = joined
        .expect(
            "the handler panicked and nothing caught it before it crossed the \
             task boundary — issue #39 requires a panic-catching layer (e.g. \
             tower_http::catch_panic::CatchPanicLayer) in wayfinder::app's \
             router so this becomes an HTTP 500 instead of a dropped \
             connection",
        )
        .expect("oneshot must not fail at the transport level");

    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a caught handler panic must surface as HTTP 500, not any other status"
    );

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    let body: Value =
        serde_json::from_slice(&bytes).expect("panic-recovery response body must be valid JSON");

    // Match one of `WfError`'s envelope shapes (src/error.rs): an `error`
    // object carrying `msg`, `code`, and `metadata`, regardless of which of
    // the three envelope flavours (`WithParams`/`NoParams`/`Bare`) wraps it.
    let error = body
        .get("error")
        .expect("panic-recovery response must include a Solr-style `error` object");
    assert!(
        error.get("msg").and_then(Value::as_str).is_some(),
        "error.msg must be a string, got: {error:?}"
    );
    assert!(
        error.get("code").and_then(Value::as_i64).is_some(),
        "error.code must be a number, got: {error:?}"
    );
    assert!(
        error.get("metadata").and_then(Value::as_array).is_some(),
        "error.metadata must be an array, got: {error:?}"
    );
    assert_eq!(
        error.get("code").and_then(Value::as_i64),
        Some(500),
        "error.code should mirror the 500 status, per WfError::into_response"
    );
}
