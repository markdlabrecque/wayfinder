//! Central error type -> Solr error envelope (`docs/solr-ref-findings.md`
//! finding 10). Handlers construct a `WfError` and return it; nothing
//! hand-rolls an error body.
//!
//! Solr uses three different envelopes for errors, all captured in
//! `solr-ref/responses/`, and clients see the difference — so the flavour is
//! explicit rather than inferred.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::params::Params;

/// Which envelope wraps the `error` block.
#[derive(Clone, Copy, Debug)]
pub enum Envelope {
    /// `/select`-style: `responseHeader` with the echoed request params.
    /// See `err_bad_syntax.json`.
    WithParams,
    /// `/update`-style: `responseHeader`, but no `params` echo — `/update`
    /// never echoes params. See `err_update_bad_json.json`.
    NoParams,
    /// No `responseHeader` at all, just `error`. Originally the unsupported-
    /// HTTP-method envelope (see `err_update_put.json`); also used by the
    /// router's panic-catching layer (`src/lib.rs::handle_panic`), which runs
    /// outside any single request's parsed `Params` and so has nothing to
    /// echo.
    Bare,
}

/// The parts of a `WfError` that are set on only some error paths (the
/// `WithParams` envelope's echoed request, and finding 59's one
/// trace-carrying 500). Grouped and boxed as a single field on `WfError`
/// rather than inlined, so every `WfError` — and every function returning
/// `Result<_, WfError>`, which is most of `src/lib.rs`'s request-handling
/// path — carries one pointer's worth of overhead for this instead of a
/// `serde_json::Value` plus an `Option<String>`'s worth
/// (`clippy::result_large_err`).
#[derive(Default, Debug)]
struct ErrorExtra {
    params: Value,
    /// `WithParams` errors use the same envelope switch as successes.
    omit_header: bool,
    /// Issue #35: some errors are detected only after the base query has
    /// already run, so Solr's own fixture for them carries the base query's
    /// `response` block alongside `error` (e.g. `facet_unknown_field.json`).
    /// `None` when never set, which must render with no `response` key at
    /// all — not a regression for errors detected before any query runs
    /// (`facet_err_range_single.json`).
    response: Option<Value>,
    /// Set only for the one captured 500 whose error object carries `msg,
    /// trace, code` with **no** `metadata` key at all (finding 59's
    /// `err_regex_bad_class.json`: a regex that parses as a query but fails
    /// automaton compilation) — every other error here, 400 or 500, keeps
    /// the `metadata` array. `trace` itself is free text (a Java stack
    /// trace on Solr's side) the differential normaliser drops the same way
    /// it drops `error.msg` (finding 10), so its content never has to
    /// match; only its *shape* — present, and `metadata` absent — does.
    trace: Option<String>,
}

#[derive(Debug)]
pub struct WfError {
    status: StatusCode,
    /// Wayfinder-honest analogue of Solr's `root-error-class`. Solr puts a Java
    /// class name here; the values are not part of the comparison contract, the
    /// array's shape is.
    class: &'static str,
    msg: String,
    envelope: Envelope,
    /// Boxed to keep `WfError` — and therefore every `Result<_, WfError>` in
    /// the crate — small (`clippy::result_large_err`); the fields inside are
    /// set on only some error paths.
    extra: Box<ErrorExtra>,
}

impl WfError {
    pub fn new(status: StatusCode, class: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status,
            class,
            msg: msg.into(),
            envelope: Envelope::WithParams,
            extra: Box::default(),
        }
    }

    pub fn bad_request(class: &'static str, msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, class, msg)
    }

    pub fn internal(class: &'static str, msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, class, msg)
    }

    /// Attaches request state for a header-bearing envelope. `NoParams` uses
    /// only `omitHeader`; it never renders the echoed params.
    pub fn with_params(mut self, params: &Params) -> Self {
        self.extra.params = params.echo();
        self.extra.omit_header = params.omit_header();
        self
    }

    pub fn envelope(mut self, envelope: Envelope) -> Self {
        self.envelope = envelope;
        self
    }

    /// Explicitly suppresses `responseHeader`, independent of parsed request
    /// parameters. Used for invalid `omitHeader` values, whose validation
    /// error intentionally remains headerless JSON.
    pub fn suppress_response_header(mut self) -> Self {
        self.extra.omit_header = true;
        self
    }

    /// Attaches a `response` block, rendered between `responseHeader` and
    /// `error` (`WithParams` envelope only — see the module docs on issue
    /// #35).
    pub fn with_response(mut self, response: Value) -> Self {
        self.extra.response = Some(response);
        self
    }

    /// Marks this error as the one shape whose `error` object has no
    /// `metadata` key at all — see `ErrorExtra::trace`'s doc comment.
    pub fn with_trace(mut self, trace: impl Into<String>) -> Self {
        self.extra.trace = Some(trace.into());
        self
    }
}

/// Renders just `msg`, so a `WfError` raised deep inside an `anyhow`-based
/// module (`src/facet.rs`, `src/stats.rs`) survives the trip out as its Solr
/// message — the handler that catches it rebuilds the envelope anyway, from
/// `e.to_string()`. Issue #187 needs this for `facet.missing`: the parse
/// happens in `facet::facet_counts`, but the error has to come out through
/// the non-`PreQueryFacetError` path so the base query's `response` block is
/// attached (`bool_facet_missing_invalid.json`).
impl std::fmt::Display for WfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for WfError {}

impl IntoResponse for WfError {
    fn into_response(self) -> Response {
        let code = self.status.as_u16() as i64;
        // Flat alternating array, like Solr's (finding 10) — except the one
        // `trace`-carrying shape, which never had a `metadata` key to begin
        // with (`err_regex_bad_class.json`, finding 59).
        let error = match &self.extra.trace {
            Some(trace) => json!({
                "msg": self.msg,
                "trace": trace,
                "code": code,
            }),
            None => json!({
                "metadata": ["error-class", "wayfinder::Error", "root-error-class", self.class],
                "msg": self.msg,
                "code": code,
            }),
        };
        let body = match self.envelope {
            Envelope::Bare => json!({ "error": error }),
            Envelope::NoParams if self.extra.omit_header => json!({ "error": error }),
            Envelope::NoParams => json!({
                "responseHeader": { "status": code, "QTime": 0 },
                "error": error,
            }),
            Envelope::WithParams => match (self.extra.omit_header, self.extra.response) {
                (true, Some(response)) => json!({
                    "response": response,
                    "error": error,
                }),
                (true, None) => json!({ "error": error }),
                (false, Some(response)) => json!({
                    "responseHeader": { "status": code, "QTime": 0, "params": self.extra.params },
                    "response": response,
                    "error": error,
                }),
                (false, None) => json!({
                    "responseHeader": { "status": code, "QTime": 0, "params": self.extra.params },
                    "error": error,
                }),
            },
        };
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_of(resp: Response) -> Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body must be readable");
        serde_json::from_slice(&bytes).expect("body must be valid JSON")
    }

    /// Issue #35: a facet.query/facet.field error is detected *after* the base
    /// query has already run, so Solr's fixture (`facet_unknown_field.json`,
    /// `facet_err_query_single.json`) carries the base query's real `response`
    /// block alongside `error` — positioned between `responseHeader` and
    /// `error`, matching Solr's own key order. `WfError` currently has no way
    /// to attach one.
    #[tokio::test]
    async fn with_response_places_response_between_header_and_error() {
        let params = Params::parse("q=*:*&facet.field=nosuchfield&rows=0&facet=true&wt=json");
        let response_block = json!({
            "numFound": 5,
            "start": 0,
            "numFoundExact": true,
            "docs": []
        });
        let err = WfError::bad_request("undefined-field", "undefined field: \"nosuchfield\"")
            .with_params(&params)
            .with_response(response_block.clone())
            .into_response();

        let status = err.status();
        let body = body_of(err).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.get("response"),
            Some(&response_block),
            "with_response must attach the given response block, got {body}"
        );

        // Key order must be responseHeader, response, error — matching Solr's
        // fixture, since some client JSON readers are order-sensitive and the
        // repo's own `tests/json_key_order.rs` treats key order as contract.
        let keys: Vec<&str> = body
            .as_object()
            .expect("body must be a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        let header_idx = keys
            .iter()
            .position(|&k| k == "responseHeader")
            .expect("responseHeader must be present");
        let response_idx = keys
            .iter()
            .position(|&k| k == "response")
            .expect("response must be present");
        let error_idx = keys
            .iter()
            .position(|&k| k == "error")
            .expect("error must be present");
        assert!(
            header_idx < response_idx && response_idx < error_idx,
            "key order must be responseHeader, response, error; got {keys:?}"
        );
    }

    /// Existing errors that never call `with_response` must not regress: no
    /// `response` key appears at all (e.g. `facet_err_range_single.json`,
    /// where Solr detects the error before running the base query).
    #[tokio::test]
    async fn without_with_response_there_is_no_response_key() {
        let params = Params::parse("q=*:*&facet.range=tag&wt=json");
        let err =
            WfError::bad_request("range-unfacetable", "can not range facet on the text field")
                .with_params(&params)
                .into_response();

        let body = body_of(err).await;
        assert!(
            body.get("response").is_none(),
            "an error with no attached response block must not carry `response`, got {body}"
        );
    }
}
