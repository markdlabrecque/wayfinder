> **Historical implementation record.** This completed spec does not define current requirements or future work.

# PREP-1 (#368) — Vendor the full `search_api_solr` 4.4.0 source

Issue: **#368**. Branch: `368-vendor-source`. Claim the issue before starting
(`gh issue edit 368 --add-assignee @me`) and open the PR with `Closes #368`.

**Lands on `main` before any other spec in this batch starts.**

## The problem

`coverage/search_api_solr_4.4.0_source/` currently contains three files:

```
src/SolrConnector/SolrConnectorPluginBase.php
src/SolrSpellcheckBackendTrait.php
src/Plugin/search_api/backend/SearchApiSolrBackend.php
```

Every issue in this batch cites line numbers in files that are **not there**:
`src/Hook/SearchApiSolrHooks.php`, `src/Solarium/Autocomplete/Query.php`,
`modules/search_api_solr_admin/**`, `config/install/*.yml`,
`modules/search_api_solr_legacy/**`.

This matters beyond convenience. Issue #352 overturns a conclusion that is
committed to this repo — the #291 report
(`docs/reports/2026-08-03-suggestcomponent-autocomplete.md`) states that
`getSuggesterQuery()` is "defined but never called anywhere in the backend",
which is true of the three-file snapshot and false of the full module. The
compatibility contract says ground truth lives in the tree. Right now the
evidence for half this batch does not.

## Scope

Vendor the complete 4.4.0 source under
`coverage/search_api_solr_4.4.0_source/`, preserving the module's own directory
layout so the existing citation style (`coverage/.../SearchApiSolrBackend.php:1485`)
keeps working unchanged for the three files already there.

Include at minimum, because specs in this batch cite them:

- `src/Hook/SearchApiSolrHooks.php` (#352 — the cron `suggest.buildAll` caller)
- `src/Solarium/Autocomplete/Query.php` (#351 — the `autocomplete` handler)
- `modules/search_api_solr_autocomplete/**` (#351 — the three suggester plugins)
- `modules/search_api_solr_admin/**` (#354 — the three admin endpoints)
- `config/install/**` (#351, #352 — the request-handler config entities)

Exclude `tests/`, `.github/`, and any vendored dependencies of the module itself
if they inflate the tree without being cited. State in the PR what you excluded
and why.

Record the exact provenance in the PR body and in a short header comment or
`PROVENANCE` file alongside the tree: where the source came from (the drupal.org
release tarball or the git tag), the version, the retrieval date, and a checksum
of the archive. Someone auditing a finding in a year needs to be able to confirm
they are reading the same bytes you were.

## Verify before you start

1. Confirm the version really is 4.4.0 — read `search_api_solr.info.yml` in the
   retrieved tree and check it against the three files already vendored. If the
   already-vendored files differ from the same files in your download, **stop
   and report it**: it means the existing citations point at a different version
   than the one you are adding, and every finding derived from them needs
   rechecking. That is a much bigger problem than this chore, and it must not be
   silently absorbed.
2. Confirm `src/Hook/SearchApiSolrHooks.php` actually contains the cron path
   #352 describes (a `suggest.buildAll` request via `fireAndForget`, gated on
   Drupal-only-writeable + updates since last build + 1800s). Report the real
   line numbers. If it is not there, #352's premise is wrong and that spec stops
   until it is resolved.

## Definition of done

- The full 4.4.0 tree is committed under `coverage/search_api_solr_4.4.0_source/`
  with provenance recorded.
- Existing citations to the three original files still resolve to the same
  content at the same line numbers (spot-check the ones in
  `docs/solr-ref-findings.md` — `tests/finding_citations.rs` may already do this
  for you; run it).
- `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
  all clean. This PR should not change any Rust behaviour; if a test's result
  changes, something is wrong.
- The PR body reports the verification results from both numbered items above,
  including the real `SearchApiSolrHooks.php` line numbers for #352 to cite.
