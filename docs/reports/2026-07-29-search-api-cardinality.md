# Issue #81 — fix `FieldMapper::isMultiValued()` to use real storage cardinality

Part of #57 (search_api_wayfinder backend), follow-up from #75 (M1) round-2 review.

## What was built

`FieldMapper::isMultiValued()` (added in M1, #75) used
`DataDefinitionInterface::isList()` as its cardinality signal. Against real
Drupal this misclassifies every content-entity field as multi-valued:
`BaseFieldDefinition`/`FieldConfigBase` are lists by construction, so `isList()`
returns `TRUE` unconditionally regardless of the field's actual cardinality.
Every field would have mapped to the multi-valued dynamic-field prefix
(`tm_title` instead of `ts_title`, etc.), diverging from `search_api_solr`'s
naming convention and blocking M2 (#76), which needs single-valued fields for
sorts.

`src/FieldMapper.php` now walks the field's colon-separated property path
through the index's property definitions (a Wayfinder-shaped port of
`search_api_solr`'s `getPropertyPathCardinality()` / `FieldsHelper::getInnerProperty()`,
both GPL-2.0-or-later, per locked decision 1 in
`docs/plans/57-search-api-wayfinder-backend.md`):

- Where a path segment is a `FieldDefinitionInterface`, the real signal is
  `getFieldStorageDefinition()->getCardinality()` — `1` is single-valued,
  anything else (`-1` unlimited, or `>1`) is multi-valued.
- Where a segment is not a field definition (a plain nested TypedData
  property), fall back to `isList()`.
- Before descending to the next path segment, the segment is unwrapped
  (`unwrapProperty()`: `ListDataDefinitionInterface::getItemDefinition()`,
  then `DataReferenceDefinitionInterface::getTargetDefinition()`) so the walk
  can actually reach nested/reference properties, since
  `FieldDefinitionInterface` is a `ListDataDefinitionInterface`, not a
  `ComplexDataDefinitionInterface`, and looks like a dead end without
  unwrapping first.

Files changed (all uncommitted on top of the M1 commit `0252568`, on branch
`81-search-api-wayfinder-cardinality`):

- `drupal/search_api_wayfinder/src/FieldMapper.php`
- `drupal/search_api_wayfinder/tests/src/Unit/FieldMapperTest.php`
- `drupal/search_api_wayfinder/tests/src/Unit/DocumentBuilderTest.php`
- `drupal/search_api_wayfinder/tests/src/Unit/QueryBuilderTest.php`

## Pipeline

1. **test-writer** added regression tests to `FieldMapperTest.php` proving the
   bug (a list-by-construction property with field-storage cardinality 1 must
   resolve single-valued — the exact shape the old bare-`DataDefinitionInterface`
   mocks couldn't distinguish), and upgraded `DocumentBuilderTest.php` /
   `QueryBuilderTest.php` mocks from a bare `DataDefinitionInterface` to
   realistic `FieldDefinitionInterface`/`FieldStorageDefinitionInterface`
   shapes.
2. **implementor round 1**: replaced the `isList()` check with
   `FieldDefinitionInterface::getFieldStorageDefinition()->getCardinality()`.
   Reached 51/51 green.
3. **reviewer round 1: BOUNCED.** The property-path descent was dead for every
   real content-entity path — `FieldDefinitionInterface` is a list, not a
   `ComplexDataDefinitionInterface`, so the walk's complex-type check failed
   after segment 1 and never descended into nested/reference properties. This
   was worse than the original bug: it silently caused `DocumentBuilder` to
   drop all but the first value for a specific multi-hop shape (a
   single-valued reference field pointing at a multi-valued target field on
   the referenced entity), since the whole path would wrongly resolve
   single-valued and `DocumentBuilder::buildAddCommand()` collapses
   single-valued output to `$formatted[0] ?? NULL`. **This is the value of the
   round-1 review: the fix in round 1 passed its own tests green while
   introducing a data-loss bug the test-writer's original mocks could not have
   caught, because they didn't yet exercise multi-segment paths.**
4. **implementor round 2**: added `unwrapProperty()` (mirroring
   `search_api_solr`'s `FieldsHelper::getInnerProperty()`) to unwrap
   `ListDataDefinitionInterface`/`DataReferenceDefinitionInterface` before the
   complex-type check, enabling real descent. Added two new regression tests:
   one covering multi-segment descent (`body:value` shape, resolves single),
   and one covering the reference-to-multi-valued-target data-loss case
   (`field_ref:entity:field_tags`, resolves multi). Also added an
   `instanceof FieldStorageDefinitionInterface` guard around
   `getFieldStorageDefinition()` and simplified the cardinality comparison to
   `!== 1`. Reached 53/53 green.
5. **reviewer round 2: APPROVED**, with two tracked (non-blocking) follow-ups
   (below).

## Test evidence

```
$ cd drupal/search_api_wayfinder && vendor/bin/phpunit
PHPUnit 9.6.35 by Sebastian Bergmann and contributors.

.....................................................             53 / 53 (100%)

Time: 00:00.021, Memory: 10.00 MB

OK (53 tests, 73 assertions)
```

Run by the reporter directly against the working tree on
`81-search-api-wayfinder-cardinality`, not copied from an earlier stage's
claim.

## Review outcome

Two full rounds were used (the pipeline's cap). Round 1 found a real,
data-losing regression that the round-1 implementation's own test additions
did not catch — a genuine example of why the review gate exists, not a
rubber-stamp pass. Round 2 approved, but **this ticket has now used both
available review passes**; the two follow-ups below were deferred rather than
fixed in-branch, and per the pipeline's own rule this work could use further
review passes before being treated as fully hardened, particularly around
property-path edge cases not yet covered by a test (multi-bundle references,
storage-definition exceptions).

## Follow-ups (deferred, not fixed in this branch)

1. **Unrestricted entity references miss configurable fields.**
   `FieldMapper.php` uses bare
   `EntityDataDefinitionInterface::getPropertyDefinitions()`, which only
   returns base fields unless the reference is restricted to exactly one
   bundle. Configurable fields on an unrestricted reference (e.g.
   `field_ref:entity:field_tags` where the target isn't bundle-restricted)
   are missed by the property-path walk — the same class of data-loss bug as
   the round-1 regression, in a narrower case the current tests don't cover.
   The real fix is a `search_api`-style `getNestedProperties()`-equivalent
   that unions in bundle field definitions. Not addressed in this branch; no
   issue filed yet for this specific sub-case.
2. **`FieldConfig::getFieldStorageDefinition()` can throw `FieldException`.**
   The current guard (`instanceof FieldStorageDefinitionInterface`) only
   protects against a non-instanceof return; it does not catch the exception
   `getFieldStorageDefinition()` can itself throw. Nothing in the descent path
   catches it. Not addressed in this branch; no issue filed yet.
3. **`FieldMapper::formatValue()` serializes fulltext `TextValue` objects to
   `{}`.** Discovered independently by the round-2 reviewer while tracing the
   failure blocking the sibling integration-harness ticket (#80) — this is a
   *different* bug from #81's, not fixed by this ticket. Search API hands
   fulltext (`text`-type) fields `Drupal\search_api\...\TextValue` objects,
   not plain strings; `formatValue()`'s default branch returns them untouched,
   and `json_encode()` of a `TextValue` (protected properties, not
   `JsonSerializable`) produces `{}`, which Wayfinder correctly rejects
   (`field \`tm_body\` expects a string value, got {}`). Reproduced end-to-end
   against real Wayfinder via the #80 harness. **Tracked as
   [issue #83](https://github.com/markdlabrecque/wayfinder/issues/83)**
   (already filed at the time of this report).

## Acceptance criteria (from #81) — status

- [x] `isMultiValued()` uses field-storage cardinality, not `isList()`
- [x] Unit tests use a mock shape that would have caught the original bug
- [x] `vendor/bin/phpunit` green (53/53)
- [x] docs/reports entry (this document)
