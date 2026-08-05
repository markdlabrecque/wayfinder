//! Recovers JSON **object key order** from raw response/fixture *text*
//! (issue #25).
//!
//! Solr's envelope key order is meaningful everywhere: it serialises
//! `SimpleOrderedMap`/`NamedList`, so `responseHeader, response, facet_counts`,
//! `status, QTime, params`, `numFound, start, numFoundExact, docs`,
//! `metadata, msg, code`, `counts, gap, start, end` and the bucket order inside
//! a `json.nl=map` facet are all part of the wire contract. None of that
//! survives `serde_json::from_str::<Value>()` unless `serde_json` is built with
//! the `preserve_order` feature — with the default `BTreeMap` backing,
//! `Value::Object` silently alphabetises.
//!
//! **This module deliberately does not go through `serde_json::Value`.** Reading
//! `Value::as_object().keys()` is exactly the blind spot issue #25 exists to
//! close: it would make every assertion here a tautology that reports the map
//! type's iteration order rather than the document's. Instead `KeyOrder` has a
//! hand-written `Deserialize` impl driven by `MapAccess`/`SeqAccess`. Serde
//! yields map entries in *document* order regardless of what container the
//! caller ultimately builds, so the recorded order is the order of the bytes on
//! the wire — true whether or not `preserve_order` is enabled. The `helper_*`
//! self-tests at the top of `tests/json_key_order.rs` pin that property so this
//! module can never quietly become a no-op. (They live there, not here, because
//! `tests/common/` is compiled into every integration test binary and a
//! `#[cfg(test)] mod tests` here would run once per binary.)

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;
use std::path::Path;
use tower::ServiceExt;

/// The shape of a JSON document reduced to *just* its structure and its object
/// key order. Scalar values are discarded — other suites compare values; this
/// one compares order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOrder {
    /// Object entries in document order.
    Object(Vec<(String, KeyOrder)>),
    /// Array elements in document order.
    Array(Vec<KeyOrder>),
    /// Any scalar (string, number, bool, null).
    Scalar,
}

impl KeyOrder {
    /// Parses raw JSON *text*.
    pub fn parse(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_else(|e| panic!("parse JSON for key order: {e}"))
    }

    /// This node's object keys in document order, or `None` if it is not an
    /// object.
    pub fn keys(&self) -> Option<Vec<String>> {
        match self {
            KeyOrder::Object(entries) => Some(entries.iter().map(|(k, _)| k.clone()).collect()),
            _ => None,
        }
    }

    /// Looks up a dotted path of object keys, with `[i]` for array indices —
    /// e.g. `facet_counts.facet_ranges.views.counts` or `response.docs[0]`.
    pub fn at(&self, path: &str) -> Option<&KeyOrder> {
        let mut node = self;
        for raw in path.split('.').filter(|s| !s.is_empty()) {
            let (key, indices) = split_indices(raw);
            if !key.is_empty() {
                node = match node {
                    KeyOrder::Object(entries) => &entries.iter().find(|(k, _)| k == key)?.1,
                    _ => return None,
                };
            }
            for i in indices {
                node = match node {
                    KeyOrder::Array(items) => items.get(i)?,
                    _ => return None,
                };
            }
        }
        Some(node)
    }

    /// The object keys at `path`, panicking with the reason if the path is
    /// missing or is not an object — a silent empty result here would let a
    /// wrong query pass for the wrong reason.
    pub fn keys_at(&self, path: &str, what: &str) -> Vec<String> {
        match self.at(path) {
            Some(node) => node
                .keys()
                .unwrap_or_else(|| panic!("{what}: `{path}` is not a JSON object, got {node:?}")),
            None => panic!("{what}: no such path `{path}`"),
        }
    }
}

/// Splits `docs[0][1]` into (`docs`, `[0, 1]`).
fn split_indices(segment: &str) -> (&str, Vec<usize>) {
    match segment.find('[') {
        None => (segment, Vec::new()),
        Some(pos) => {
            let (key, rest) = segment.split_at(pos);
            let indices = rest
                .split(']')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.trim_start_matches('[')
                        .parse::<usize>()
                        .unwrap_or_else(|e| panic!("bad array index in `{segment}`: {e}"))
                })
                .collect();
            (key, indices)
        }
    }
}

impl<'de> Deserialize<'de> for KeyOrder {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        de.deserialize_any(KeyOrderVisitor)
    }
}

struct KeyOrderVisitor;

impl<'de> Visitor<'de> for KeyOrderVisitor {
    type Value = KeyOrder;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeyOrder, A::Error> {
        // `MapAccess` yields entries in document order, which is the whole
        // point: it sits upstream of whatever container a caller would build.
        let mut entries = Vec::new();
        while let Some((k, v)) = map.next_entry::<String, KeyOrder>()? {
            entries.push((k, v));
        }
        Ok(KeyOrder::Object(entries))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<KeyOrder, A::Error> {
        let mut items = Vec::new();
        while let Some(v) = seq.next_element::<KeyOrder>()? {
            items.push(v);
        }
        Ok(KeyOrder::Array(items))
    }

    fn visit_bool<E>(self, _: bool) -> Result<KeyOrder, E> {
        Ok(KeyOrder::Scalar)
    }

    fn visit_i64<E>(self, _: i64) -> Result<KeyOrder, E> {
        Ok(KeyOrder::Scalar)
    }

    fn visit_u64<E>(self, _: u64) -> Result<KeyOrder, E> {
        Ok(KeyOrder::Scalar)
    }

    fn visit_f64<E>(self, _: f64) -> Result<KeyOrder, E> {
        Ok(KeyOrder::Scalar)
    }

    fn visit_str<E>(self, _: &str) -> Result<KeyOrder, E> {
        Ok(KeyOrder::Scalar)
    }

    fn visit_unit<E>(self) -> Result<KeyOrder, E> {
        Ok(KeyOrder::Scalar)
    }

    fn visit_none<E>(self) -> Result<KeyOrder, E> {
        Ok(KeyOrder::Scalar)
    }

    fn visit_some<D: Deserializer<'de>>(self, de: D) -> Result<KeyOrder, D::Error> {
        de.deserialize_any(self)
    }
}

/// Raw text of `solr-ref/responses/<name>.json`. `common::fixture` parses to a
/// `Value`, which is precisely the step that loses the order.
pub fn fixture_text(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("solr-ref/responses")
        .join(format!("{name}.json"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// `KeyOrder` of a named fixture.
pub fn fixture_key_order(name: &str) -> KeyOrder {
    KeyOrder::parse(&fixture_text(name))
}

/// `GET /wayfinder/<core>/<path_and_query>` returning the response body as **raw
/// text**. `common::get` parses to `Value`; that is the lossy step this module
/// exists to avoid.
pub async fn get_text(app: &Router, core: &str, path_and_query: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(format!("/wayfinder/{core}/{path_and_query}"))
        .body(Body::empty())
        .unwrap();
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("request must not fail at the transport level");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("response body must be UTF-8"),
    )
}

/// True if `keys` is already in ascending byte-lexicographic order — i.e. what
/// a `BTreeMap`-backed `serde_json::Map` would produce. A vacuity guard:
/// "actual order == fixture order" proves nothing about this bug when the
/// fixture's order happens to *be* alphabetical, so tests that depend on the two
/// differing assert this is false for the fixture.
pub fn is_alphabetical(keys: &[String]) -> bool {
    keys.windows(2).all(|w| w[0] <= w[1])
}

/// Keys only real Solr emits (internal fields Wayfinder deliberately does not
/// have — findings fact 9 / PRD §7, the same set `common::normalize_envelope`
/// drops). Ignored on both sides so a whole-envelope order comparison is about
/// order, not about doc field membership.
const IGNORED_KEYS: [&str; 2] = ["_version_", "_root_"];

/// Paths exempt from key-order comparison, with the reason.
///
/// `responseHeader.params`: Solr's echoed-param order is neither the request
/// order nor alphabetical — it is Java `HashMap` iteration order. See
/// `facet_range_json_nl_map.json`, whose params come back as `facet.range, q,
/// facet.range.gap, json.nl, rows, facet, wt, facet.range.start,
/// facet.range.end`. No implementation can reproduce that, so it is not a
/// contract; this mirrors findings fact 6 ("the fixture normaliser must be
/// order-insensitive on this object"). Every *other* object in the envelope is
/// ordered on purpose and is compared.
const EXEMPT_PATHS: [&str; 1] = ["responseHeader.params"];

/// Recursively asserts that `actual_text`'s object key order equals
/// `fixture_name`'s, everywhere both documents have an object.
///
/// Array lengths and scalar values are *not* asserted (other suites own those);
/// elements are compared pairwise over the common prefix, and objects present on
/// only one side are skipped rather than failed, for the same reason.
pub fn assert_same_key_order(actual_text: &str, fixture_name: &str) {
    assert_same_key_order_texts(actual_text, &fixture_text(fixture_name), fixture_name);
}

/// Test-support variant of `assert_same_key_order` that compares two raw
/// JSON texts directly, rather than looking the "expected" side up as a
/// named fixture under `solr-ref/responses/`. `assert_same_key_order` is
/// exactly this with `fixture_text(fixture_name)` as the expected side; this
/// exists so regression tests that need a synthetic "expected" text (e.g. the
/// `IGNORED_KEYS` path-scoping regression, which needs a `_version_` outside
/// `response.docs[*]`) do not need a committed fixture to exercise the
/// comparison logic. No behaviour change from the pre-existing
/// `assert_same_key_order` body.
pub fn assert_same_key_order_texts(actual_text: &str, expected_text: &str, label: &str) {
    let actual = KeyOrder::parse(actual_text);
    let expected = KeyOrder::parse(expected_text);
    let mut checked = 0usize;
    compare(&actual, &expected, "", label, &mut checked);
    assert!(
        checked > 0,
        "assert_same_key_order_texts({label}) compared no objects at all - \
         the comparison is vacuous, which is a bug in the test, not the code"
    );
}

fn compare(actual: &KeyOrder, expected: &KeyOrder, path: &str, fixture: &str, checked: &mut usize) {
    if EXEMPT_PATHS.contains(&path) {
        return;
    }
    match (actual, expected) {
        (KeyOrder::Object(a), KeyOrder::Object(e)) => {
            // `IGNORED_KEYS` (`_version_`/`_root_`) is only ever a legitimate
            // Wayfinder omission *inside* `response.docs[<i>]` (findings fact
            // 9) or `/mlt`'s `match.docs[<i>]` (issue #6, same doc shape) —
            // elsewhere, e.g. at the top level, a `_version_` key present on
            // only one side is a real key-order mismatch and must not be
            // silently filtered away from both sides before the comparison
            // (issue #31 follow-up 1).
            let scope_ignored = is_response_docs_entry(path);
            let got = filtered_keys(a, scope_ignored);
            let want = filtered_keys(e, scope_ignored);
            *checked += 1;
            assert_eq!(
                got,
                want,
                "key order at `{}` does not match fixture `{fixture}`",
                display_path(path)
            );
            for (key, e_child) in e {
                if scope_ignored && IGNORED_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if let Some((_, a_child)) = a.iter().find(|(k, _)| k == key) {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    compare(a_child, e_child, &child_path, fixture, checked);
                }
            }
        }
        (KeyOrder::Array(a), KeyOrder::Array(e)) => {
            for (i, (a_item, e_item)) in a.iter().zip(e.iter()).enumerate() {
                compare(a_item, e_item, &format!("{path}[{i}]"), fixture, checked);
            }
        }
        _ => {}
    }
}

/// True if `path` is exactly `response.docs[<i>]` or `match.docs[<i>]` for
/// some array index `i` — the places `IGNORED_KEYS` applies (see `compare`
/// above), not any depth under either or anywhere else in the envelope.
/// `match.docs[<i>]` was added for `/mlt` (issue #6): `match` is its own
/// nested search-result object, so it carries the same real-doc internal
/// fields (`_version_`/`_root_`) `response.docs[<i>]` does, per
/// `tests/mlt.rs`'s `normalize_mlt` doc comment.
fn is_response_docs_entry(path: &str) -> bool {
    for prefix in ["response.docs[", "match.docs["] {
        if let Some(rest) = path.strip_prefix(prefix)
            && let Some(idx) = rest.strip_suffix(']')
            && !idx.is_empty()
            && idx.bytes().all(|b| b.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

fn filtered_keys(entries: &[(String, KeyOrder)], scope_ignored: bool) -> Vec<&str> {
    entries
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !(scope_ignored && IGNORED_KEYS.contains(k)))
        .collect()
}

fn display_path(path: &str) -> &str {
    if path.is_empty() { "<root>" } else { path }
}
