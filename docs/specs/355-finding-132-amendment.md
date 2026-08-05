> **Historical implementation record.** This completed spec does not define current requirements or future work.

# #355 — correct finding 132: the client's JSON facet is nested terms facets

Branch: `355-finding-132-amendment`. Group C, but docs-only — land any time.

## The issue's framing is now out of date, in a good way

#355 was written to land **before** #343 (JSON Facet API) so that #343 would not
build to the wrong first target. **#343 has already landed** — PR #365,
`src/json_facet.rs`.

And it built the right thing. `src/json_facet.rs` implements `type: terms` with
nesting, `local_key` echoed verbatim as the response key (`:105`), and
`limit: -1` meaning unlimited (`:36`, `:518-523`), with fixtures
`jf343_terms_nested.json`, `jf343_deep_max.json`, `jf343_terms_limit.json`.

So the scope collapses. **Verify first, then write docs.**

## Step 1 — verify the implementation actually matches the correction

Before amending anything, confirm against `src/json_facet.rs` and its fixtures
that #343 built the corrected shape and not the one finding 132 described:

- nested `terms` facets are the primary supported path
- `local_key` is the response key
- `limit: -1` is handled as the normal case, not an edge case
- `max()` aggregation works, as the fallback it actually is

**If any of these is missing, this issue is not docs-only** — it becomes an
implementation gap in #343, and the PR body must say so plainly rather than
amending the finding and calling it done.

## Step 2 — the correction to record

What finding 132 got right: `search_api_solr` reads `_version_` only through a
JSON facet, never via `stats.field`, never writing it, never `versions=true`.

What it got wrong: the shape. It presents `max(_version_)` as *the* read path.
`doDocumentCounts()` (`SearchApiSolrBackend.php:4895-4930`) shows the reverse —
the primary request is nested `terms` facets, and `max(_version_)` is the
**exception fallback**:

```php
try {
  // PRIMARY: nested terms facets
  $json_facet_query = $facet_set->createJsonFacetTerms([
    'local_key' => 'siteHashes', 'limit' => -1, 'field' => 'hash',
  ]);
  $nested = $facet_set->createJsonFacetTerms([
    'local_key' => 'numDocsPerIndex', 'limit' => -1, 'field' => 'index_id',
  ], FALSE);
  $json_facet_query->addFacet($nested);
}
catch (\Exception) {
  // FALLBACK, non-Drupal indexes only: the "most minimalistic facet we can
  // think of" over the one field guaranteed to exist
  $facet_set->createJsonFacetAggregation([
    'local_key' => 'maxVersion', 'function' => 'max(_version_)',
  ]);
}
```

Base query: `+hash:* +index_id:*` as a `{!key=search_api}` filter query, `rows=1`,
`fl=id`.

`getMaxDocumentVersions()` (`:4987+`, reached from `:1064`) is the other
JSON-facet caller — `terms` facets on `hash`, `index_id` and
`ss_search_api_datasource`, plus per-datasource `max(_version_)` aggregations
(`:5052-5095`).

Confirm these line numbers against the vendored source and report the real ones.

## Step 3 — the wire detail that may be worth its own fixture

`:5079-5085`: for Solr >= 8.1.0 the module forces `setOmitHeader(FALSE)` on the
fallback query, because with headers omitted the facet triggers a
**`NullPointerException` inside Solr itself** (SOLR-13509). So a real client
sends `omitHeader=false` here specifically, against its own default.

Two things follow:

- **Capture it** if no fixture covers it — check `tests/omit_header.rs` and the
  existing fixtures first; this may already be covered.
- Wayfinder has no reason to reproduce the NPE, which makes this a candidate
  **ratified divergence** — same category as the `json.nl=garbage` truncated-JSON
  case in finding 128. If you conclude it is one, record it in PRD §5 and
  `EXPECTED_DIVERGENCES` rather than leaving it implicit.

## Scope

Documentation and scope work. Amend finding 132 and the PRD §5 v3 `_version_`
subsection to the corrected shape.

**Follow the batch amendment convention** (`docs/specs/README.md`): leave finding
132's text in place, append a bold
`**Amended by finding N (YYYY-MM-DD):**` line to it, and write the correction as
a new numbered finding at the bottom. Do not edit the body in place — other
documents cite findings by number and content.

Run `tests/finding_citations.rs` afterwards.

## Files

**You own:** `docs/solr-ref-findings.md` (append-plus-amend), PRD §5.

**Sibling conflict:** #351 also amends a finding in the same file. Same
convention, so they merge mechanically — but rebase and re-run the gates if it
lands first.

## Definition of done

- Step 1 verification reported: either #343 matches the corrected shape, or the
  gap is named explicitly as an implementation issue.
- Finding 132 amended per convention; PRD §5 updated.
- The `omitHeader=false` / SOLR-13509 detail either covered by an existing
  fixture, newly captured, or ratified as a divergence — with the choice stated.
- `tests/finding_citations.rs` green; Rust gates clean.
