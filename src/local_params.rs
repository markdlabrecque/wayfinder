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
//! The bound run ends at the first whitespace *at any paren depth*. Issue
//! #197 captures the nested form directly in
//! `solr-ref/responses/edismax_shape_b_debug_nested_paren.json`:
//! `({!edismax qf='...'}(+"quick" +"fox"))` is cut at the depth-one whitespace,
//! leaving the outer parser an unbalanced `+"fox"))`, and real Solr answers
//! 400. This is capture-derived behavior, not a ponytail ceiling.
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
    /// First value for `key`, if present. Repeated local-param keys are
    /// first-wins, matching captured Solr behaviour (finding 108).
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
/// Both the "bind the next run only" rule and each terminator below are
/// confirmed by Solr's own parse tree, not inferred from `numFound`
/// consistency (issue #147). Two `debugQuery=true` captures, one per
/// terminator, taken against a real `solr:9` with `capture.sh`'s edismax block
/// schema and 10-doc corpus and with `qf=title body` against `df=id` so the
/// parsed query names the field each token resolved through:
/// - `solr-ref/responses/edismax_shape_b_debug_parsedquery.json` —
///   `q=({!edismax qf='title body'}+"quick" +"rocket")`, the **whitespace**
///   terminator (trace 00003's shape). `parsedquery` is
///   `(+(+DisjunctionMaxQuery((title:quick | body:quick)))) +id:rocket`: only
///   `+"quick"` reached the nested edismax query, and `+"rocket"` — after the
///   run — was resolved by the outer lucene parser against `df=id`, matching
///   nothing (`numFound=0`). A "bind the whole remainder" reading would have
///   fanned `rocket` out over `qf` and never touched `df`.
/// - `solr-ref/responses/edismax_shape_b_debug_parsedquery_paren_terminated.json`
///   — `q=({!edismax qf='title body'}+"quick")`, the **`)` at run-local paren
///   depth 0** terminator (trace 00006's shape, no whitespace after `}` at
///   all). `parsedquery` is `+(+DisjunctionMaxQuery((title:quick |
///   body:quick)))` with `numFound=2` (`pA pB`): the `)` closed the query's
///   opening paren and contributed no clause of its own. A whitespace-only
///   terminator would have bound `+"quick")` and handed the nested parser an
///   unbalanced paren instead of a 200.
/// - `solr-ref/responses/edismax_shape_b_debug_nested_paren.json` —
///   `q=({!edismax qf='title body'}(+"quick" +"fox"))`, with the first bound-run
///   whitespace at **paren depth 1**. Real Solr answers 400 because the cut
///   leaves the outer parser the unbalanced remainder `+"fox"))`; a
///   depth-zero-only whitespace rule would bind the complete balanced inner
///   expression and parse successfully.
///
/// All three are checked by `tests/edismax.rs`'s `shape_b_*` tests. None is a
/// `manifest.tsv` row: Wayfinder emits no `debug` section, and the third is an
/// error envelope whose Java parser text cannot match Wayfinder verbatim. The
/// whole-body sweeps could only pass by widening a normaliser over real gaps
/// (same exclusion as `edismax_qf_partial_invalid`, #111). The commands are
/// commented at the end of `solr-ref/capture.sh`.
///
/// Terminators, all of them capture-derived rather than assumed:
/// - whitespace outside a quoted phrase, *at any paren depth*.
///   `+"quick" +"rocket"` (traces 00003/00004) binds `+"quick"` only, which is
///   the whole reason those traces answer 0. The issue #197 fixture above pins
///   the depth-independent half: `(+"quick" +"fox"))` binds `(+"quick"` and
///   leaves the outer parser unbalanced text, i.e. a 400.
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
    // True once a type-less inert block (`{!tag=...}`/`{!ex=...}`/`{!key=...}`)
    // has been stripped below: with no nested queries lifted, the loop would
    // otherwise return `Ok(None)` and the caller would re-parse the *original*
    // string (block and all). A strip is itself a rewrite.
    let mut rewrote = false;
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
                // `payload_score` (issue #340) is the second recognised
                // nested-query type, and unlike `edismax` it is the *inline*
                // form the client actually emits: `preQuery` joins
                // `{!boost b=boost_document}` with one `{!payload_score ...}`
                // block per boosted term and a `*:*` fallback, and the lucene
                // parser sums those SHOULD clauses. Its query text lives
                // entirely in its own local params (`f`/`v`/`func`), so the
                // bound run after `}` -- empty in every client-emitted query,
                // since a space follows each block -- is discarded by
                // `CoreIndex::build_nested_query`, exactly as Solr discards the
                // remainder when a `v` local param is present.
                Some("edismax" | "payload_score") => {}
                Some(other) => {
                    return Err(format!(
                        "unsupported local-params query parser `{other}` in `{block_src}`"
                    ));
                }
                None => {
                    // Type-less block (the `{!key=Foo}` shape issue #138 also
                    // uses on `facet.field`). On `q`/`fq`/`bq`/`facet.query`
                    // only the inert prefix params `tag`/`ex`/`key` are
                    // accepted (issue #295): they tag or relabel a query but
                    // select no parser, so the block is a prefix to strip and
                    // the remainder is the query itself — not a nested query
                    // to bind. A bare `{!}` or params like `qf=` that would
                    // imply a parser keep the hard 400: no capture defines
                    // their meaning, so they must not silently half-work
                    // (issue #137's open question 5).
                    let inert = !local.params.is_empty()
                        && local
                            .params
                            .iter()
                            .all(|(k, _)| matches!(k.as_str(), "tag" | "ex" | "key"));
                    if !inert {
                        return Err(format!(
                            "local-params block with no query parser type: `{block_src}`"
                        ));
                    }
                    rest = &rest[consumed..];
                    rewrote = true;
                    continue;
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
    if nested.is_empty() && !rewrote {
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

    /// Issue #298: the `facets` module tags an OR facet's filter with
    /// `facet:<search_api_field_name>` (the exact string `search_api_solr`'s
    /// `SearchApiSolrBackend` puts in `{!ex=...}` via
    /// `addExcludes(['facet:' . $info['field']])`). That tag carries a colon,
    /// and `{!tag=...}`/`{!ex=...}` are *bare* local-param values that
    /// `read_value` terminates on whitespace -- never on `:` -- so the whole
    /// `facet:category` must survive as one value rather than being split at
    /// the colon. Pin the parser's behaviour hermetically rather than assume
    /// it: the colon is load-bearing wire, not decoration.
    #[test]
    fn bare_local_param_value_keeps_its_colon() {
        let (tagged, _) = parse_block("{!tag=facet:category}category:animals").expect("block");
        assert_eq!(tagged.get("tag"), Some("facet:category"));
        let (excluded, _) = parse_block("{!ex=facet:category}category").expect("block");
        assert_eq!(excluded.get("ex"), Some("facet:category"));
        // The matching `{!ex=… key=…}` shape the Drupal module emits for an
        // OR facet under a delta key -- both bare values keep their colons.
        let (both, _) = parse_block("{!ex=facet:category key=category}ss_category").expect("block");
        assert_eq!(both.get("ex"), Some("facet:category"));
        assert_eq!(both.get("key"), Some("category"));
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

    /// Round-2 review item 2: the rejection message must not claim `q` (this
    /// is also `fq`/`bq`/`facet.query`/`/mlt`'s parser) and must echo the block
    /// the caller can find in their own query. Covers a typed block; the
    /// type-less `{!tag=...}` case moved to its own test below because #295
    /// made it accepted rather than rejected.
    #[test]
    fn rejection_messages_echo_the_block_and_name_no_param() {
        let typed = extract_nested_queries("{!lucene df=id}quick").expect_err("rejected");
        assert!(typed.contains("`lucene`"), "{typed}");
        assert!(typed.contains("{!lucene df=id}"), "{typed}");
        assert!(!typed.contains("in `q`"), "must not claim `q`: {typed}");
    }

    /// #295: a type-less block whose params are all inert (`tag`/`ex`/`key`)
    /// is a prefix to strip, not a nested query to bind. The remainder parses
    /// as the ordinary outer query, so `{!tag=cat}category:animals` becomes
    /// `category:animals` with no nested queries lifted.
    #[test]
    fn inert_typeless_blocks_are_a_stripped_prefix_not_a_nested_query() {
        let rewritten = extract_nested_queries("{!tag=cat}category:animals")
            .expect("an inert prefix is accepted, not rejected")
            .expect("stripping a block is itself a rewrite, so this is Some");
        assert_eq!(rewritten.outer, "category:animals");
        assert!(rewritten.nested.is_empty());

        // `ex` and `key` are inert here too, in any combination/order.
        let r = extract_nested_queries("{!ex=cat key=u}x")
            .expect("ex/key are inert")
            .expect("rewrote");
        assert_eq!(r.outer, "x");
        assert!(r.nested.is_empty());
    }

    /// A type-less block with a non-inert param (`qf=`, which would imply a
    /// parser) is still the hard 400 — #295 accepted only `tag`/`ex`/`key`.
    #[test]
    fn a_typeless_block_with_a_non_inert_param_still_400s() {
        assert!(extract_nested_queries("{!qf=title}quick").is_err());
        // A bare `{!}` selects nothing and is not an inert prefix either.
        assert!(extract_nested_queries("{!}quick").is_err());
    }

    #[test]
    fn a_block_inside_a_quoted_phrase_is_literal_text() {
        assert_eq!(extract_nested_queries("\"{!edismax}\""), Ok(None));
    }
}
