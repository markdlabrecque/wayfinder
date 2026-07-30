# Issue #83 — fix `FieldMapper::formatValue()` serializing `TextValue` objects to `{}`

Part of #57 (search_api_wayfinder backend), follow-up from #81's round-2 review (which discovered
this bug independently while tracing the failure blocking the sibling integration-harness ticket,
#80).

## What was built

`FieldMapper::formatValue()`'s `default` branch returned fulltext (`text`-type) field values
untouched, on the (wrong) assumption they arrive as plain PHP strings. Real Search API hands
`text`-type fields `Drupal\search_api\Plugin\search_api\data_type\value\TextValue` objects
(`TextDataType::getValue()`), which implement `TextValueInterface extends \Stringable`. All of
`TextValue`'s properties are `protected` and the class is not `JsonSerializable`, so
`json_encode()` of one produces `{}` — exactly the malformed body that broke real indexing in the
#80 integration harness (`SearchApiException: field tm_body expects a string value, got {}`,
surfaced against a live Wayfinder server, not the unit suite).

`src/FieldMapper.php` now adds a `text` case to `formatValue()`'s switch: casts the value through
`(string)` when it is a `\Stringable` (which delegates to `TextValue::__toString()` ->
`toText()`, reflecting the object's *current* text, not a constructor snapshot — important because
`setText()` can mutate it after construction), otherwise passes it through unchanged.
`string`-type handling was deliberately left alone: `StringDataType::getValue()` was checked and
always returns a plain scalar, with no object path, so only `text` needed the fix (per the
ticket's explicit instruction to verify rather than assume).

Files changed (all uncommitted on top of #81's cardinality fix, on branch
`83-search-api-wayfinder-textvalue`):

- `drupal/search_api_wayfinder/src/FieldMapper.php`
- `drupal/search_api_wayfinder/tests/src/Unit/FieldMapperTest.php`
- `drupal/search_api_wayfinder/tests/src/Unit/DocumentBuilderTest.php`

## Pipeline

1. **test-writer** added two regression tests to `FieldMapperTest.php`, both using the real
   `TextValue` class (not a mock or hand-rolled stub, per #83's own diagnosis that a mock-shape
   mismatch is exactly what let this bug ship invisible): one proving `formatValue()` returns a
   plain string for a `TextValue('Some fulltext body')` input (and explicitly asserts the result
   is *not* `'{}'`, guarding against a superficial fix that stringifies to the wrong thing); a
   second constructing a `TextValue`, mutating it via `setText()`, and asserting `formatValue()`
   reflects the mutated text — proving the fix reads current state via `__toString()`, not a
   constructor-time snapshot. It also converted `DocumentBuilderTest.php`'s existing text-field
   mock from a plain PHP string to a real `TextValue` (both in the existing single-value test and
   a new multi-valued end-to-end test asserting the built doc round-trips through
   `json_encode()`/`json_decode()` as plain strings) — that plain-string mock was exactly the
   wrong-shaped mock #83 names as having let this bug ship invisible through M1 and the #81
   cardinality work.
2. **implementor** added the one `text` case described above. Reached 56/56 green.
3. **reviewer approved outright (round 1, no bounce)**, having verified: `TextValueInterface`
   explicitly extends `\Stringable` (not relying on PHP 8's implicit-`Stringable` inference from a
   bare `__toString()` method); `__toString()` delegates to `toText()`, so the fix reflects
   current object state rather than a snapshot; the type-boundary claim (only `text` needs this
   cast) checked against all six data types `WayfinderBackend::supportsDataType()` whitelists;
   and test realism — the regression tests construct a real `TextValue`, not a stub whose
   behavior the test author merely assumed.

## Test evidence

```
$ cd drupal/search_api_wayfinder && vendor/bin/phpunit
PHPUnit 9.6.35 by Sebastian Bergmann and contributors.

........................................................          56 / 56 (100%)

Time: 00:00.021, Memory: 10.00 MB

OK (56 tests, 79 assertions)
```

Run by the reporter directly against the working tree on `83-search-api-wayfinder-textvalue`, not
copied from an earlier stage's claim.

## Review outcome

The reviewer approved on round 1 — no bounce, so only one review pass was used against the
pipeline's two-round cap. This ticket's scope was narrow (one switch case, three new/updated
tests) and the reviewer's verification (interface hierarchy, current-state semantics, type-boundary
completeness, test realism) was substantive rather than a rubber stamp, but per the pipeline's own
convention this is noted: a single-round approval means the work has had less independent scrutiny
than #81's two-round pass, and the follow-ups below were surfaced by the reviewer's own checking,
not left unexamined.

## Follow-ups from review (deferred, not fixed in this branch)

1. **`formatValue()`'s `default:` branch still returns unhandled value objects untouched.** There
   is at least one live path that bypasses the type whitelist entirely: `Field.php` skips
   `getValue()`'s normal type-plugin path when the data type plugin is missing or disabled, and
   can store a raw value under a type id that lands in `default:`. That is the same `{}`-serialization
   bug class as #83 itself, just with a narrower trigger. Not fixed in this ticket; no issue filed
   yet.
2. **`toText()` re-joins fulltext tokens with a single space when preprocessors have set tokens,
   discarding per-token boosts** (search_api_solr indexes tokens individually rather than
   re-joining them). Acceptable for M1's scope, but worth naming as a known ceiling on the fix's
   fidelity to full Search API semantics.
3. **The mutation-reflects-current-state test only exercises `setText()`, not `setTokens()`** — the
   tokens-re-join path of `toText()` (see follow-up 2) isn't directly pinned by a test, so a future
   regression in *that* path would not be caught by this branch's tests.
4. **#83's acceptance criterion about #80's integration harness getting past indexing once
   rebased onto this fix is an outcome to verify separately** — this branch, being unit-tests-only
   per the ticket's own scope note ("do not touch tests/integration/"), cannot demonstrate that
   itself. Recorded here as **unverified**, to be confirmed when #80 is rebased onto this fix (and
   #81's).

## Acceptance criteria (from #83) — status

- [x] `formatValue()` returns a plain scalar string for `TextValue`-shaped input, not a
      serialized-to-`{}` object
- [x] Regression test added using a realistic (non-plain-string) value shape (the real `TextValue`
      class, including a mutation-based test)
- [x] `vendor/bin/phpunit` green (56/56)
- [ ] #80's integration harness getting past indexing once rebased onto this fix — **not
      verified by this branch**; explicitly out of this ticket's scope per its own instructions,
      and unverified as of this report
- [x] docs/reports entry (this document)
