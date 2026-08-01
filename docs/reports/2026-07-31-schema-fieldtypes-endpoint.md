# Issue #156 — `GET /solr/{core}/schema/fieldtypes`

- Branch: `156-schema-fieldtypes`
- Worktree: `/Users/mark/Projects/wayfinder-156`
- Commits: `d003ed3` (red tests), `4e55b48` (implementation), `85f4ef3` (fix: core guard,
  exact name set, PRD language count)
- Base: `b09522d`

## What was built

`GET /solr/{core}/schema/fieldtypes` now serves the field-type list, resolving PRD open
question #142 as "In" (docs/PRD.md section 5, v2.75 block). The handler is `schema_fieldtypes`
in `src/lib.rs`; the reported set comes from `src/schema.rs`'s `LANGUAGES` table plus
`NON_LANGUAGE_BUILTIN_TYPES` — 9 non-language built-ins and 17 non-English `text_<code>`
presets, 26 names total.

**Client impact.** `search_api_solr`'s `SearchApiSolrBackend::isPartOfSchema` does a plain
`in_array($name, ...)` name-membership check against this endpoint's response. Its caller
`getSchemaLanguageStatistics()` swallows a 404 into `FALSE` (documented in docs/PRD.md line
566), so before this change every language reported "unsupported" on Drupal's server-status
screen even though search itself worked fine against Wayfinder.

**Deliberate divergence** (docs/PRD.md section 5, v2.75 block, lines 570-595), recorded because
a differential-harness row for this endpoint could only ever become a permanent
`EXPECTED_DIVERGENCES` entry, never resolved:
- *Omission* — no Lucene analyzer chains (`indexAnalyzer`/`queryAnalyzer`/`analyzer`) are
  emitted; Wayfinder cannot describe them truthfully.
- *Addition* — `indexed`/`stored`/`multiValued`/`docValues` are emitted uniformly on every
  entry, where Solr emits them sparsely. These four are Wayfinder's real type-level defaults
  and no client reads them.

**Honesty constraint.** The endpoint reports exactly the 18 languages Wayfinder has a stemmer
for (English plus 17 non-English presets in `LANGUAGES`, `src/schema.rs`), not a padded list.
Padding would convert today's misreport-downward into a misreport-upward — worse, because a
green "supported" row is never investigated, and `ta`/`tr` support would be silently hidden
from a client that could otherwise use it.

## Test evidence (re-run for this report, not copied)

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact invocation).
- `cargo test` — 668 passed, 35 suites, 0 failed.
- Coverage: `schema.fieldtypes.fieldTypes` probe (`src/coverage.rs:1166`) now passes;
  `tests/search_api_coverage.rs` asserts `48/75` (up from `46/75`).
- Ground truth: `solr-ref/search-api/trace/00020.json`
  (`GET /solr/search_api_capture/schema/fieldtypes?wt=json&json.nl=flat`), `manifest.tsv:21`.

## Review outcome

Two rounds (the pipeline's default cap), both by an independent Opus reviewer. This work could
use further review passes beyond the two the cap allowed — nothing in either round certified
the diff as exhaustively checked, only that the specific attacks made came back clean.

**Round 1** found three untested guards, each proven with a mutation that left the suite green
at 664 tests, plus one real bug:
- `check_core` was missing entirely from the handler — `GET /solr/nosuchcore/schema/fieldtypes`
  served the real core's field types instead of a 404. Fixed in `85f4ef3`
  (`check_core(&state, &core, &params, Envelope::WithParams)?` added at `src/lib.rs:1102`;
  regression test at `tests/schema_fieldtypes.rs:478-508`).
- docs/PRD.md stated 16 languages where the code (`LANGUAGES`) has 18 — the document
  authorizing the divergence contradicted the code implementing it. Fixed in `85f4ef3`
  (docs/PRD.md now states 18, and names `ta`/`tr` as the two the earlier draft and issue #156
  both missed).
- (Third untested guard — see mutation-testing note below; closed alongside the other two in
  `85f4ef3`.)

**Round 2** re-ran every Round 1 mutation and confirmed each now fails the suite. It additionally
verified, independently of the implementor's claims:
- every one of the 26 reported names is a type `resolve_type` actually accepts (no fabricated
  names in the response);
- `text_ja`/`text_zh`/`text_ko`/`text_pl` are rejected by `resolve_type` and absent from the
  response (`UNSUPPORTED_LANGUAGE_NAMES`, `tests/schema_fieldtypes.rs:64`).

Approved with no must-fix items.

## Follow-ups deferred by the reviewer — not fixed here

1. **Coverage-fraction merge conflict.** The `48/75` assertion in
   `tests/search_api_coverage.rs` conflicts with sibling branch #155 (`terms`), which is also
   incrementing the same fraction. Whichever of #155/#156 merges second must rebase and
   re-derive the fraction (and the corresponding history comment block) rather than mechanically
   resolving the conflict.
2. **Pre-existing schema-loader gap, surfaced by this endpoint.** Two `[[field_types]]` entries
   with the same `name` are accepted by the schema loader without error, and the
   `schema/fieldtypes` handler then emits that name twice in the response. `resolve_type` uses
   `find`, so the second definition is dead — indexing/query behavior is unaffected, impact is
   purely cosmetic (a duplicate name in this one response). Root-cause fix belongs in schema
   validation (reject the duplicate at load time), not a dedupe in the handler. Needs its own
   issue; not filed as part of this branch.
3. **Coverage-probe contract is loose.** `src/coverage.rs`'s `schema.fieldtypes.fieldTypes`
   probe only checks `body.get("fieldTypes")...is_array()`, so it would report covered even on
   an empty array. It shares this shape with the `admin.luke.index` and `terms.terms` probes
   (`is_object()` / presence checks with no content assertion) — a contract-sharpening
   candidate across all three, not a defect specific to this diff.

## Bottom line

`GET /solr/{core}/schema/fieldtypes` lands, resolving PRD open question #142 as "In" and fixing
the language-support misreport on Drupal's server-status screen. All local gates green
(fmt/clippy clean, 668/35 tests passing), coverage 46/75 -> 48/75. Two review rounds: round 1
caught a real missing-`check_core` bug plus a PRD/code language-count mismatch, both fixed on
this branch; round 2 re-confirmed the fixes and probed name-set correctness in both directions,
and approved with no must-fix items outstanding. Three follow-ups deferred, none blocking:
a coverage-fraction merge collision with #155, a pre-existing schema-loader duplicate-name gap
this endpoint exposed but did not cause, and a loose coverage-probe contract shared with two
other endpoints.
