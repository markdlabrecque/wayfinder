# Report: facet.field/facet.query errors omit Solr's `response` block

- Branch: `35-facet-error-response`
- Issue: [#35](https://github.com/markdlabrecque/wayfinder/issues/35) — "v1: facet.field error on
  an unknown field omits the response block Solr includes". Working tree at the time of this
  report is uncommitted (`git status` shows 6 modified files, no local commits yet; branch is
  behind `origin/main` by 8 commits it has not rebased onto).
- Pipeline: test-writer -> implementor -> reviewer (1 round: BOUNCE with 2 doc-only must-fix
  items, then APPROVED) -> reporter (this report).

## What was built

1. **`WfError::with_response` (`src/error.rs`).** A new builder method that attaches an optional
   `response` block to a `WfError`, rendered between `responseHeader` and `error` in the
   `Envelope::WithParams` case (Solr's own key order in `facet_unknown_field.json` and
   `facet_err_query_single.json`). `WfError` gained a `response: Option<Box<Value>>` field
   (boxed, alongside the pre-existing `params` field which was also boxed in this change, to keep
   `WfError` under clippy's `result_large_err` threshold). `None` renders with no `response` key
   at all, preserving the existing behaviour for errors that never call `with_response` (e.g.
   `facet_err_range_single.json`). Two new unit tests in `src/error.rs` pin: (a) that
   `with_response` attaches the given block in the right key position, and (b) that omitting the
   call leaves no `response` key.

2. **`PreQueryFacetError` marker (`src/facet.rs`).** Solr detects a broken `facet.range` before
   running the base query at all (no `response` in its fixtures), but detects a broken
   `facet.query`/`facet.field` only after the base query has run (its fixtures do carry
   `response`). `facet_counts` previously returned one `Result` regardless of which sub-check
   failed, so Wayfinder could not tell the two cases apart. `PreQueryFacetError` wraps the
   original `anyhow::Error` from `facet_ranges` (forwarding `Display` verbatim, so no error
   message anywhere changes) so `src/lib.rs`'s `select` handler can `downcast_ref` on it and
   decide whether to attach `response`.

3. **`response` block construction moved earlier in `select` (`src/lib.rs`).** The `response`
   `Map` (`numFound`/`start`/optional `maxScore`/`numFoundExact`/`docs`) is now built *before*
   `facet::facet_counts` runs, rather than after. When `facet_counts` returns an error, `select`
   checks whether it downcasts to `facet::PreQueryFacetError`: if so (a `facet.range` failure) no
   `response` is attached, matching Solr; otherwise (`facet.query`/`facet.field`) the
   already-built `response` map is cloned onto the `WfError` via `.with_response(...)`.

4. **Test evidence in `tests/faceting.rs`.** Section 23 (`facet_field_error_still_carries_the_base_querys_response_block`,
   `facet_query_error_still_carries_the_base_querys_response_block`) exercises the real handler
   end-to-end against the exact requests `facet_unknown_field.json` and
   `facet_err_query_single.json` were captured from, asserting `response.numFound`, `.start`,
   `.numFoundExact`, `.docs`, and that `error` is still present alongside it.

5. **`docs/solr-ref-findings.md`.** New numbered finding 43 documents the pre-query/post-query
   split (facet.range detected before the base query runs vs. facet.query/facet.field detected
   after), names the fixtures it is pinned against, and states which
   `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` entries it closes. The harness section's stale
   passage describing the expected-divergence list as carrying "one entry" was corrected to say
   the list is now empty.

## `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` entries removed

All 5 self-expiring entries under issue #35 were deleted from `tests/differential.rs`, verified
against the real hermetic differ (not skipped/ignored) via the section-23 tests plus a full
`cargo test` run:

- `facet_unknown_field`
- `facet_err_query_single`
- `facet_err_field_single`
- `facet_err_query_field`
- `facet_err_query_vs_unfacetable`

All five shared the identical root cause (facet.query/facet.field errors are post-query and
Solr's fixture carries `response`; Wayfinder omitted it). `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`
is now an empty list (`&[]`), with a comment explaining why and pointing at `PreQueryFacetError`
as the mechanism that keeps future facet.range errors from being lumped in with this fix.

The 4 range-triggered rows from the same manifest-errors set — `facet_err_range_single`,
`facet_err_query_range`, `facet_err_field_range`, `facet_err_all_three` — were **correctly left
untouched**. Wayfinder already matched Solr for those before this change (Solr detects
`facet.range` errors before the base query runs, so its own fixtures for them carry no
`response`, and Wayfinder's un-fixed code also omitted `response` unconditionally, so the two
sides already agreed there) and this diff's `PreQueryFacetError` split preserves that: `select`
still omits `response` for anything wrapped in `PreQueryFacetError`.

## Test evidence

- `cargo test`: **256 passed**, 0 failed, across 11 suites.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.

## Review outcome

**Round 1: BOUNCE**, two doc-only must-fix items, returned to the original implementor and fixed:

1. `docs/solr-ref-findings.md`'s harness-section passage was stale — it still described
   `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` as carrying one entry (`facet_unknown_field`) after the
   fix had emptied the list to zero. Corrected to state the list is now empty and summarize what
   closed it.
2. No numbered finding existed yet documenting the pre-query/post-query facet-error split that
   the fix's own code comments describe (in `src/facet.rs` and `src/lib.rs`). Added as finding 43,
   citing the specific fixtures (`facet_unknown_field.json`, `facet_err_query_single.json` vs.
   `facet_err_range_single.json`) that pin the behaviour.

**Round 2: not needed — APPROVED** on the resubmission; both fixes were doc-only and verified
directly against the diff.

The 2-round cap was not exhausted (round 1 bounce, resubmission approved without a second bounce),
but per the pipeline's own convention this report still notes: the round-1 review was
doc-focused and did not include a second independent pass over the Rust logic itself
(`with_response` key ordering, the `downcast_ref` wiring, and the boxing change to `WfError`)
beyond what round 1 already covered — a further review pass on the code, not just the docs, would
still be worthwhile if this were revisited.

## Follow-ups noted by the reviewer (non-blocking, deferred)

1. **Unfixtured combination:** `rows>0` and/or `fl=score` alongside a facet.query/facet.field
   error is not pinned by any captured fixture — every fixture this fix is verified against uses
   `rows=0`. Whether Solr's `response.docs`/`maxScore` render the same way in that combination
   when faceting also fails is unverified.
2. **Untested failure mode inside `facet_ranges`:** `PreQueryFacetError` currently wraps *any*
   error `facet_ranges` returns, including a hypothetical internal/`count()` failure distinct from
   a validation failure (e.g. an unfacetable-field check). Such a failure would still be
   categorized as pre-query/no-`response` by the current code, and there is no fixture covering
   that path to confirm Solr would agree.

## Pointers

- Production code: `src/error.rs` (`WfError::with_response`, `response` field boxing),
  `src/facet.rs` (`PreQueryFacetError`), `src/lib.rs` (`select` handler — `response` map built
  before `facet::facet_counts`, error-path branching on `downcast_ref::<facet::PreQueryFacetError>`).
- Tests: `src/error.rs` unit tests (`with_response_places_response_between_header_and_error`,
  `without_with_response_there_is_no_response_key`), `tests/faceting.rs` section 23
  (`facet_field_error_still_carries_the_base_querys_response_block`,
  `facet_query_error_still_carries_the_base_querys_response_block`), `tests/differential.rs`
  (`EXPECTED_DIVERGENCES_MANIFEST_ERRORS` now `&[]`).
- Docs: `docs/solr-ref-findings.md` — finding 43 (new), harness-section correction to the
  now-empty expected-divergence list.
- Issue: [#35](https://github.com/markdlabrecque/wayfinder/issues/35), including a prior comment
  from the #31/#33 work recording that all 5 entries (not just `facet_unknown_field`) must expire
  together — confirmed done by this diff.
