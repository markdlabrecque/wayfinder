//! Minimal `application/x-www-form-urlencoded` query-string parsing.
//!
//! Solr's params echo (findings fact 6) needs the raw request preserved as
//! strings, with repeated keys turned into an array — so we parse into an
//! ordered list of pairs rather than a plain map, and let callers decide how
//! to fold that into `responseHeader.params`.

use serde_json::{Map, Value, json};

/// One decoded `key=value` pair from a query string, in request order.
#[derive(Debug, Clone)]
pub struct Params {
    pairs: Vec<(String, String)>,
}

impl Params {
    pub fn parse(query: &str) -> Params {
        let mut pairs = Vec::new();
        for segment in query.split('&') {
            if segment.is_empty() {
                continue;
            }
            let (key, value) = match segment.split_once('=') {
                Some((k, v)) => (k, v),
                None => (segment, ""),
            };
            pairs.push((decode(key), decode(value)));
        }
        Params { pairs }
    }

    /// First value for `key`, if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// First value of Solr's per-field override form `f.<field>.<param>`, if
    /// present — e.g. `per_field("category", "facet.missing")` reads
    /// `f.category.facet.missing`.
    ///
    /// Keys off the *field* being faceted, never a `{!key=...}` response label
    /// (finding 97, and issue #138's `facet_local_params_key_f_field.json` /
    /// `_f_key.json`): callers must pass the resolved field name.
    pub fn per_field(&self, field: &str, param: &str) -> Option<&str> {
        self.get(&format!("f.{field}.{param}"))
    }

    /// All values for `key`, in request order (for repeatable params like `fq`).
    pub fn get_all(&self, key: &str) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Every param key present, in request order (repeated keys repeat).
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.pairs.iter().map(|(k, _)| k.as_str())
    }

    /// Solr's `omitHeader`: `true` drops `responseHeader` from the response
    /// entirely. Anything else — `false`, absent, an unrecognized value — keeps
    /// it.
    ///
    /// That exact-string strictness is *this codebase's* convention, not
    /// Solr's: `== Some("true")` is how every other boolean param is read
    /// (`src/lib.rs`'s `commit`, `softCommit`, `facet`, `stats`, `hl`,
    /// `mlt.boost`, `terms`). Solr itself is laxer — `StrUtils.parseBool`
    /// accepts `1`, `t`, `yes` and is case-insensitive, so `omitHeader=1`
    /// suppresses the header in real Solr and does not here.
    ///
    /// ponytail: that is a real, unfixtured divergence. No fixture exercises
    /// it — `search_api_solr` only ever sends the literal `true`/`false` (all
    /// 28 traces), and no `manifest.tsv` row uses `omitHeader` at all — so
    /// widening it here would be guessing at behaviour nothing captured
    /// confirms, and would diverge from the sibling params above for no
    /// evidenced gain. Settling it belongs with the other open `omitHeader`
    /// question in issue #179: one `capture.sh` block covering `omitHeader=1`
    /// alongside the error-envelope case answers both, after which either
    /// widen all the boolean reads together or pin the strictness with a test.
    ///
    /// Ground truth is `search_api_solr`'s own traffic
    /// (`solr-ref/search-api/trace/`): all twenty traces that send
    /// `omitHeader=true` (`00002`-`00019`, `00021` on `/select`, `00022` on
    /// `/mlt`, plus `00028` on `/terms`) have responses with no
    /// `responseHeader` key at all, while `00001` (`/update`,
    /// `omitHeader=false`) does carry one.
    ///
    /// ponytail: **success responses only.** Every error envelope
    /// (`src/error.rs`'s `WfError`) still carries its `responseHeader`
    /// unconditionally, whatever `omitHeader` says. That is not a decision
    /// backed by evidence, it is the absence of one: all 28 captured traces
    /// are 200s, and neither `solr-ref/manifest.tsv` nor
    /// `solr-ref/manifest-errors.tsv` has a single row using `omitHeader`, so
    /// no fixture shows whether real Solr suppresses the header on an error.
    /// The ceiling is deliberate — leaving the landed error-envelope shape
    /// untouched beats guessing. What would settle it: a `capture.sh` block
    /// issuing an erroring request (say `select?q=*:*&facet=true&facet.field=nope`)
    /// with `omitHeader=true`, captured against real `solr:9`. If it shows
    /// suppression, thread this check into `WfError`'s rendering; if not,
    /// pin the current behaviour with a test.
    pub fn omit_header(&self) -> bool {
        self.get("omitHeader") == Some("true")
    }

    /// Renders `responseHeader.params` per findings fact 5/6: raw string
    /// values, repeated keys folded into a JSON array.
    pub fn echo(&self) -> Value {
        let mut map = Map::new();
        for (key, value) in &self.pairs {
            match map.get_mut(key) {
                None => {
                    map.insert(key.clone(), json!(value));
                }
                Some(Value::Array(arr)) => arr.push(json!(value)),
                Some(existing) => {
                    let prev = existing.clone();
                    *existing = json!([prev, value]);
                }
            }
        }
        Value::Object(map)
    }
}

/// Recognises Solr's per-field override shape `f.<field>.<param>` for the
/// base params in `honoured`, returning the field and the base param it
/// overrides. Anything not of that shape, or naming a base param outside
/// `honoured`, is `None`.
///
/// The split is anchored on the *suffix*, not the first `.`, because field
/// names may themselves contain dots — a dotted dynamic field
/// (`f.ss_field.name.facet.missing`, see `src/schema.rs`'s dynamic patterns)
/// would otherwise be truncated to `ss_field`.
pub fn split_per_field_key<'a, 'b>(
    key: &'a str,
    honoured: &'b [&'b str],
) -> Option<(&'a str, &'b str)> {
    let rest = key.strip_prefix("f.")?;
    honoured.iter().find_map(|param| {
        let field = rest.strip_suffix(param)?.strip_suffix('.')?;
        (!field.is_empty()).then_some((field, *param))
    })
}

/// Decodes `application/x-www-form-urlencoded`: `+` is a space, `%XX` is a
/// hex-encoded byte. Invalid escapes are passed through literally.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = (i + 2 < bytes.len())
                    .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
                    .flatten()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
