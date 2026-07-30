# Issue #76 — M2 filters, sorts, and paging

## Completed

Implemented M2 query translation, field mapping, document handling, and client query serialization in:

- `drupal/search_api_wayfinder/src/DocumentBuilder.php`
- `drupal/search_api_wayfinder/src/FieldMapper.php`
- `drupal/search_api_wayfinder/src/QueryBuilder.php`
- `drupal/search_api_wayfinder/src/WayfinderClient.php`
- Corresponding unit tests under `drupal/search_api_wayfinder/tests/src/Unit/`

Pinned provenance: `search_api_solr` 4.3.13. `QueryBuilder` adapts its method-level filter-query behavior—especially `createFilterQuery()`-style condition/operator handling, one-value range normalization, range `NULL`/`*` endpoints, and list validation—so Wayfinder emits compatible Solr-wire queries rather than reproducing Solr configuration behavior.

The mixed-`NULL` `IN` behavior is a deliberate correction: a list containing values and `NULL` emits a value-match alternative **or** a missing-field alternative. This is intentional rather than blindly inheriting upstream behavior.

## Verification

- Initial red PHPUnit run: 36 tests, 17 failures, failing for the missing M2 behavior.
- Targeted implementation fixes and mutation checks passed; validation guards were deliberately broken and confirmed caught, then restored.
- Independent final review gate passed:
  - PHPUnit: 82/82 tests, 110 assertions.
  - Root and bench `fmt`, `clippy`, and test gates: green.
- Two review rounds completed; all findings were resolved.

## Workflow note

The ownership/stale-guard artifact caused workflow-only friction. It was not a technical failure and did not affect the delivered behavior or verification result.

## Follow-up

- Before M3, generalize repeated-array query serialization. Current repeated `fq` serialization is correct.

No unresolved M2 blockers or other follow-ups.
