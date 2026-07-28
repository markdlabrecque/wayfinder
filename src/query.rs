//! Query-string constructs beyond Tantivy's own `QueryParser` grammar (issue
//! #8): fuzzy (`term~`, `term~N`), wildcard/prefix (`te?t`, `test*`, `*mals`,
//! `an*ls`, `field:*`), regex (`/pattern/`).
//!
//! Established from `tantivy-query-grammar` 0.26.0's own source (not
//! memory), not from wishful thinking about what a Lucene-alike parser
//! "should" do:
//! - a bare word may contain a literal `*`/`?` — the grammar's `word()`
//!   combinator does not treat either as special, so `anim*` tokenizes as
//!   the single opaque literal `"anim*"`, which a field's own analyzer then
//!   mangles (`SimpleTokenizer` drops the `*` as a separator, leaving just
//!   `"anim"` — an exact-term query for `"anim"`, which is why the red
//!   baseline is "0 hits", not a parse error).
//! - a single-word literal's `~`/`~N` suffix parses fine as `slop`, but
//!   `QueryParser::generate_literals_for_str` (tantivy 0.26.1) silently drops
//!   `slop` whenever the literal tokenizes to <= 1 term — only a *phrase*
//!   (>= 2 tokens) keeps it. So `animals~2` becomes a plain exact-term query
//!   for `"animals"`, again no parse error.
//! - `field:*` parses to `UserInputLeaf::Exists`, but
//!   `QueryParser::compute_logical_ast_from_leaf_lenient`'s `Exists` arm
//!   unconditionally errors `"...need to target a specific field"` even
//!   though the field is right there — a real tantivy 0.26.1 gap, not a
//!   Wayfinder bug to route around gently.
//! - `/pattern/` *is* parsed into `UserInputLeaf::Regex` natively, but
//!   `QueryParser` only accepts it when `.allow_regexes()` was called, which
//!   `CoreIndex::parse_query` does not do (deliberately: see `RegexQuery`
//!   usage below, which needs the field-level checks this module does
//!   itself rather than tantivy's own arm).
//!
//! None of that is something to nurse along — Tantivy's parser was never
//! going to produce these on its own, so detection happens *upstream* of
//! feeding a query string into `tantivy::query::QueryParser` at all, and
//! only for the one shape every fixture in `tests/query_types.rs` actually
//! needs: the *entire* (trimmed) query collapsing to one atomic
//! `[field:]term<suffix>[^boost]` clause. Nothing here attempts general
//! boolean composition with fuzzy/wildcard/regex (no fixture exercises `foo
//! OR bar~1`) — `detect` returns `None` for anything else, and the caller
//! falls through unchanged to Tantivy's own grammar, which already covers
//! ranges, boosts, phrases and boolean composition correctly (verified
//! against the fixtures, not rebuilt — see `docs/solr-ref-findings.md`
//! findings 42-45).

use std::fmt;

/// The whole-string decomposition of one atomic special-syntax clause,
/// before any field/schema knowledge is applied — `detect` is pure string
/// splitting, kept free of `tantivy`/`CoreIndex` entirely so it is trivial to
/// reason about (and to unit-test) in isolation from index state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Atomic {
    /// `field:*` or `field:[* TO *]` (also `{...}`/mixed `[...}` brackets) —
    /// Solr's field-exists idiom.
    FieldExists { field: String },
    /// `[field:]term~` or `[field:]term~N` (`N` an integer or decimal).
    /// `distance_raw` is the text after the last `~`, still unparsed —
    /// resolving it to an edit distance is `resolve_fuzzy_distance`'s job,
    /// kept separate so semantics (finding 42's out-of-range-clamp rule)
    /// live in one place independent of this module's string-splitting.
    Fuzzy {
        field: Option<String>,
        term: String,
        distance_raw: String,
    },
    /// `[field:]glob`, `glob` containing an unescaped `*` or `?` anywhere
    /// (leading, trailing, infix) — the bare-`*` field-exists case above is
    /// checked first, so this never sees a lone `*`.
    Wildcard { field: Option<String>, glob: String },
    /// `field:/pattern/` — a balanced, non-empty `/.../ ` delimiter pair.
    Regex { field: String, pattern: String },
    /// `field:/pattern` — an opening `/` with no matching closer:
    /// `err_regex_unclosed.json`'s 400, never a silent term-query fallback
    /// (today's baseline before this module existed).
    RegexUnclosed,
}

/// Splits `query_str` into an `Atomic` clause plus an optional trailing
/// `^boost`, or returns `None` when the whole string is not one of these
/// constructs — the caller's signal to fall through to Tantivy's own parser
/// untouched. `None` is also returned (deliberately conservative) for
/// anything starting with a quote character or a range bracket other than
/// the pure field-exists shape, so this never second-guesses a phrase,
/// phrase slop, or a range Tantivy already parses correctly.
pub fn detect(query_str: &str) -> Option<(Atomic, Option<f32>)> {
    let trimmed = query_str.trim();
    if trimmed.is_empty() || trimmed.starts_with('"') || trimmed.starts_with('\'') {
        return None;
    }
    let (field, rest) = split_field(trimmed);
    let (body, boost) = split_boost(rest);
    if body.is_empty() {
        return None;
    }
    classify(field, body).map(|atomic| (atomic, boost))
}

/// `field:rest`, when the text before the first `:` is a bare identifier
/// (letters/digits/`_`/`.`); otherwise the whole string is the (unfielded)
/// body. Deliberately simple — this only ever runs on a whole-string atomic
/// candidate, never a general query, so there is exactly one `:` to
/// consider.
fn split_field(s: &str) -> (Option<&str>, &str) {
    if let Some(pos) = s.find(':') {
        let (candidate, rest) = (&s[..pos], &s[pos + 1..]);
        let is_ident = !candidate.is_empty()
            && candidate
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
        if is_ident {
            return (Some(candidate), rest);
        }
    }
    (None, s)
}

/// Strips a trailing `^<number>` boost suffix, if the text after the last
/// `^` parses as a float and the text before it is non-empty. Otherwise the
/// string is returned unchanged with no boost — including when `^` is
/// present but its suffix does not parse (e.g. `quick^bad`), so that shape
/// falls through to Tantivy's own (already-correct) rejection of it.
fn split_boost(s: &str) -> (&str, Option<f32>) {
    if let Some(pos) = s.rfind('^')
        && pos > 0
        && let Ok(boost) = s[pos + 1..].parse::<f32>()
    {
        return (&s[..pos], Some(boost));
    }
    (s, None)
}

fn classify(field: Option<&str>, body: &str) -> Option<Atomic> {
    if body.starts_with('"') || body.starts_with('\'') {
        return None;
    }
    if body.len() >= 2 && body.starts_with('/') && body.ends_with('/') {
        return Some(Atomic::Regex {
            field: field?.to_string(),
            pattern: body[1..body.len() - 1].to_string(),
        });
    }
    if body.starts_with('/') {
        return Some(Atomic::RegexUnclosed);
    }
    if body.starts_with('[') || body.starts_with('{') {
        // Range syntax: Tantivy's own grammar already covers inclusive/
        // exclusive/half-open/star-endpoint ranges correctly (finding 44).
        // The one gap is the pure field-exists shape (`[* TO *]`), which
        // Tantivy's own `QueryParser` silently drops as a no-op range when
        // both bounds are `Unbounded` — everything else here must fall
        // through untouched, in particular a query containing a bare `*` or
        // `~` as one endpoint among others (`[garden TO *]`) must NOT be
        // misread as a wildcard/fuzzy clause.
        return if is_field_exists_range(body) {
            field.map(|f| Atomic::FieldExists {
                field: f.to_string(),
            })
        } else {
            None
        };
    }
    if body == "*" {
        return field.map(|f| Atomic::FieldExists {
            field: f.to_string(),
        });
    }
    if let Some(pos) = body.rfind('~') {
        let (term, distance_raw) = (&body[..pos], &body[pos + 1..]);
        if !term.is_empty() && distance_raw.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Some(Atomic::Fuzzy {
                field: field.map(str::to_string),
                term: term.to_string(),
                distance_raw: distance_raw.to_string(),
            });
        }
    }
    if body.contains('*') || body.contains('?') {
        return Some(Atomic::Wildcard {
            field: field.map(str::to_string),
            glob: body.to_string(),
        });
    }
    None
}

/// True if `body` is a bracketed range (`[...]`, `{...}`, or a mixed
/// `[...}`/`{...]`) whose inner text is exactly `* TO *` — Solr/Lucene's
/// `TO` is a case-sensitive keyword (finding 44), so this deliberately does
/// not lowercase or trim internal whitespace beyond the two single spaces
/// the syntax itself requires.
fn is_field_exists_range(body: &str) -> bool {
    let opens_ok = body.starts_with('[') || body.starts_with('{');
    let closes_ok = body.ends_with(']') || body.ends_with('}');
    opens_ok && closes_ok && body.len() >= 2 && &body[1..body.len() - 1] == "* TO *"
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

/// Plain Levenshtein edit distance (insert/delete/substitute, no
/// transposition — no fixture pins Damerau-style transposition, so this is
/// the ceiling; see the module doc for why nothing here needs to match
/// Tantivy's own `levenshtein_automata`-based `FuzzyTermQuery` bit for bit,
/// only its match set). Used to enumerate a field's term dictionary for a
/// *scored* fuzzy match (finding 42: fuzzy hits are BM25-scored, not
/// constant-score, so the matching terms have to become real `TermQuery`s
/// rather than one constant-score automaton hit-set).
///
/// ponytail: an O(dictionary size) scan computing a fresh DP table per term
/// rather than a shared Levenshtein automaton — correct and simple, not fast;
/// revisit if a real corpus's term dictionary makes this a bottleneck.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = if ca == cb {
                prev[j]
            } else {
                1 + prev[j].min(prev[j + 1]).min(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// An error building one of these constructs into a real Tantivy `Query`.
/// Kept distinct from a plain `anyhow::Error` (which `CoreIndex::parse_query`
/// still returns, via `From`) so `select`'s error mapping can tell finding
/// 45's one 500 (`Internal`, a regex that parses but fails automaton
/// compilation) apart from every other 400 `SyntaxError` here — every other
/// kind of query-construction failure this module produces (unknown field,
/// unclosed regex, prefix-on-a-numeric-field) is `Syntax`.
#[derive(Debug)]
pub enum QueryError {
    Syntax(String),
    Internal(String),
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::Syntax(msg) | QueryError::Internal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for QueryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_fuzzy_bare_and_explicit_distance() {
        assert_eq!(
            detect("category:animols~"),
            Some((
                Atomic::Fuzzy {
                    field: Some("category".to_string()),
                    term: "animols".to_string(),
                    distance_raw: String::new(),
                },
                None
            ))
        );
        assert_eq!(
            detect("category:animols~1"),
            Some((
                Atomic::Fuzzy {
                    field: Some("category".to_string()),
                    term: "animols".to_string(),
                    distance_raw: "1".to_string(),
                },
                None
            ))
        );
    }

    #[test]
    fn detects_fuzzy_composed_with_boost() {
        assert_eq!(
            detect("category:animols~1^3"),
            Some((
                Atomic::Fuzzy {
                    field: Some("category".to_string()),
                    term: "animols".to_string(),
                    distance_raw: "1".to_string(),
                },
                Some(3.0)
            ))
        );
    }

    #[test]
    fn detects_wildcard_shapes() {
        assert_eq!(
            detect("category:anim*"),
            Some((
                Atomic::Wildcard {
                    field: Some("category".to_string()),
                    glob: "anim*".to_string(),
                },
                None
            ))
        );
        assert_eq!(
            detect("laz*"),
            Some((
                Atomic::Wildcard {
                    field: None,
                    glob: "laz*".to_string(),
                },
                None
            ))
        );
    }

    #[test]
    fn detects_field_exists_star_and_range() {
        assert_eq!(
            detect("category:*"),
            Some((
                Atomic::FieldExists {
                    field: "category".to_string(),
                },
                None
            ))
        );
        assert_eq!(
            detect("category:[* TO *]"),
            Some((
                Atomic::FieldExists {
                    field: "category".to_string(),
                },
                None
            ))
        );
    }

    #[test]
    fn does_not_misread_a_star_endpoint_range_as_wildcard() {
        assert_eq!(detect("category:[garden TO *]"), None);
        assert_eq!(detect("category:[* TO classic]"), None);
    }

    #[test]
    fn detects_regex_and_unclosed_regex() {
        assert_eq!(
            detect("category:/animals/"),
            Some((
                Atomic::Regex {
                    field: "category".to_string(),
                    pattern: "animals".to_string(),
                },
                None
            ))
        );
        assert_eq!(
            detect("category:/animals"),
            Some((Atomic::RegexUnclosed, None))
        );
    }

    #[test]
    fn does_not_misread_a_quoted_phrase() {
        assert_eq!(detect("\"category:animals\""), None);
        assert_eq!(detect("\"big bad wolf\"~2"), None);
    }

    #[test]
    fn does_not_misread_a_plain_boosted_term() {
        assert_eq!(detect("quick^10"), None);
        assert_eq!(detect("body:quick^bad"), None);
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
}
