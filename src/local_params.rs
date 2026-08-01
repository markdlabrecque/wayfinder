//! Solr local-params blocks inside a param *value* (issue #137):
//! `{!edismax qf='fieldA^1 fieldB^1'}`.
//!
//! ## What the captured traces actually need, and why this is not a general
//! local-params engine
//!
//! `/select`'s captured handler defaults
//! (`solr-ref/search-api/configset/solrconfig_extra.xml:110-118`) are
//! `defType=lucene`, `df=id`, `omitHeader=true`. So in every Shape B trace
//! (`solr-ref/search-api/trace/00003-00008`, `00021`) the outer parser is
//! **lucene**, and `{!edismax ...}` appears *inside* the query text rather
//! than at position 0 of `q` — it is an **inline nested query**, not a
//! position-0 local-params block that would re-select the whole query's
//! parser. The leading `(` around each captured `q` is therefore irrelevant:
//! the block was never going to be a position-0 block regardless.
//!
//! Lucene's `_TERM_CHAR` set makes an inline nested query bind exactly the
//! **next whitespace-delimited run** of characters (`+`, `-` and quotes are
//! ordinary term characters mid-token, so `{!edismax ...}+"quick"` binds
//! `+"quick"` whole). Everything *after* that run is parsed by the outer
//! lucene parser against `df`, which in the capture is `id` and so matches
//! nothing. That single rule explains all seven captured traces and nothing
//! else was needed:
//!
//! | trace | text after `}`       | numFound | under the rule                             |
//! |-------|----------------------|----------|--------------------------------------------|
//! | 00006 | `+"quick"`           | 2        | edismax(`+"quick"`)                        |
//! | 00005 | `"quick" "rocket"`   | 2        | edismax(`"quick"`) OR `id:"rocket"` (no hit) |
//! | 00007 | `"quick" "rocket"`   | 2        | duplicate of 00005                         |
//! | 00003 | `+"quick" +"rocket"` | **0**    | edismax(`+"quick"`) AND `id:"rocket"` -> 0  |
//! | 00004 | `+"quick" +"fox"`    | **0**    | edismax(`+"quick"`) AND `id:"fox"` -> 0     |
//! | 00008 | `+"quick" +"fox"`    | **0**    | duplicate of 00004                         |
//! | 00021 | `+"qwick"`           | 0        | typo, no term match                        |
//!
//! Trace 00004/00008 is the decisive one, and it is a **bug in
//! `search_api_solr`, faithfully reproduced here**: `entity:node/1` contains
//! both "quick" and "fox", so an edismax applied to the whole remainder would
//! return it. Real Solr returns 0 because `+"fox"` never reaches edismax at
//! all. Per `CLAUDE.md`'s compatibility contract the fixtures are ground
//! truth, so this module reproduces the low-recall outcome rather than
//! "fixing" it — see `tests/local_params.rs`.
//!
//! ## Scope, and where the ceiling is
//!
//! ponytail: only `{!edismax ...}` is recognised as a nested-query type, and
//! only the `qf` local param is consulted. Every other type
//! (`{!lucene}`, `{!term}`, `{!func}`, ...) and a type-less block
//! (`{!qf=title}`) is a hard 400 `SyntaxError`, deliberately: `{!func}` is
//! PRD v4 scope and issue #137's open question 5 requires it must not
//! silently half-work, and nothing in the capture sends any other type. A
//! `v='...'` local param (Solr's way of supplying the nested query text
//! inline instead of binding the following token) is likewise not
//! implemented; no capture uses it, and it would currently bind the
//! following token anyway.
//!
//! ponytail: the bound run ends at the first whitespace *at any paren depth*,
//! so a run that itself contains whitespace inside parens —
//! `({!edismax qf='...'}(+"quick" +"fox"))` — is cut at that whitespace and
//! leaves the outer parser an unbalanced `+"fox"))`, i.e. a 400. No captured
//! trace sends a bound run containing whitespace inside parens, so what real
//! Solr answers for it is unverified here and nothing pins it; the 400 is the
//! ceiling, not a claim about Solr.
//!
//! The block grammar itself (`parse_block`) is general — type plus
//! `k=v`/`k='v v'`/`k="v v"` pairs — because issue #138 needs the same
//! grammar for `{!key=X}field` in `facet.field`. Only the *nested-query*
//! wiring below is `q`-specific.

/// One parsed `{!type k=v ...}` block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalParams {
    /// The leading bare token, i.e. the `edismax` in `{!edismax qf=...}`.
    /// `None` for a type-less block such as `{!key=Foo}` (issue #138's shape).
    pub query_type: Option<String>,
    /// `k=v` pairs in the order written.
    pub params: Vec<(String, String)>,
}

impl LocalParams {
    /// First value for `key`, if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A `{!...}` block plus the query text bound to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedQuery {
    pub local: LocalParams,
    /// The next whitespace-delimited run after `}` — see the module doc's
    /// binding rule. Empty when `}` is immediately followed by whitespace or
    /// end of input.
    pub text: String,
}

/// `q` with each inline nested query replaced by a sentinel literal, plus the
/// nested queries themselves in sentinel order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewritten {
    /// The outer query string, still lucene syntax, with `self.sentinel(i)`
    /// standing in for `nested[i]`.
    pub outer: String,
    pub nested: Vec<NestedQuery>,
    /// The sentinel prefix this rewrite was keyed with — `SENTINEL_PREFIX`,
    /// extended if the input already contained it (see `unique_prefix`).
    /// Callers must resolve sentinels through `sentinel_index` against *this*
    /// prefix, never against the base constant.
    pub sentinel_prefix: String,
}

impl Rewritten {
    /// The placeholder term standing in for `nested[i]` in `outer`. Test-only:
    /// production code resolves sentinels in the other direction, via
    /// `sentinel_index` against `sentinel_prefix`.
    #[cfg(test)]
    pub fn sentinel(&self, i: usize) -> String {
        sentinel_with(&self.sentinel_prefix, i)
    }
}

/// Base prefix of the placeholder term substituted for a nested query.
/// Deliberately built only from characters the lucene grammar does not treat
/// as special, so the outer parser sees an ordinary optional/required/excluded
/// clause in exactly the position the nested query occupied — that is what
/// makes the surrounding `+`/`-`/parens structure fall out for free instead of
/// needing to be re-derived.
const SENTINEL_PREFIX: &str = "__wf_nested_query_";

/// A sentinel prefix that cannot occur in `q`, so no user-supplied text can be
/// mistaken for a placeholder.
///
/// Without this, a `q` containing the literal `__wf_nested_query_0__` had that
/// text resolved to nested query 0 and answered a *different query than the
/// one asked* — `({!edismax qf='...'}+"quick" +__wf_nested_query_0__)` returned
/// 2 where real Solr parses `+__wf_nested_query_0__` as a mandatory term
/// against `df=id`, matches nothing, and returns 0. Re-keying rather than
/// erroring keeps that Solr semantics: the reserved-looking token stays
/// ordinary outer-parser text, which is exactly what Solr does with it.
///
/// The prefix is extended one `_` at a time until it is absent from `q`, which
/// terminates because a prefix longer than `q` cannot be contained in it. Since
/// the returned prefix is not a substring of `q`, no substring of `q` can equal
/// `prefix + i + "__"` either, so the whole sentinel is collision-free.
fn unique_prefix(q: &str) -> String {
    let mut prefix = String::from(SENTINEL_PREFIX);
    while q.contains(&prefix) {
        prefix.push('_');
    }
    prefix
}

/// The placeholder term for nested query `i` under `prefix`.
fn sentinel_with(prefix: &str, i: usize) -> String {
    format!("{prefix}{i}__")
}

/// The nested-query index a leaf's literal text refers to, if it is a sentinel
/// of `prefix`. An empty `prefix` never matches: it is the "no nested queries
/// were lifted" case, where no literal may resolve to anything.
pub fn sentinel_index(prefix: &str, phrase: &str) -> Option<usize> {
    if prefix.is_empty() {
        return None;
    }
    phrase
        .strip_prefix(prefix)?
        .strip_suffix("__")?
        .parse()
        .ok()
}

/// Parses one `{!...}` block at the start of `s`.
///
/// Returns the parsed block and the byte length consumed (through the closing
/// `}`). `None` if `s` does not start with `{!` or the block is unterminated —
/// callers treat an unterminated block as "not a local-params block at all"
/// and leave the text to the ordinary query parser, which is what produces the
/// pre-existing 400 rather than a new bespoke one.
pub fn parse_block(s: &str) -> Option<(LocalParams, usize)> {
    let body_start = s.strip_prefix("{!").map(|_| 2)?;
    let bytes = s.as_bytes();
    let end = find_block_end(bytes, body_start)?;
    let mut local = LocalParams::default();
    for (key, value) in split_pairs(&s[body_start..end]) {
        match key {
            None => {
                if local.query_type.is_none() && local.params.is_empty() {
                    local.query_type = Some(value);
                }
                // A later bare token (`{!edismax bogus}`) is dropped: Solr
                // only ever reads the *first* as the parser name.
            }
            Some(key) => local.params.push((key, value)),
        }
    }
    Some((local, end + 1))
}

/// Byte index of the `}` closing a block whose body starts at `from`, skipping
/// any `}` inside a quoted value.
fn find_block_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' {
                    i += 1;
                } else if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'}' => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Splits a block body into `(Some(key), value)` pairs, plus `(None, token)`
/// for a bare token such as the leading parser name. Values may be single- or
/// double-quoted, which is what lets `qf='a^1 b^1'` carry spaces.
fn split_pairs(body: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    let mut chars = body.char_indices().peekable();
    while let Some(&(_, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        // Key (or bare token): up to `=` or whitespace.
        let mut key = String::new();
        let mut saw_eq = false;
        while let Some(&(_, c)) = chars.peek() {
            if c == '=' {
                chars.next();
                saw_eq = true;
                break;
            }
            if c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }
        if !saw_eq {
            if !key.is_empty() {
                out.push((None, key));
            }
            continue;
        }
        let value = read_value(&mut chars);
        out.push((Some(key), value));
    }
    out
}

/// Reads one local-param value: a `'`/`"`-quoted run (quotes stripped,
/// `\`-escapes honoured) or a bare run up to the next whitespace.
fn read_value<I>(chars: &mut std::iter::Peekable<I>) -> String
where
    I: Iterator<Item = (usize, char)>,
{
    let mut value = String::new();
    match chars.peek() {
        Some(&(_, q @ ('\'' | '"'))) => {
            chars.next();
            let mut escaped = false;
            for (_, c) in chars.by_ref() {
                if escaped {
                    value.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    break;
                } else {
                    value.push(c);
                }
            }
        }
        _ => {
            while let Some(&(_, c)) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
    }
    value
}

/// Byte length of the run bound to an inline nested query — everything
/// immediately after the block's `}` up to, but not including, the first
/// terminator.
///
/// Terminators, all of them capture-derived rather than assumed:
/// - whitespace outside a quoted phrase, *at any paren depth*.
///   `+"quick" +"rocket"` (traces 00003/00004) binds `+"quick"` only, which is
///   the whole reason those traces answer 0. Because the whitespace arm does
///   not consult `depth`, a run that opens a paren and then contains
///   whitespace is still cut at that whitespace: `(+"quick" +"fox"))` binds
///   `(+"quick"` and leaves the outer parser unbalanced text, i.e. a 400. No
///   captured trace sends that shape — see the module doc's ponytail ceiling.
/// - a `)` at paren depth 0, i.e. one that would close a paren opened *before*
///   the run. Every captured `q` wraps the whole query in `(...)`, so trace
///   00006's `({!edismax ...}+"quick")` has no whitespace after the bound run
///   at all; without this the run would swallow the closing paren and fail to
///   parse. A `)` matching a `(` opened inside the run does not terminate.
///
/// Note `"` is deliberately *not* a terminator: every captured bound run is a
/// quoted phrase, optionally `+`-prefixed, and the quotes belong to the nested
/// query's own text.
fn bound_token_len(rest: &str) -> usize {
    let mut depth = 0usize;
    let mut in_quote = false;
    for (i, c) in rest.char_indices() {
        if in_quote {
            if c == '"' {
                in_quote = false;
            }
            continue;
        }
        match c {
            '"' => in_quote = true,
            '(' => depth += 1,
            ')' if depth == 0 => return i,
            ')' => depth -= 1,
            c if c.is_whitespace() => return i,
            _ => {}
        }
    }
    rest.len()
}

/// Extracts every inline nested query from a lucene query string.
///
/// `Ok(None)` means the string contains no local-params block and callers
/// should take their existing path unchanged — no behaviour of any
/// local-params-free query goes through the code below. `Err` is a 400-worthy
/// message for a recognised block whose type this module deliberately does not
/// implement (see the module doc's ceiling note). The message names no request
/// param, because this is the parser for `q`, `fq`, `bq`, `facet.query`,
/// `/mlt`'s `q` and delete-by-query alike.
pub fn extract_nested_queries(q: &str) -> Result<Option<Rewritten>, String> {
    if !q.contains("{!") {
        return Ok(None);
    }
    let sentinel_prefix = unique_prefix(q);
    let mut outer = String::with_capacity(q.len());
    let mut nested: Vec<NestedQuery> = Vec::new();
    let mut rest = q;
    // Only `"` — the lucene grammar's sole phrase delimiter. A `'` in outer
    // query text is an ordinary term character (an apostrophe), unlike inside
    // a block body where it quotes a value.
    let mut in_quote = false;
    while !rest.is_empty() {
        // A `{!` inside a quoted phrase is literal text, not a block.
        if !in_quote
            && rest.starts_with("{!")
            && let Some((local, consumed)) = parse_block(rest)
        {
            // Echo the block as written, so the 400 names something the caller
            // can find in their own query rather than a reconstructed `{!}`.
            let block_src = &rest[..consumed];
            match local.query_type.as_deref() {
                Some("edismax") => {}
                Some(other) => {
                    return Err(format!(
                        "unsupported local-params query parser `{other}` in `{block_src}`"
                    ));
                }
                None => {
                    return Err(format!(
                        "local-params block with no query parser type: `{block_src}`"
                    ));
                }
            }
            rest = &rest[consumed..];
            let bound_len = bound_token_len(rest);
            let text = rest[..bound_len].to_string();
            rest = &rest[bound_len..];
            outer.push_str(&sentinel_with(&sentinel_prefix, nested.len()));
            nested.push(NestedQuery { local, text });
            continue;
        }
        let c = rest.chars().next().expect("rest is non-empty");
        if c == '"' {
            in_quote = !in_quote;
        }
        outer.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if nested.is_empty() {
        return Ok(None);
    }
    Ok(Some(Rewritten {
        outer,
        nested,
        sentinel_prefix,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The factual basis for PRD §2's divergence 6 claiming the hard 400 on an
    /// unrecognised local-params type is **not a regression**: before this
    /// module existed, `q` went straight to Tantivy's grammar, and that grammar
    /// rejects the raw `{!` string outright (`{` opens an exclusive range), so
    /// `{!lucene}quick` already 400d. Issue #137 changed only the error
    /// message. If Tantivy ever starts accepting these strings, that PRD claim
    /// needs restating rather than this test relaxing.
    #[test]
    fn raw_tantivy_grammar_rejects_unrecognised_local_params_blocks() {
        for q in ["{!lucene}quick", "{!term f=id}doc1", "{!func}sum(a,b)"] {
            assert!(
                tantivy::query_grammar::parse_query(q).is_err(),
                "PRD §2's divergence 6 states that `{q}` already 400d before issue #137 because \
                 Tantivy's own grammar rejects it; the grammar now accepts it, so that claim of \
                 'not a regression' must be re-checked"
            );
        }
    }

    #[test]
    fn parses_type_only_block() {
        let (local, consumed) = parse_block("{!edismax}quick").expect("block");
        assert_eq!(local.query_type.as_deref(), Some("edismax"));
        assert!(local.params.is_empty());
        assert_eq!(consumed, "{!edismax}".len());
    }

    /// The case `qf` depends on: a single-quoted value carrying spaces.
    #[test]
    fn parses_single_quoted_value_with_spaces() {
        let (local, _) = parse_block("{!edismax qf='a^1 b^1'}rest").expect("block");
        assert_eq!(local.query_type.as_deref(), Some("edismax"));
        assert_eq!(local.get("qf"), Some("a^1 b^1"));
    }

    #[test]
    fn parses_double_quoted_and_bare_values() {
        let (local, _) = parse_block(r#"{!edismax qf="a^1 b^1" mm=2}x"#).expect("block");
        assert_eq!(local.get("qf"), Some("a^1 b^1"));
        assert_eq!(local.get("mm"), Some("2"));
    }

    /// Issue #138's shape must not be structurally precluded by this module.
    #[test]
    fn parses_type_less_block() {
        let (local, _) = parse_block("{!key=Categories}category").expect("block");
        assert_eq!(local.query_type, None);
        assert_eq!(local.get("key"), Some("Categories"));
    }

    #[test]
    fn a_closing_brace_inside_a_quoted_value_does_not_end_the_block() {
        let (local, consumed) = parse_block("{!edismax qf='a} b'}x").expect("block");
        assert_eq!(local.get("qf"), Some("a} b"));
        assert_eq!(consumed, "{!edismax qf='a} b'}".len());
    }

    #[test]
    fn rejects_unterminated_and_non_block() {
        assert!(parse_block("{!edismax qf='a'").is_none());
        assert!(parse_block("quick").is_none());
        assert!(parse_block("{a TO b}").is_none());
    }

    #[test]
    fn no_local_params_means_no_rewrite() {
        assert_eq!(extract_nested_queries("quick brown"), Ok(None));
        assert_eq!(extract_nested_queries("date:{a TO b}"), Ok(None));
    }

    /// The binding rule, on trace 00003's exact shape: only `+"quick"` is
    /// bound; ` +"rocket")` stays with the outer parser.
    #[test]
    fn binds_only_the_next_whitespace_delimited_token() {
        let rewritten = extract_nested_queries("({!edismax qf='t^1 b^1'}+\"quick\" +\"rocket\")")
            .expect("supported")
            .expect("rewritten");
        assert_eq!(
            rewritten.outer,
            format!("({} +\"rocket\")", rewritten.sentinel(0)),
            "everything after the bound token must remain outer-parser text"
        );
        assert_eq!(rewritten.nested.len(), 1);
        assert_eq!(rewritten.nested[0].text, "+\"quick\"");
        assert_eq!(rewritten.nested[0].local.get("qf"), Some("t^1 b^1"));
    }

    #[test]
    fn binds_nothing_when_the_block_is_followed_by_whitespace() {
        let rewritten = extract_nested_queries("{!edismax} quick")
            .expect("supported")
            .expect("rewritten");
        assert_eq!(rewritten.outer, format!("{} quick", rewritten.sentinel(0)));
        assert_eq!(rewritten.nested[0].text, "");
    }

    /// Trace 00006's shape: the whole `q` is parenthesised and there is no
    /// whitespace after the bound run, so the closing `)` must terminate it.
    #[test]
    fn a_closing_paren_from_outside_the_run_terminates_it() {
        let rewritten = extract_nested_queries("({!edismax qf='t^1'}+\"quick\")")
            .expect("supported")
            .expect("rewritten");
        assert_eq!(rewritten.outer, format!("({})", rewritten.sentinel(0)));
        assert_eq!(rewritten.nested[0].text, "+\"quick\"");
    }

    #[test]
    fn bound_run_terminators() {
        assert_eq!(bound_token_len("quick"), 5);
        assert_eq!(bound_token_len("quick rocket"), 5);
        assert_eq!(bound_token_len("quick+rocket"), 12);
        assert_eq!(bound_token_len("+\"a b\" +\"c\""), 6);
        assert_eq!(bound_token_len(")rest"), 0);
        // A `)` matching a `(` opened inside the run does not terminate.
        assert_eq!(bound_token_len("(a)b)"), 4);
        // But whitespace does, at any paren depth: the run is cut mid-paren and
        // the outer parser is left unbalanced text (round-2 review item 4 --
        // the doc used to claim the opposite). No captured trace sends this.
        assert_eq!(bound_token_len("(a b)"), 2);
        // A `)` inside a quoted phrase is text, not a terminator.
        assert_eq!(bound_token_len("\"a)b\")"), 5);
    }

    #[test]
    fn sentinels_round_trip() {
        let prefix = unique_prefix("quick");
        assert_eq!(prefix, SENTINEL_PREFIX);
        assert_eq!(sentinel_index(&prefix, &sentinel_with(&prefix, 0)), Some(0));
        assert_eq!(sentinel_index(&prefix, &sentinel_with(&prefix, 7)), Some(7));
        assert_eq!(sentinel_index(&prefix, "quick"), None);
        assert_eq!(sentinel_index(&prefix, "__wf_nested_query_x__"), None);
        // The "nothing was lifted" prefix resolves nothing at all.
        assert_eq!(sentinel_index("", "__wf_nested_query_0__"), None);
    }

    /// Round-2 review item 1: a `q` containing the sentinel literal must not
    /// have its own text resolve to a nested query.
    #[test]
    fn a_sentinel_literal_in_the_input_is_re_keyed_around() {
        let q = "({!edismax qf='t^1'}+\"quick\" +__wf_nested_query_0__)";
        let rewritten = extract_nested_queries(q)
            .expect("supported")
            .expect("rewritten");
        assert_ne!(
            rewritten.sentinel_prefix, SENTINEL_PREFIX,
            "the colliding base prefix must have been extended"
        );
        assert!(
            !q.contains(&rewritten.sentinel_prefix),
            "the chosen prefix must be absent from the input"
        );
        assert_eq!(
            sentinel_index(&rewritten.sentinel_prefix, "__wf_nested_query_0__"),
            None,
            "the user's own literal must not resolve to a nested query"
        );
        assert_eq!(
            rewritten.outer,
            format!("({} +__wf_nested_query_0__)", rewritten.sentinel(0)),
            "the user's literal stays ordinary outer-parser text"
        );
        assert_eq!(rewritten.nested.len(), 1);
    }

    /// The extension loop must terminate however much of the prefix space the
    /// input squats on.
    #[test]
    fn the_prefix_is_extended_until_it_is_absent_from_the_input() {
        let squatted = format!("{SENTINEL_PREFIX}{}", "_".repeat(8));
        let q = format!("{{!edismax qf='t^1'}}+\"quick\" {squatted}");
        let rewritten = extract_nested_queries(&q)
            .expect("supported")
            .expect("rewritten");
        assert!(!q.contains(&rewritten.sentinel_prefix));
        assert!(rewritten.sentinel_prefix.starts_with(SENTINEL_PREFIX));
    }

    #[test]
    fn unsupported_types_are_rejected_rather_than_half_working() {
        for q in ["{!lucene}quick", "{!term f=id}doc1", "{!func}sum(a,b)"] {
            assert!(
                extract_nested_queries(q).is_err(),
                "{q} must not silently half-work"
            );
        }
        // A type-less block in `q` is not the #138 `facet.field` case and has
        // no captured `q` behaviour either.
        assert!(extract_nested_queries("{!qf=title}quick").is_err());
    }

    /// Round-2 review items 2 and 3: the message must not claim `q` (this is
    /// also `fq`/`bq`/`facet.query`/`/mlt`'s parser) and must echo the block
    /// rather than reconstructing an empty `{!}` for the type-less case.
    #[test]
    fn rejection_messages_echo_the_block_and_name_no_param() {
        let typed = extract_nested_queries("{!lucene df=id}quick").expect_err("rejected");
        assert!(typed.contains("`lucene`"), "{typed}");
        assert!(typed.contains("{!lucene df=id}"), "{typed}");
        assert!(!typed.contains("in `q`"), "must not claim `q`: {typed}");

        let typeless = extract_nested_queries("{!tag=x}quick").expect_err("rejected");
        assert!(
            typeless.contains("{!tag=x}"),
            "the type-less case must echo the block body, not `{{!}}`: {typeless}"
        );
        assert!(!typeless.contains("{!}"), "{typeless}");
        assert!(
            !typeless.contains("in `q`"),
            "must not claim `q`: {typeless}"
        );
    }

    #[test]
    fn a_block_inside_a_quoted_phrase_is_literal_text() {
        assert_eq!(extract_nested_queries("\"{!edismax}\""), Ok(None));
    }
}
