//! Query-string constructs beyond Tantivy's own `QueryParser` grammar (issue
//! #8): fuzzy (`term~`, `term~N`), wildcard/prefix (`te?t`, `test*`, `*mals`,
//! `an*ls`, `field:*`), regex (`/pattern/`) — and their composition with
//! ordinary boolean structure (`OR`, `AND`, parens), which round-1 review
//! established a whole-string special case cannot get right.
//!
//! Established from `tantivy-query-grammar` 0.26.0's own source (not
//! memory), not from wishful thinking about what a Lucene-alike parser
//! "should" do:
//! - a bare word may contain a literal `*`/`?` — the grammar's `word()`
//!   combinator does not treat either as special (`*`/`?` are not in its
//!   `ESCAPE_IN_WORD` exclusion set), so `anim*` parses as one opaque literal
//!   whose `phrase` is the four-character string `"anim*"`. Tantivy's own
//!   conversion then tokenizes that phrase with the field's analyzer, which
//!   mangles it (`SimpleTokenizer` drops the `*` as a separator, leaving
//!   just `"anim"` — an exact-term query for `"anim"`, which is why the red
//!   baseline is "0 hits", not a parse error). `^` (boost) and whitespace
//!   *do* terminate a bare word, so a boost suffix and clause-level
//!   composition are already split apart correctly by the grammar itself,
//!   at the point this module's `classify_literal` gets to look at one
//!   literal's `phrase` in isolation.
//! - a single-word literal's `~`/`~N` suffix parses fine as `slop`, but
//!   `QueryParser::generate_literals_for_str` (tantivy 0.26.1) silently drops
//!   `slop` whenever the literal tokenizes to <= 1 term — only a *phrase*
//!   (>= 2 tokens, i.e. a *quoted* one — `Delimiter::None` literals are
//!   always exactly one whitespace-free run) keeps it. So `animals~2`
//!   becomes a plain exact-term query for `"animals"`, again no parse error.
//! - `field:*` (when nothing follows the `*` but whitespace/EOF/an
//!   escape-in-word char — the grammar's own `exists_precond`, which is what
//!   keeps `category:*mals` from being misread) parses to
//!   `UserInputLeaf::Exists { field }` — *correctly fielded*, the grammar
//!   sets it right via `UserInputLeaf::set_field`. The bug is entirely on
//!   tantivy's `QueryParser` side: `compute_logical_ast_from_leaf_lenient`'s
//!   `Exists` arm pattern-matches `{ .. }` and unconditionally errors
//!   `"...need to target a specific field"`, discarding the field it was
//!   just given — a real tantivy 0.26.1 gap, not a Wayfinder bug to route
//!   around gently.
//! - `/pattern/` *is* parsed into `UserInputLeaf::Regex` natively by the
//!   grammar. `QueryParser` only accepts it when `.allow_regexes()` was
//!   called, which this module's caller does not do (deliberately: building
//!   the leaf itself, below, needs the field-level checks tantivy's own arm
//!   does not do — a numeric-field wildcard 400, a non-Str-field 400/500
//!   split).
//!
//! None of that is something to nurse along — Tantivy's parser was never
//! going to produce these on its own for a bare/fielded literal, so
//! `CoreIndex::parse_query` parses the whole query into a
//! `tantivy::query_grammar::UserInputAst` itself (the exact same grammar
//! entry point Tantivy's own `QueryParser` uses internally) and walks it,
//! leaf by leaf: a leaf this module recognises as fuzzy/wildcard/regex/
//! field-exists is built directly; everything else (plain terms/phrases,
//! ranges, sets, boosts, and the boolean `Clause` structure joining them) is
//! delegated *per leaf* to Tantivy's own `QueryParser::
//! build_query_from_user_input_ast`, which already gets all of that right
//! against the fixtures — this module extends it rather than replacing it,
//! and composes with it at the AST level rather than the whole-query-string
//! level a round-1 review found could not discriminate a compound query
//! (`category:animals OR body:laz*`) from a bare atomic one.

use std::fmt;

use tantivy::query_grammar::{Delimiter, UserInputLeaf};

/// What a single `Delimiter::None` (bare, unquoted) literal's `phrase` text
/// collapses to, beyond the plain term/phrase Tantivy's own per-leaf
/// conversion already gets right. Only ever applied to a bare literal — a
/// quoted phrase, a phrase with `slop`/`prefix` already set by the grammar
/// (only possible for a *quoted* multi-token phrase), a range, a set, or
/// `UserInputLeaf::All`/`Exists` never reach this at all; see
/// `leaf_is_special_literal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteralKind {
    /// `term~` or `term~N` (`N` an integer or decimal). `distance_raw` is the
    /// text after the last `~`, still unparsed — resolving it to an edit
    /// distance is `resolve_fuzzy_distance`'s job, kept separate so
    /// semantics (finding 42's out-of-range-clamp rule) live in one place
    /// independent of this classification.
    Fuzzy { term: String, distance_raw: String },
    /// `glob` containing an unescaped `*` or `?` anywhere (leading, trailing,
    /// infix).
    Wildcard { glob: String },
    /// An opening `/` with no matching closer: `err_regex_unclosed.json`'s
    /// 400, never a silent term-query fallback (a *closed* `/pattern/` never
    /// reaches this classifier at all — the grammar's own `regex()`
    /// combinator already turns it into a distinct `UserInputLeaf::Regex`
    /// before `term_or_phrase` gets a chance to see it as a literal).
    RegexUnclosed,
    /// Nothing special — an ordinary bare word, safe to delegate to
    /// Tantivy's own literal conversion.
    Plain,
}

/// Classifies one bare literal's phrase text. Callers must have already
/// established the literal is `Delimiter::None` with `slop == 0` and
/// `prefix == false` (see `leaf_is_special_literal`) — this function does not
/// re-check any of that, it is pure string classification of the phrase
/// alone.
pub fn classify_literal(phrase: &str) -> LiteralKind {
    if phrase.starts_with('/') {
        return LiteralKind::RegexUnclosed;
    }
    if let Some(pos) = phrase.rfind('~') {
        let (term, distance_raw) = (&phrase[..pos], &phrase[pos + 1..]);
        if !term.is_empty() && distance_raw.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return LiteralKind::Fuzzy {
                term: term.to_string(),
                distance_raw: distance_raw.to_string(),
            };
        }
    }
    if phrase.contains('*') || phrase.contains('?') {
        return LiteralKind::Wildcard {
            glob: phrase.to_string(),
        };
    }
    LiteralKind::Plain
}

/// True for exactly the `UserInputLeaf::Literal` shape `classify_literal` is
/// meant to run on: a bare (`Delimiter::None`) single-token literal with no
/// slop/prefix already applied by the grammar. A *quoted* literal
/// (`"big bad wolf"`, `"big bad wolf"~2`, `"big bad wo"*`) never has its
/// `phrase` text second-guessed here — the grammar itself is the quote-
/// awareness (this is why the round-1-review-fixed predecessor's separate
/// "starts with a quote char" check no longer exists as a thing to get
/// wrong: there is no whole-string text left to misread once the leaf is
/// already a parsed literal).
pub fn leaf_is_special_literal(leaf: &UserInputLeaf) -> bool {
    matches!(
        leaf,
        UserInputLeaf::Literal(l) if l.delimiter == Delimiter::None && l.slop == 0 && !l.prefix
    )
}

/// Resolves a fuzzy `~`-suffix's raw distance text to an edit distance
/// (finding 42): empty (`~` alone) is the Lucene/Solr default of 2; a valid
/// number is floored and clamped to Tantivy's actual supported maximum of 2
/// (`FuzzyTermQuery::specialized_weight`'s `AUTOMATON_BUILDER` table only has
/// three rows, 0/1/2) — this is why `~3` and the legacy-similarity-style
/// `~0.8` are never syntax errors (finding 42's `err_fuzzy_dist3`/
/// `err_fuzzy_fractional`, both 200s with the exact-match set): Solr accepts
/// them and answers the same as a small in-range distance would for this
/// corpus's term dictionary, and clamping/flooring here reproduces that
/// without needing to special-case either fixture by name.
pub fn resolve_fuzzy_distance(distance_raw: &str) -> u8 {
    if distance_raw.is_empty() {
        return 2;
    }
    match distance_raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => (v.floor() as u64).min(2) as u8,
        _ => 2,
    }
}

/// Translates a Solr/Lucene wildcard glob (`*` = any run, `?` = exactly one
/// char, everything else literal) into the equivalent anchored regex pattern
/// for `tantivy::query::RegexQuery` (whose underlying `tantivy_fst::Regex`
/// automaton already matches a term end-to-end, per finding 43's
/// `regex_substring` fixture — no explicit `^`/`$` anchor needed or added).
pub fn glob_to_regex(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len() * 2);
    for c in glob.chars() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            c if is_regex_metachar(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

fn is_regex_metachar(c: char) -> bool {
    matches!(
        c,
        '.' | '^' | '$' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '+' | '\\'
    )
}

/// Damerau (restricted-edit / "optimal string alignment") edit distance:
/// insert/delete/substitute, *plus* a transposition of two adjacent
/// characters counting as one edit — Lucene's `FuzzyQuery` default
/// (`transpositions=true`), which round-1 review's `fuzzy_transposition_dist1`
/// fixture pins directly: `animasl` (the last two letters of `animals`
/// swapped) is plain-Levenshtein distance 2 but Damerau distance 1, and
/// `category:animasl~1` hits both `animals` docs.
///
/// This is the *restricted* variant (each substring may be transposed at
/// most once, transpositions may not overlap) rather than unrestricted
/// Damerau-Levenshtein — the same restriction tantivy's own
/// `levenshtein_automata`-based `FuzzyTermQuery` computes, so this only has
/// to match Lucene/tantivy's actual small-distance behaviour, not the more
/// expensive unrestricted general case no fixture exercises.
///
/// ponytail: an O(dictionary size) scan computing a fresh DP table per term
/// rather than a shared Levenshtein automaton — correct and simple, not fast;
/// revisit if a real corpus's term dictionary makes this a bottleneck.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate().take(n + 1) {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate().take(m + 1) {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[n][m]
}

/// An error building one of these constructs into a real Tantivy `Query`.
/// Kept distinct from a plain `anyhow::Error` (which `CoreIndex::parse_query`
/// still returns, via `From`) so `select`'s error mapping can tell finding
/// 45's one 500 (`Internal`, a regex that parses but fails automaton
/// compilation) apart from every other 400 `SyntaxError` here — every other
/// kind of query-construction failure this module produces (unknown field,
/// unclosed regex, prefix-on-a-numeric-field) is `Syntax`. `RegexCompile` is
/// its own variant rather than folded into a general `Internal` (round-1
/// review's cheap-extra (a)): a term-dictionary I/O error (`build_fuzzy`'s
/// `matching_terms`, wrapped as `Internal`) is a real 500 too but must not
/// come out dressed in finding 45's trace-carrying, no-`metadata` shape —
/// that shape is specifically and only for a regex failing automaton
/// compilation.
#[derive(Debug)]
pub enum QueryError {
    Syntax(String),
    RegexCompile(String),
    Internal(String),
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::Syntax(msg) | QueryError::RegexCompile(msg) | QueryError::Internal(msg) => {
                write!(f, "{msg}")
            }
        }
    }
}

impl std::error::Error for QueryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::query_grammar::UserInputLiteral;

    #[test]
    fn classifies_fuzzy_bare_and_explicit_distance() {
        assert_eq!(
            classify_literal("animols~"),
            LiteralKind::Fuzzy {
                term: "animols".to_string(),
                distance_raw: String::new(),
            }
        );
        assert_eq!(
            classify_literal("animols~1"),
            LiteralKind::Fuzzy {
                term: "animols".to_string(),
                distance_raw: "1".to_string(),
            }
        );
    }

    #[test]
    fn classifies_wildcard_shapes() {
        assert_eq!(
            classify_literal("anim*"),
            LiteralKind::Wildcard {
                glob: "anim*".to_string()
            }
        );
        assert_eq!(
            classify_literal("*mals"),
            LiteralKind::Wildcard {
                glob: "*mals".to_string()
            }
        );
        assert_eq!(
            classify_literal("anima?s"),
            LiteralKind::Wildcard {
                glob: "anima?s".to_string()
            }
        );
    }

    #[test]
    fn classifies_regex_unclosed_and_plain() {
        assert_eq!(classify_literal("/animals"), LiteralKind::RegexUnclosed);
        assert_eq!(classify_literal("animals"), LiteralKind::Plain);
        assert_eq!(classify_literal("quick"), LiteralKind::Plain);
    }

    fn literal(phrase: &str, delimiter: Delimiter, slop: u32, prefix: bool) -> UserInputLeaf {
        UserInputLeaf::Literal(UserInputLiteral {
            field_name: None,
            phrase: phrase.to_string(),
            delimiter,
            slop,
            prefix,
        })
    }

    #[test]
    fn only_a_bare_unmodified_literal_is_special() {
        assert!(leaf_is_special_literal(&literal(
            "laz*",
            Delimiter::None,
            0,
            false
        )));
        assert!(!leaf_is_special_literal(&literal(
            "big bad wolf",
            Delimiter::DoubleQuotes,
            0,
            false
        )));
        assert!(!leaf_is_special_literal(&literal(
            "big bad wolf",
            Delimiter::DoubleQuotes,
            2,
            false
        )));
        assert!(!leaf_is_special_literal(&literal(
            "big bad wo",
            Delimiter::DoubleQuotes,
            0,
            true
        )));
        assert!(!leaf_is_special_literal(&UserInputLeaf::All));
    }

    #[test]
    fn fuzzy_distance_default_and_clamped() {
        assert_eq!(resolve_fuzzy_distance(""), 2);
        assert_eq!(resolve_fuzzy_distance("0"), 0);
        assert_eq!(resolve_fuzzy_distance("1"), 1);
        assert_eq!(resolve_fuzzy_distance("2"), 2);
        assert_eq!(resolve_fuzzy_distance("3"), 2);
        assert_eq!(resolve_fuzzy_distance("0.8"), 0);
    }

    #[test]
    fn glob_translates_and_escapes() {
        assert_eq!(glob_to_regex("anim*"), "anim.*");
        assert_eq!(glob_to_regex("anima?s"), "anima.s");
        assert_eq!(glob_to_regex("*mals"), ".*mals");
        assert_eq!(glob_to_regex("an*ls"), "an.*ls");
    }

    #[test]
    fn levenshtein_matches_known_distances() {
        assert_eq!(levenshtein("animals", "animals"), 0);
        assert_eq!(levenshtein("animals", "animols"), 1);
        assert_eq!(levenshtein("animals", "animblz"), 2);
    }

    /// Round-1 review's discriminator: `animasl` (last two letters of
    /// `animals` transposed) is plain-Levenshtein distance 2 but Damerau
    /// distance 1 — `fuzzy_transposition_dist1.json` pins the Damerau
    /// answer.
    #[test]
    fn levenshtein_counts_an_adjacent_transposition_as_one_edit() {
        assert_eq!(levenshtein("animals", "animasl"), 1);
    }
}
