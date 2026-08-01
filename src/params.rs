//! Minimal `application/x-www-form-urlencoded` query-string parsing.
//!
//! Solr's params echo (findings fact 6) needs the raw request preserved as
//! strings, with repeated keys turned into an array — so we parse into an
//! ordered list of pairs rather than a plain map, and let callers decide how
//! to fold that into `responseHeader.params`.

use serde_json::{Map, Value, json};

use crate::error::WfError;

/// One decoded `key=value` pair from a query string, in request order.
#[derive(Debug, Clone)]
pub struct Params {
    pairs: Vec<(String, String)>,
    /// Only endpoints that allowlist `omitHeader` may let it affect an
    /// envelope. This keeps an unsupported spelling inert on admin routes.
    omit_header_allowed: bool,
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
        Params {
            pairs,
            omit_header_allowed: false,
        }
    }

    /// Enables `omitHeader` envelope handling for an endpoint that implements
    /// and allowlists the parameter.
    pub fn allow_omit_header(mut self) -> Self {
        self.omit_header_allowed = true;
        self
    }

    /// First value for `key`, if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Reads `key` as a boolean through [`parse_bool`], or `None` when the
    /// param is absent. An invalid value is Solr's 400 `invalid boolean
    /// value: <raw>` (`bool_facet_invalid.json`), with this request's params
    /// already echoed onto it — the `WithParams` envelope is what every
    /// `/select`-family caller wants; `/update`'s two callers re-stamp
    /// `Envelope::NoParams` on top.
    pub fn bool_opt(&self, key: &str) -> Result<Option<bool>, WfError> {
        match self.get(key) {
            None => Ok(None),
            Some(raw) => match parse_bool(raw) {
                Some(value) => Ok(Some(value)),
                None => Err(WfError::bad_request(
                    "wayfinder::InvalidBoolean",
                    invalid_bool_msg(raw),
                )
                .with_params(self)),
            },
        }
    }

    /// [`Params::bool_opt`] with the param's Solr default filled in. Most
    /// boolean params default to `false`; `overwrite` and `mlt.match.include`
    /// default to `true`.
    pub fn bool_or(&self, key: &str, default: bool) -> Result<bool, WfError> {
        Ok(self.bool_opt(key)?.unwrap_or(default))
    }

    /// [`Params::bool_opt`] over Solr's per-field override form
    /// `f.<field>.<param>` — e.g. `per_field_bool("category",
    /// "facet.missing")` reads `f.category.facet.missing`. `None` when no
    /// override was sent, so callers can fall back to the global param.
    ///
    /// Keys off the *field* being faceted, never a `{!key=...}` response label
    /// (finding 97, and issue #138's `facet_local_params_key_f_field.json` /
    /// `_f_key.json`): callers must pass the resolved field name.
    pub fn per_field_bool(&self, field: &str, param: &str) -> Result<Option<bool>, WfError> {
        self.bool_opt(&format!("f.{field}.{param}"))
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

    /// Whether this endpoint's `omitHeader` policy suppresses `responseHeader`.
    ///
    /// An unsupported parameter or invalid value remains inert. Invalid values
    /// are explicitly suppressed by their validation-error policy instead.
    ///
    /// Ground truth for the accept side is `bool_omit_header_yes.json`
    /// (issue #187) plus `search_api_solr`'s own traffic
    /// (`solr-ref/search-api/trace/`): all twenty traces that send
    /// `omitHeader=true` (`00002`-`00019`, `00021` on `/select`, `00022` on
    /// `/mlt`, plus `00028` on `/terms`) have responses with no
    /// `responseHeader` key at all, while `00001` (`/update`,
    /// `omitHeader=false`) does carry one. Issue #214 settled the error side.
    pub fn omit_header(&self) -> bool {
        self.omit_header_allowed && matches!(self.parse_omit_header(), Ok(true))
    }

    /// Validates `omitHeader` against Solr's boolean vocabulary -- the shared
    /// [`parse_bool`], so this parameter accepts exactly what every other
    /// boolean param does (issue #187, finding 113): `true`/`on`/`yes` and
    /// `false`/`off` by case-insensitive prefix, `no` exactly, and nothing
    /// else. Numeric and single-letter spellings (`1`, `t`, `y`) stay invalid
    /// here, as they are everywhere else.
    pub fn validate_omit_header(&self) -> Result<(), &str> {
        self.parse_omit_header().map(|_| ())
    }

    fn parse_omit_header(&self) -> Result<bool, &str> {
        match self.get("omitHeader") {
            None => Ok(false),
            Some(value) => parse_bool(value).ok_or(value),
        }
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

/// Solr's `StrUtils.parseBool`, as measured against real `solr:9` for issue
/// #187 (finding 113) — the one parser every boolean request param in this
/// crate goes through. `None` means "invalid", which callers turn into Solr's
/// 400 `invalid boolean value: <raw>`.
///
/// On the value lowercased:
/// - starts with `true`, `on` or `yes` → `true` (`TRUE`, `oN`, `truestuff`,
///   `onward`, `yesss` are all true)
/// - starts with `false` or `off`, or *equals* `no` → `false` (`offside`,
///   `falsey`, `NO` are false)
/// - anything else, including the empty string, is invalid
///
/// The `no` arm is an exact match, not a prefix: `noo` is invalid, which is
/// why this cannot be collapsed into the `false`/`off` prefix list. The
/// ticket's own premise — that Solr accepts `1`/`0`/`t`/`f`/`y` — is wrong;
/// captured Solr 400s on all five.
pub fn parse_bool(raw: &str) -> Option<bool> {
    let value = raw.to_ascii_lowercase();
    if value.starts_with("true") || value.starts_with("on") || value.starts_with("yes") {
        Some(true)
    } else if value.starts_with("false") || value.starts_with("off") || value == "no" {
        Some(false)
    } else {
        None
    }
}

/// Solr's verbatim wording for a value [`parse_bool`] rejects
/// (`bool_facet_invalid.json`, `bool_facet_missing_invalid.json`). Shared so
/// the `WfError` path in `Params` and the `anyhow` path in `src/facet.rs`
/// cannot drift apart.
pub fn invalid_bool_msg(raw: &str) -> String {
    format!("invalid boolean value: {raw}")
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

#[cfg(test)]
mod tests {
    use super::split_per_field_key;

    /// The honoured list `check_params` (`src/lib.rs`) passes in. Kept local
    /// so these cases exercise the split itself, not `PER_FIELD_PARAMS`'s
    /// current contents.
    const HONOURED: &[&str] = &["facet.missing"];

    /// The plain case: one undotted field, one honoured base param.
    #[test]
    fn splits_a_simple_field_from_its_base_param() {
        assert_eq!(
            split_per_field_key("f.category.facet.missing", HONOURED),
            Some(("category", "facet.missing"))
        );
    }

    /// Why the split is anchored on the suffix and not the first `.`: dotted
    /// dynamic field names are real here (`src/schema.rs`'s patterns, issue
    /// #180), and a `split_once('.')` would hand back `a` plus a base param of
    /// `b.facet.missing`, which is in no honoured list -- so the override
    /// would silently 400 under `strict_params` and be ignored otherwise.
    #[test]
    fn anchors_on_the_suffix_so_dotted_field_names_survive() {
        assert_eq!(
            split_per_field_key("f.a.b.facet.missing", HONOURED),
            Some(("a.b", "facet.missing"))
        );
        assert_eq!(
            split_per_field_key("f.ss_field.name.facet.missing", HONOURED),
            Some(("ss_field.name", "facet.missing"))
        );
    }

    /// An empty field name is not a field. `f..facet.missing` must not
    /// resolve to `Some(("", "facet.missing"))` -- nothing downstream can
    /// look that up, and accepting it would let `strict_params` wave through
    /// a malformed param.
    #[test]
    fn rejects_an_empty_field_name() {
        assert_eq!(split_per_field_key("f..facet.missing", HONOURED), None);
    }

    /// `f.facet.missing` has no field segment at all -- the base param sits
    /// directly against the `f.` prefix.
    #[test]
    fn rejects_a_key_with_no_field_segment() {
        assert_eq!(split_per_field_key("f.facet.missing", HONOURED), None);
    }

    /// The base param has to be the whole suffix. Trailing text after it is a
    /// different param name, not a per-field override of this one.
    #[test]
    fn rejects_trailing_text_after_the_base_param() {
        assert_eq!(
            split_per_field_key("f.x.facet.missing.extra", HONOURED),
            None
        );
    }

    /// A base param outside the honoured list gets no shape match, however
    /// well-formed the key is -- this is what keeps unimplemented
    /// `f.<field>.facet.limit` 400ing under `strict_params`.
    #[test]
    fn rejects_a_base_param_outside_the_honoured_list() {
        assert_eq!(
            split_per_field_key("f.category.facet.limit", HONOURED),
            None
        );
    }

    /// No `f.` prefix, no per-field shape.
    #[test]
    fn rejects_a_key_without_the_f_prefix() {
        assert_eq!(split_per_field_key("facet.missing", HONOURED), None);
        assert_eq!(
            split_per_field_key("fx.category.facet.missing", HONOURED),
            None
        );
    }
}

/// Issue #187: the shared boolean-param parser must match real Solr 9's
/// `StrUtils.parseBool`, not the stricter `== "true"`/`starts_with("true")`
/// checks scattered through `src/lib.rs`/`src/facet.rs` today.
///
/// Ground truth is `docs/solr-ref-findings.md`'s finding for this issue
/// (captured against real `solr:9`, port 8996, 2026-08-01), not the ticket's
/// own premise — the ticket claims Solr accepts `1`/`0`/`t`/`f`/`y`, which is
/// wrong; measured behaviour is:
/// - `true` if the lowercased value *starts with* `true`, `on`, or `yes`
/// - `false` if it *starts with* `false` or `off`, or *equals* `no` exactly
/// - anything else, including the empty string, is invalid
///
/// **Interpretation this test file has to make**: the spec names a shared
/// parser in `src/params.rs` but not its exact signature. These tests call
/// `parse_bool(raw: &str) -> Option<bool>` — `None` for anything invalid,
/// leaving the `WfError`/`"invalid boolean value: <raw>"` construction to the
/// `Params` accessor that calls it (which has the raw string in scope to
/// format the message; the parser itself does not need to know it). This is
/// the free function the module does not yet define — a compile failure
/// (`cannot find function \`parse_bool\``) is the expected red here, not a
/// test assertion failure, until it exists.
#[cfg(test)]
mod parse_bool_tests {
    use super::parse_bool;

    /// `true`/`TRUE`/`True`/`tRuE` (case-insensitivity), `truestuff` (a
    /// `true`-prefixed value, not just the exact word), and the `on`/`yes`
    /// families with the same case- and prefix-insensitivity.
    const TRUE_VALUES: &[&str] = &[
        "true",
        "TRUE",
        "True",
        "tRuE",
        "truestuff",
        "on",
        "ON",
        "onward",
        "yes",
        "YES",
        "yesss",
    ];

    /// `false`/`falsey`, `off`/`offside`, and the one exact-match exception:
    /// `no` (and its case variants) is false, but — per `INVALID_VALUES`
    /// below — `noo` is NOT, so `no` cannot be treated as merely another
    /// `false`-prefixed family.
    const FALSE_VALUES: &[&str] = &["false", "falsey", "off", "offside", "no", "NO", "No"];

    /// Everything the ticket's own premise wrongly claimed Solr accepts
    /// (`1`, `0`, `t`, `f`, `y`), plus `nope` (not `no`- or `false`-prefixed),
    /// `noo` (not exactly `no`), `maybe`, `2`, and the empty string.
    const INVALID_VALUES: &[&str] = &["1", "0", "t", "f", "y", "nope", "noo", "maybe", "2", ""];

    #[test]
    fn accepts_true_prefixed_case_insensitive_values() {
        for v in TRUE_VALUES {
            assert_eq!(parse_bool(v), Some(true), "expected `{v}` to parse as true");
        }
    }

    #[test]
    fn accepts_false_prefixed_values_and_the_exact_no_exception() {
        for v in FALSE_VALUES {
            assert_eq!(
                parse_bool(v),
                Some(false),
                "expected `{v}` to parse as false"
            );
        }
    }

    #[test]
    fn rejects_everything_solr_rejects_including_1_0_t_f_y_noo_and_empty() {
        for v in INVALID_VALUES {
            assert_eq!(
                parse_bool(v),
                None,
                "expected `{v}` to be rejected -- Solr's real StrUtils.parseBool does NOT \
                 accept 1/0/t/f/y (the ticket's premise is wrong; see docs/solr-ref-findings.md)"
            );
        }
    }
}
