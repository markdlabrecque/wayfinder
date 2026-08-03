# #289 — function queries (`{!func}`, `{!boost b=...}`, `bf`, `boost`)

**Date:** 2026-08-04. Closes #289. Branch `markdlabrecque/implement-issue-289`
off `main`.

## Premise verification

Finding 129 (`docs/solr-ref-findings.md`) corrected the issue's premise: the
module never sends `bf=`. The document-boost score is emitted inline in `q` as
`{!boost b=sum(boost_document,...)}` or `{!boost b=boost_document}`
(`SearchApiSolrBackend.php:1953-1977`), making a function-query **evaluator**
reached through the `{!func}`/`{!boost}` query-parser local params the real
dependency — not a fixed function list reached through `bf=`. Verified against
the live `4.4.x` source.

One refinement on finding 129's "first targets": **`payload_score` is a separate
query parser, not an arithmetic function.** `Utility::flattenKeysToPayloadScore`
(`src/Utility/Utility.php:981`, fetched from `git.drupalcode.org`'s `4.4.x`
branch — outside the three-file snapshot) emits
`{!payload_score f=boost_term v=<term> func=max}` blocks over a payload-bearing
`boost_term_payload` field type. That is a different implementation site and a
different field-type requirement from the arithmetic evaluator `{!boost b=...}`
consumes, so it is a follow-up increment, not part of this branch. `ms`/`rord`
are off the corrected client path too (finding 129 corrected the
`product(...,recip(ms(...)))`-as-`bf` premise; `rord()` over a Points field is a
hard 400) and need date/ordinal field types. Posted the scope split to the issue
thread.

## What landed

- **`src/function_query.rs`** — the parser + AST + per-document evaluator and a
  bespoke Tantivy `Query`/`Weight`/`Scorer` (`FunctionScoreQuery`) that wraps a
  child query and multiplies (`{!boost}`, edismax `boost`) or adds (edismax
  `bf`) each document's score by a per-document function value. The AST covers
  constants, numeric field references, and `sum`/`product`/`max`/`min`/`recip`;
  it is the foundation #292's `geodist()` will extend. A field reference reads a
  numeric fast-field column; a missing value resolves to `0.0` (Solr's default).
  The scorer mirrors Tantivy's own `BoostQuery` (`BoostScorer` is the template);
  `seek_danger` falls back to its default impl because `SeekDangerResult` is not
  re-exported by tantivy 0.26.
- **`src/core_index.rs`** — `parse_function_query_q` handles a position-0
  `{!func}`/`{!boost b=...}` on `q` (precedence over `defType`); edismax's
  `boost`/`bf` are applied via a shared `apply_edismax_boost_bf` helper that the
  `q=*:*` short-circuit also uses (the bug that first left `bf`/`boost` inert on
  `*:*`).
- **`src/lib.rs`** — the two #232 warnings (`bf`, non-constant `boost`) are
  gone; those params are now applied. `{!func}`/`{!boost}` short-circuit before
  the `defType`/`parse_query` path.
- **`solr-ref/capture.sh`** — a self-contained `fnq` block appended at the end
  (own container, port 9060, core), 15 rows in `solr-ref/manifest-errors.tsv`.

## Evidence

15 fixtures captured 2026-08-04 against a real `solr:9` on a dedicated `fnq`
core (5 docs, numeric `docValues` fields; `d4`/`d5` carry missing values to pin
the missing→0 default). Every score fixture uses `q=*:*` or bare `{!func}`: a
match-all scores a constant `1.0`, so `{!boost b=f}*:*` is `1.0*f`, `bf=f` is
`1.0+f`, and `{!func}f` is `f` — the captured score is the pure function value,
with no BM25-magnitude divergence in the comparison. The differential harness
replays all 15 against a hermetic `fnq_app` and they **wire-match exactly** (0
diffs, scores within `1e-3`): `fnq_func_{field,sum,max,product,recip,const,
missing}`, `fnq_boost_{field,sum}`, `fnq_bf_additive`, `fnq_boost_param`, and
the four `fnq_err_*` 400s (unknown function, unbalanced parens, empty body,
unknown field).

`validate_fields` (the unknown-field → 400 guard) was mutation-tested: disabling
it makes `fnq_err_unknown_field` fail, and the differential harness catches it.

## Expiring guards updated

`tests/select_warnings.rs`, `tests/edismax.rs` (the two `bf`/`boost` ignore
guards), `tests/server_config.rs` (`strict_params_accepts_bf_*`),
`tests/edismax_descope_guard.rs` (PRD §5 descope, six → five params), and
`tests/local_params.rs` (PRD §2 divergence 6, `{!func}` now implemented) all
moved from "ignored/deferred" to "applied/landed". The exact-score behaviour for
function queries is fixture-backed in the `fnq` differential rows; the unit
tests guard the warning-envelope and descope contracts.

## Gates

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` are all green (58 test binaries, 0 failed). No network, no Docker.

## Follow-ups (named descopes)

- **`{!payload_score}`** — the `{!payload_score f=boost_term v=... func=max}`
  query parser over a `boost_term_payload` field type. Findings 143-146.
- **`ms`/`rord`** — date/ordinal functions, off the corrected client path.
- **Inline `{!func}`/`{!boost}`** — only the position-0 form is wired; an inline
  block still reaches `extract_nested_queries`'s unsupported-parser 400, which
  is the honest answer until an inline evaluator is needed (the client never
  nests these).
