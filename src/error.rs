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
#[derive(Clone, Copy)]
pub enum Envelope {
    /// `/select`-style: `responseHeader` with the echoed request params.
    /// See `err_bad_syntax.json`.
    WithParams,
    /// `/update`-style: `responseHeader`, but no `params` echo — `/update`
    /// never echoes params. See `err_update_bad_json.json`.
    NoParams,
    /// Unsupported HTTP method: no `responseHeader` at all, just `error`.
    /// See `err_update_put.json`.
    Bare,
}

pub struct WfError {
    status: StatusCode,
    /// Wayfinder-honest analogue of Solr's `root-error-class`. Solr puts a Java
    /// class name here; the values are not part of the comparison contract, the
    /// array's shape is.
    class: &'static str,
    msg: String,
    envelope: Envelope,
    params: Value,
}

impl WfError {
    pub fn new(status: StatusCode, class: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status,
            class,
            msg: msg.into(),
            envelope: Envelope::WithParams,
            params: Value::Object(serde_json::Map::new()),
        }
    }

    pub fn bad_request(class: &'static str, msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, class, msg)
    }

    pub fn internal(class: &'static str, msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, class, msg)
    }

    /// Attaches the request params for the `WithParams` envelope.
    pub fn with_params(mut self, params: &Params) -> Self {
        self.params = params.echo();
        self
    }

    pub fn envelope(mut self, envelope: Envelope) -> Self {
        self.envelope = envelope;
        self
    }
}

impl IntoResponse for WfError {
    fn into_response(self) -> Response {
        let code = self.status.as_u16() as i64;
        // Flat alternating array, like Solr's (finding 10).
        let error = json!({
            "metadata": ["error-class", "wayfinder::Error", "root-error-class", self.class],
            "msg": self.msg,
            "code": code,
        });
        let body = match self.envelope {
            Envelope::Bare => json!({ "error": error }),
            Envelope::NoParams => json!({
                "responseHeader": { "status": code, "QTime": 0 },
                "error": error,
            }),
            Envelope::WithParams => json!({
                "responseHeader": { "status": code, "QTime": 0, "params": self.params },
                "error": error,
            }),
        };
        (self.status, Json(body)).into_response()
    }
}
