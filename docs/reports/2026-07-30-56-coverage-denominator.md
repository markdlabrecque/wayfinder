# Issue #56 — Search API coverage denominator

## Result

`wayfinder coverage --format json` is a hermetic, deterministic report over
issue #55's frozen `search_api_solr` 4.4.0 capture.

| Bucket | Covered | Total | Fraction |
|---|---:|---:|---:|
| Endpoints | 5 | 9 | 5/9 |
| Request semantics | 27 | 51 | 27/51 |
| Client-consumed response fields | 9 | 15 | 9/15 |
| **Overall** | **41** | **75** | **41/75** |

**Update (post-merge follow-up):** fixed three non-discriminating request-semantics probes
(`select.q.plain-query`, `select.highlight.snippets`, `select.highlight.fragsize`) — see
"Follow-up: probe honesty pass" below. `select.highlight.snippets` moved from covered to
uncovered (`28/51` → `27/51`, overall `42/75` → `41/75`): the old probe passed regardless of
whether `hl.snippets` did anything, because the seeded corpus only ever had one possible
snippet window. Wayfinder's `CoreIndex::highlight_field` is genuinely capped at one snippet
(tracked as issue #103); the honest number is 41/75 until that lands.

## Denominator and provenance

`coverage/search_api_coverage_contract.json` is the checked-in derived
contract for all 28 frozen traces and the Search API manifest. It records all
43 decoded request parameter names, all value/trace occurrences, 51 material
request/body semantic variants, nine normalized method-and-endpoint shapes,
and 15 client-consumed response fields.

Each semantic parameter has both its material value class and exact per-value
trace occurrences. The integration guard reparses every frozen URL/body and
requires the union of semantic occurrences to equal the full captured
occurrence set. This prevents a parameter-name-only denominator. In
particular, local-param edismax is split into AND, OR, and single-term forms,
and `q=*:*` is its own covered semantic; all of `00003` through `00008` and
`00021` are now explicitly represented.

`coverage/search_api_solr_4.4.0_source_evidence.json` plus the immutable,
focused `coverage/search_api_solr_4.4.0_source/` snapshot live outside
`drupal/`. They pin the public `search_api_solr` 4.4.0 archive SHA-256
`5cfcb17d7a325a01eb04f09ca12b6f0d3012ebe0fcfea431ee04a592507c0bce`,
the three consumed source-file hashes, exact source ranges/excerpts, and all
15 field-to-client citations. The guard verifies the exact snapshot file set,
hard-pinned source hashes, every range and excerpt against the copied source,
and the required/forbidden expressions for all four emitted-only exclusions.

`responseHeader.status`, `response.start`, `response.maxScore`, and
`response.numFoundExact` remain excluded because the pinned Search API paths
do not consume them.

## Numerator derivation

- Endpoints use the single `search_api_routes!` table that builds the Axum
  router.
- Semantics and response fields execute real strict-router requests against a
  secure `tempfile::TempDir` workspace. The probe requires delayed
  `commitWithin` visibility; actual pagination windows; exact sorted ID order;
  exact facet sort/limit/mincount buckets; MLT threshold, max-query-term, and
  boost-sensitive responses; and response value shapes/types rather than
  HTTP success or pointer presence.
- The fixed corpus and output ordering keep the command deterministic without
  network, Docker, user schema, index, or environment configuration.

## Uncovered backlog

### Endpoints

- `GET /solr/{core}/admin/luke`
- `GET /solr/{core}/admin/mbeans`
- `GET /solr/{core}/schema/fieldtypes`
- `GET /solr/{core}/terms`

### Request semantics

- `admin.mbeans.stats`
- `mlt.filters`
- `mlt.fl.wildcard-plus-score`
- `mlt.match-include-and-offset`
- `mlt.maxntp`
- `request.json-nl.flat`
- `request.json-nl.repeated-map-and-flat`
- `request.omitHeader`
- `request.timezone.utc`
- `select.facet.local-key`
- `select.facet.per-field-missing`
- `select.highlight.merge-contiguous`
- `select.highlight.require-field-match`
- `select.highlight.wildcard-fields`
- `select.q.local-params-edismax.and`
- `select.q.local-params-edismax.or`
- `select.q.local-params-edismax.single-term`
- `select.spellcheck.collate`
- `select.spellcheck.dictionaries`
- `select.spellcheck.enable`
- `select.spellcheck.query`
- `terms.enumeration`
- `update.json-command-add-batch`

### Response fields

- `admin.luke.index`
- `admin.mbeans.solr-mbeans`
- `schema.fieldtypes.fieldTypes`
- `select.spellcheck.collations`
- `select.spellcheck.suggestions`
- `terms.terms`

## Review round 1 remediation

Reviewer round 1 **bounced** on status/pointer-only classifications,
incomplete material query provenance, absent client-source citations, and a
predictable workspace. This remediation tightens the live probes and output
guards, adds complete occurrence provenance and local-param variants, adds the
pinned source-evidence snapshot, and replaces `create_dir_all` with an
exclusive `TempDir`. The denominator rose from 72 to 75 and the mechanically
recomputed fraction is **42/75**.

No Drupal, CI, or frozen `solr-ref/search-api/` input was modified.

### Mutation evidence

Temporarily removing `"sort"` from the live `SELECT_PARAMS` allowlist made
`coverage_command_requires_complete_deterministic_contract_schema_and_output`
fail as expected. The live report changed request semantics from `28/51` to
`25/51` and overall coverage from `42/75` to `39/75`, adding all three sort
variants to the uncovered list. The exact source was restored before gates.

## Verification

- `cargo test --test search_api_coverage -- --nocapture` — pass (6 tests).
- `cargo test --test search_api_coverage_endpoint_provenance -- --exact each_endpoint_cites_every_frozen_exchange_with_its_method_and_shape --nocapture` — pass (1 test).
- `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — pass; fmt clean, clippy clean, 498 tests passed, 0 failed (including doc tests).

## Follow-up: probe honesty pass

Three request-semantics probes reported `covered` without actually discriminating supported
from unsupported behavior:

- **`select.q.plain-query`** — the contract's captured value is Search API Solr's *internal*
  expanded Lucene syntax against dynamic fields Wayfinder doesn't host
  (`tm_X3b_en_body:(+"quick")^1 ...`), never something a real client sends as an opaque `q=`.
  Kept the existing `numFound` behavioral proxy, documented why.
- **`select.highlight.snippets`** — probed `hl.snippets=1` (the default), so it passed whether
  or not `hl.snippets` was honored at all; the seeded corpus only ever had one possible
  snippet window per doc. Added a dedicated fixture doc (`hl-snippets-gizmo`, a term repeated
  three times spaced past a snippet-window width) and switched the probe to the real captured
  value, `hl.snippets=3`. This uncovered a genuine gap: `CoreIndex::highlight_field` is capped
  at exactly one snippet (Tantivy's public `SnippetGenerator` only exposes its single
  best-scoring fragment) — filed as **issue #103**. The probe now honestly reports uncovered;
  request semantics moved `28/51` → `27/51`, overall `42/75` → `41/75`.
- **`select.highlight.fragsize`** — the presence-only check passed even with `hl.fragsize`
  handling mutated out entirely. Added a second, fixture-backed request
  (`hl.method=original&hl.fragsize=10`, matching `hl_fragsize_truncated.json`) asserting real
  truncation. The captured shape (`hl.fragsize=0`, no `hl.method`) stays a presence check —
  no fixture has a field long enough to make whole-field-vs-fragmented observable — filed as
  **issue #104**.

Mutation-tested: each fixed probe was confirmed to flip from pass to fail when the underlying
behavior was temporarily broken, then the break was reverted. `cargo test` (512 tests),
`cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` all clean afterward.
