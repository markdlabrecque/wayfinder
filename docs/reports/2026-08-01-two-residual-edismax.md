# Issue #197: two residual edismax captures

## Scope

Close the two evidence gaps left by issue #147:

1. Capture Solr's parse of the motivating mid-token `-` form, `state-of-the-art`.
2. Capture a Shape-B query whose first bound-run whitespace occurs inside nested parentheses.

Both captures use the existing `solr:9` edismax schema and 10-document corpus from `solr-ref/capture.sh`. They are commented one-off commands rather than manifest rows because Wayfinder does not emit Solr's `debug` section; the nested request also intentionally returns a parser error.

## Captured behavior

- `edismax_midtoken_minus_debug.json`: HTTP 200. Solr reports one `DisjunctionMaxQuery`, `((title:state title:art) | (body:state body:art))`. The hyphens do not split the input into query clauses; `text_en` analysis removes `of` and `the` inside that one clause.
- `edismax_shape_b_debug_nested_paren.json`: HTTP 400 for `({!edismax qf='title body'}(+"quick" +"fox"))`. The first whitespace is at bound-run paren depth one. Solr's rejection confirms that whitespace cuts there and leaves the outer parser an unbalanced remainder; a depth-zero-only cut would bind the balanced inner expression.

Findings 91 and 92 and `src/local_params.rs` now cite those fixtures directly. Provenance tests require the fixtures and their bounded citations in the findings, capture script, and source comments.

## Tests and evidence

Tests were authored first and confirmed red because each fixture was missing. After capture:

- `cargo fmt --check` — pass
- `cargo clippy --all-targets -- -D warnings` — pass
- `cargo test` — pass
- Provenance mutation checks — deleting each new finding citation made its targeted provenance test fail as expected
- Independent review round 1 found stale `src/local_params.rs` ceiling wording and a missing source-citation guard; fixed with a red-first assertion
- Independent review round 2 — approved; full `cargo test` pass

No production behavior changed. Wayfinder's existing outcomes already agree with both captures.
