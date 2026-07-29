//! Pure, self-contained grammar helpers for `defType=edismax` (issue #7,
//! PRD §5 v1 exception): the `mm` (minimum-should-match) spec-to-integer
//! function the issue names as "a small self-contained parser... implement
//! it fully" (`tests/mm.rs`), plus the `qf`/`pf` `field^boost` list grammar
//! shared by both params.
//!
//! The actual query composition (per-field `DisjunctionMaxQuery`, `tie`,
//! `boost`, `bq`, the `q` grammar walk) lives in `CoreIndex` in
//! `src/core_index.rs`, where the schema, tokenizers, and Tantivy index
//! handle already live — this module only holds the parts that are pure
//! string/arithmetic and so are unit-testable with no index at all.

/// Splits a `qf`/`pf` value (`"title^10 body"`) into `(field name, boost)`
/// pairs, in the order given. A field with no `^boost` suffix defaults to
/// `1.0`, matching Solr's own default per-field weight. A `^` suffix that
/// fails to parse as a float is treated the same as no suffix at all
/// (`1.0`) — lenient, consistent with Wayfinder's "ignore what it can't
/// understand rather than 400" stance (finding 8) for a sub-grammar with no
/// fixture pinning its error behaviour.
pub fn parse_field_weights(spec: &str) -> Vec<(String, f32)> {
    spec.split_whitespace()
        .filter(|tok| !tok.is_empty())
        .map(|tok| match tok.split_once('^') {
            Some((name, boost_str)) => {
                let boost = boost_str.parse::<f32>().unwrap_or(1.0);
                (name.to_string(), boost)
            }
            None => (tok.to_string(), 1.0),
        })
        .collect()
}

/// `min_should_match`: Solr edismax's `mm` grammar, `(spec, clause_count) ->
/// required_count`. See `tests/mm.rs`'s module doc and finding 68
/// (`docs/solr-ref-findings.md`) for the full derivation, verified against a
/// real Solr 9 rather than reconstructed from memory of the reference-guide
/// prose (which gets at least one case, `-25%`, wrong).
///
/// No floor-at-1 clamp here by design (finding 68's own scoping note): a
/// spec that computes 0 (or a clause count of 0) is returned as 0 verbatim.
/// Whether an all-optional query still needs at least one clause to match is
/// a `BooleanQuery`-construction concern, not this grammar's — and in
/// practice it falls out for free at that layer: Tantivy's own
/// `BooleanWeight` promotes an all-`Should` set with no `Must` clauses to
/// "at least one must match" regardless of `minimum_number_should_match`,
/// the same floor real Solr/Lucene's disjunction scorer has structurally
/// (a doc matching zero clauses is never enumerated by a disjunction at
/// all).
pub fn min_should_match(spec: &str, clause_count: usize) -> usize {
    let spec = spec.trim();
    let n = clause_count as i64;
    if spec.is_empty() {
        return clause_count;
    }
    let mut result = n; // default: all required, before any override below.
    for token in spec.split_whitespace() {
        match token.find('<') {
            Some(idx) => {
                let (x_str, y_str) = (&token[..idx], &token[idx + 1..]);
                let x: i64 = x_str.parse().unwrap_or(0);
                // `clause_count <= x` means this pair is SKIPPED, not
                // applied — the easy-to-get-backwards part (finding 68).
                if n > x {
                    result = apply(y_str, n);
                }
            }
            None => {
                result = apply(token, n);
            }
        }
    }
    result.clamp(0, n) as usize
}

/// `apply(spec, n)` for one bare (non-conditional) `mm` token: a positive
/// integer clamps to `n`; a negative integer subtracts from `n` (floored at
/// 0); a positive percentage floors; a negative percentage subtracts a
/// floored "may be missing" count from `n` — floor on both signs, never
/// ceiling (the case finding 68 flags as the one memory gets wrong).
fn apply(token: &str, n: i64) -> i64 {
    let raw = if let Some(pct) = token.strip_suffix('%') {
        let p: i64 = pct.parse().unwrap_or(0);
        if p < 0 {
            let missing = ((-p) as f64 * n as f64 / 100.0).floor() as i64;
            n - missing
        } else {
            (p as f64 * n as f64 / 100.0).floor() as i64
        }
    } else {
        let y: i64 = token.parse().unwrap_or(0);
        if y < 0 { n + y } else { y.min(n) }
    };
    raw.clamp(0, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_weights_default_to_one() {
        assert_eq!(
            parse_field_weights("title body"),
            vec![("title".to_string(), 1.0), ("body".to_string(), 1.0)]
        );
    }

    #[test]
    fn field_weights_parse_boost_suffix() {
        assert_eq!(
            parse_field_weights("title^10 body"),
            vec![("title".to_string(), 10.0), ("body".to_string(), 1.0)]
        );
    }

    #[test]
    fn field_weights_tolerate_bad_boost_suffix() {
        assert_eq!(
            parse_field_weights("title^oops"),
            vec![("title".to_string(), 1.0)]
        );
    }
}
