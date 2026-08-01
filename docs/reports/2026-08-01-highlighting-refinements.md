# Issue #139 — highlighting refinements (`hl.fl=*`, `hl.mergeContiguous`, `hl.requireFieldMatch`)

- Branch: `139-highlighting-refinements`
- Worktree: `/Users/mark/Projects/wayfinder-139`
- Head: `1c9788f`
- Commits: `7c0dd6d` (feat), `1c9788f` (round-2 review corrections)
- Findings: appended as #94 and #95 in `docs/solr-ref-findings.md` (cited here by number,
  not re-added)
- Follow-ups filed: #181, #184, #185 (all open)

## What was built

`search_api_solr` sends `hl.fl=*` on 19 of its 28 captured traces, plus
`hl.mergeContiguous=false` and `hl.requireFieldMatch=false` on essentially every request.
Wayfinder previously supported explicit `hl.fl` lists only, and `strict_params = true` would
400 the other two params outright.

- **`resolve_hl_fl()` and `highlightable_fields()`** (`src/highlight.rs`) resolve `hl.fl`,
  expanding the bare `*` wildcard against the schema's own field declarations rather than the
  query's `qf`/`df` set. Explicit field names go through the existing `check_highlightable`
  validation unchanged: an undefined or non-text field named explicitly is still a 400
  (`InvalidHlField`). Fields the wildcard produces are never validated that way — a
  non-highlightable field the wildcard sweeps up is silently skipped rather than erroring, since
  a naive schema-wide expansion run through the same check would 400 any schema that merely
  *contains* a non-text field, a shape real Solr cannot produce.
- **`hl.mergeContiguous` and `hl.requireFieldMatch`** added to `SELECT_PARAMS` in `src/lib.rs`.
  Both are allowlist-only: `false` is Solr's documented default for both *and* already
  Wayfinder's unconditional behaviour (no field-match filtering, no fragment merging), so there
  is deliberately no knob behind either name for the `false` path the module sends. The `true`
  side of both is unimplemented — see follow-ups.
- `tests/highlighting.rs` gained the wildcard-expansion test coverage; `tests/search_api_coverage.rs`
  was updated for the two newly-covered `request_semantics` probes.

## Deliberate divergence from Solr, stated plainly

`highlightable_fields()` narrows the wildcard's expansion to **stored, analyzed text fields
only**, excluding `string`/`keyword` fields even though they share `ValueKind::Text` with
analyzed types in Wayfinder's schema model. This is a real behavioural choice, not a no-op:
Wayfinder's snippet path *does* produce marker-wrapped output for a raw string field when one is
named explicitly (`hl.fl=category` is a 200 that emits
`{"doc4":{"category":["<em>animals</em>"]}}` against `common::SCHEMA_TOML`), so leaving string
fields in the wildcard's expansion set would change what `hl.fl=*` returns, not just how it's
computed.

Real Solr's `SolrIndexSearcher.getStoredHighlightFieldNames` is `StrField`-inclusive: it expands
to every stored `TextField` *or* `StrField`, and a `StrField` simply produces no snippet because
it is never analyzed. Wayfinder's exclusion diverges from that on purpose.

**No captured fixture settles this either way.** The traced `search_api_solr` corpus's only
genuinely stored non-`tm_` fields are `sm_context_tags`, `id`, and `_root_` (finding 94); no
captured query term ever matches a value in `sm_context_tags`, so every trace's `{}`-per-doc
`highlighting` entry is equally consistent with Solr including or excluding a matched
`StrField`. Two traces that look discriminating at first glance (`00005`/`00007`) are not: their
`sm_*` fields are `stored="false"` and reach `docs` via docValues, so Solr's stored-field
expansion never saw them in the first place. The exclusion is pinned only by Wayfinder's own
test, `hl_wildcard_fl_does_not_error_on_a_matched_non_text_field`, and its doc comment says so
explicitly rather than dressing the choice up as fixture-derived.

## Review outcome

Two rounds, reaching the default two-round cap.

**Round 1** (Opus reviewer) found:

- A deliberate-mutation check on the `is_raw_string` filter inside `highlightable_fields()`
  (mutation A2 — dropping only that filter, leaving the rest of the wildcard-expansion logic
  intact) passed the entire test suite green. The divergence the round-1 diff called out as
  deliberate had, in practice, zero test guarding it: the only test exercising the excluded-field
  path used `q=*:*&fq=category:animals`, which puts the matching term in `fq`. The highlighter
  never sees a query term for `category` under that shape, so `{}`-per-doc came out whether or
  not the wildcard swept `category` up — the test passed identically with the exclusion present
  or removed.
- Three code comments that were factually false: one claimed `solrconfig_query.xml` sets no `df`
  for the traced core (the core's `/select` handler does set one, to `id`, in
  `solrconfig_extra.xml`); the others mischaracterized which schema field a naive
  `check_highlightable`-based wildcard expansion would 400 on.
- The stage-1 notes contained self-contradicting evidence about `df`: one passage argued the
  wildcard couldn't be a `df` fallback because no `df` was configured, while a nearby passage
  correctly identified that a `df` of `id` *is* configured and is never used — the second, correct
  argument is actually the stronger piece of evidence (a real fallback candidate exists and is
  still unused), but the draft carried both versions at once.

**Round 2** closed all six must-fix items:

- The false comments were corrected and cross-referenced to finding 94.
- `hl_wildcard_fl_does_not_error_on_a_matched_non_text_field` was rewritten to put the matching
  term in `q` instead of `fq` (`q=category:animals` rather than
  `q=*:*&fq=category:animals`), which makes the wildcard's exclusion behaviour actually
  observable, and an explicit assertion was added that no document in the `highlighting` block
  carries a `category` key at all. With the term in `q`, an implementation that dropped the
  `is_raw_string` filter would emit `{"doc4":{"category":["<em>animals</em>"]}}`, which the new
  assertion now catches.
- The self-contradicting `df` evidence was resolved in favor of the correct, stronger argument
  and written into both the doc comment and finding 94.

Verified by deliberate mutation and targeted-Edit revert: mutation A2 (drop only
`is_raw_string`) and mutation A (expand the wildcard to all stored fields, not just text) are
both caught by the corrected suite.

Per CLAUDE.md's default two-round cap: this review used both rounds and closed everything raised
in round 1. The cap was reached, not exhausted with anything outstanding — but per the pipeline's
own rule, two rounds is the default cap, not evidence the work has had all the review it could
use.

## Evidence

Re-run on `1c9788f`:

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test` — 758 passed, 40 suites, 0 failed.
- `cargo run -- coverage --format json` — **62/75** overall (up from 60/75), endpoints **9/9**,
  `request_semantics` **40/51**, `response_fields` **13/15**.

## Follow-ups

Filed during the #139 review, all open:

- **#181** — `hl.requireFieldMatch=true` is unimplemented and unfixtured. `search_api_solr`
  never sends `true`, so no fixture pins its semantics; Wayfinder currently accepts
  `hl.requireFieldMatch=true` and silently produces `false`'s output rather than rejecting it,
  which the issue explicitly flags as worse than the 400 it replaced.
- **#184** — `hl.fl=*` and explicit `hl.fl` now disagree on string fields: `hl.fl=category` is a
  200 with a snippet, `hl.fl=*` never surfaces `category` at all. Real Solr's
  `getStoredHighlightFieldNames` is `StrField`-inclusive; Wayfinder's wildcard chose exclusion
  because, for Wayfinder specifically, "include then produce nothing" and "exclude" are not the
  same outcome. Neither behaviour is fixture-pinned; closing this needs a capture of a real
  `solr:9` response where `hl.fl=*` could match a stored string field's value.
- **#185** — surfaced during round-2 verification of the divergence guard: with `hl.fl` naming a
  multi-valued string field explicitly, `doc4` (`category = ["animals"]`) gets a snippet but
  `doc1` (`category = ["animals","classic"]`) gets `{}` for the identical query term, because
  multi-valued string values are space-joined and then raw-tokenised into a single term, so a
  multi-value field never matches a single-value query term. Pre-existing, not introduced by
  #139; noted as related to #184 since a resolution of #184 toward excluding string fields from
  highlighting entirely could make this moot for the wildcard path while it still affects
  explicit `hl.fl`.

## Bottom line

`hl.fl=*` now expands to the schema's analyzed text fields and produces snippets for them;
`hl.mergeContiguous` and `hl.requireFieldMatch` no longer 400 under `strict_params = true`.
Explicit `hl.fl` fields keep the existing 400 behaviour for undefined or non-text names;
wildcard-derived fields are silently skipped when non-highlightable. The `string`-field
exclusion from the wildcard is a stated, deliberate divergence from Solr's `StrField`-inclusive
expansion, unsettled by any captured fixture, and — after round 2 — actually guarded by a test
that fails if the exclusion is dropped. All local gates are green (758/40 tests, fmt and clippy
clean), coverage moved 60/75 -> 62/75, and three follow-ups (#181, #184, #185) carry the
unfixtured `true` path, the wildcard/explicit disagreement on string fields, and a pre-existing
multi-valued string inconsistency found during review.
