# #353 — five missing `hl.*` params admitted; `hl.preserveMulti` + `hl.fragmenter` implemented

Closes #353. `SearchApiSolrBackend::setHighlighting()`
(`coverage/search_api_solr_4.4.0_source/.../SearchApiSolrBackend.php:4230-4275`)
emits five `hl.*` params only when non-default, so a default-configured capture
never saw them and they were absent from `SELECT_PARAMS`. Under
`strict_params = true` each one 400'd a request the client legitimately sends —
exactly the failure the `SELECT_PARAMS` rule exists to prevent.

## What changed

**Admission (`src/lib.rs`).** All five added to `SELECT_PARAMS`, grouped with a
comment naming each one's disposition:
`hl.preserveMulti`, `hl.fragmenter`, `hl.maxAnalyzedChars`,
`hl.usePhraseHighlighter`, `hl.highlightMultiTerm`.

**`hl.preserveMulti` — implemented (`src/highlight.rs` + `src/core_index.rs`).**
Real semantics, the one with a wire-visible effect. Captured on the base corpus's
multi-valued `category` field (findings 187): under `hl.method=original` it
returns one snippet **per value, in indexed order, for every value** — matching
values highlighted, non-matching values plain. The default merges the values
into one stream and returns only matching fragments. It is a **complete no-op
under the default `hl.method=unified`** (captured byte-identical on/off), so
Wayfinder consults it only on the original path
(`CoreIndex::highlight_field_preserve_multi`); the unified highlighter is itself
a ponytail (finding 55). `hl.snippets` does not cap the value count.

**`hl.fragmenter` — `gap` is a no-op (default).** `gap` is Solr's own default
original-method fragmenter (`LuceneGapFragmenter`), byte-identical to omitting it
(finding 188, `hl353_fragmenter_gap`). `regex` is not built — see the guard.

**Inert params — `ponytail:` in `src/highlight.rs`.** `hl.maxAnalyzedChars`
(char-analysis window; Tantivy analyses the whole field), `hl.usePhraseHighlighter`
(phrase-span correlation), `hl.highlightMultiTerm` (wildcard/fuzzy expansion).
Each ceiling is named so an accepted param cannot change behaviour silently.

**Self-expiring guard (`tests/hl353_regex_descope_guard.rs`).** `hl.regex.*` is
never emitted by 4.4.0 because of an inverted inner guard at
`SearchApiSolrBackend.php:4250` (`if ('regex' !== $highlighter['fragmenter'])` —
always false once the outer `if ('gap' !== ...)` is entered). The guard asserts
the inversion is still present in the vendored source and that no captured trace
sends `hl.regex.*`; the day upstream fixes it, the guard fails and names itself
for removal, and `hl.regex.*` becomes real work.

## Fixtures (ground truth, `manifest.tsv`)

Captured against the base 5-doc `category` corpus via a new `capture.sh` block
(`hl353_`, own container/port 9353), run with `--only '^hl353_'`:

- `hl353_preserve_multi_off` — `q=category:animals&hl.method=original` → merged,
  only matching value.
- `hl353_preserve_multi_on` — + `hl.preserveMulti=true` → every value in order.
- `hl353_fragmenter_gap` — `q=body:lazy&hl.method=original&hl.fragsize=20&hl.fragmenter=gap`
  → identical to the default fragmenter.

The differential harness runs all three against the `content` core automatically
(`hermetic_whole_query_set_matches_committed_fixtures`). Hand-written tests in
`tests/highlighting.rs` pin the `/highlighting` blocks directly and add a
no-op-under-unified guard.

## Mutation testing (per spec)

- **Admission:** removed `"hl.preserveMulti"` from `SELECT_PARAMS` →
  `strict_params_accepts_every_implemented_highlight_param` went red with
  `unknown request parameter 'hl.preserveMulti' (strict_params is on)`. Reverted.
- **preserveMulti behaviour:** forced `preserve_multi = false` in the per-field
  loop → `hl_preserve_multi_on_returns_one_snippet_per_value_in_order` went red
  (`doc1` returned `["<em>animals</em>"]` merged instead of
  `["<em>animals</em>","classic"]` per-value). Reverted.
- **Guard:** flipped the vendored source's `'regex' !==` to `'regex' ===` →
  `source_still_has_the_inverted_inner_regex_guard` went red with the build-it-
  and-delete-this-guard message. Reverted.

## Pre-existing `capture.sh` breakage (fixed to unblock the capture)

The committed `capture.sh` failed `bash -n` on HEAD — a #366 merge artifact left
the `dr341` block inside the `pls` block's `cappls()` function (dropping its
`>> "$MANIFEST_ERRORS"` / `rm -f` / `}` lines) and the second `jf343` release
block was missing its `fi`. Both are restored here with comments; this was
required for any `--only` capture to reach an appended block. The deeper
`pls`-captures-inside-`dr341`-guard semantic interleaving is a separate,
pre-existing condition that does not affect a filtered run and is left for a
follow-up.

## Cross-file coupling fixed

`tests/search_api_coverage.rs::classification_guards_exercise_real_router_strict_param_and_renderer_behavior`
used `hl.maxAnalyzedChars` as its example of an *unsupported* strict param (it
had already migrated `hl.requireFieldMatch` → `hl.maxAnalyzedChars` for #139).
Moved to `hl.alternateField` (real Solr param, no alternate-field highlighting in
scope) with the migration chain documented.

## Gates

`cargo test` (all suites), `cargo fmt --check`, and
`cargo clippy --all-targets -- -D warnings` are all clean.
