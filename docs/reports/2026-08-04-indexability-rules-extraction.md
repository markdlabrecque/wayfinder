# Issue #264 — Indexability rules and extraction limits

Date: 2026-08-04
Branch: `markdlabrecque/issue-264-indexability-rules-extraction`
Part of epic #268 (document text extraction). Prior art: `search_api_attachments`
10.0.x `src/ExtractFileValidator.php` and `FilesExtractor::isFileIndexable()` /
`::limitToAllowedNumber()` / `::limitBytes()` (GPL-2.0-or-later, same licence as
this module; class name and method roles mirror it deliberately).

## The blocker, and why the rules shipped anyway

#262 (the extraction tracer / processor) is the listed blocker. It blocks the
*integration* of these rules — where they get *called* — not the guard logic
itself. `ExtractFileValidator` is built as a **standalone, config-decoupled**
class: every rule takes its inputs as explicit parameters, with no processor
`$this->configuration` coupling and no service registration. That makes each
rule independently unit- and mutation-testable now, without depending on #262's
still-open decisions (field naming, where extracted text lands). #262's
processor wires it; #266's settings form feeds it.

This keeps #264 off #262's hot files (no `WayfinderClient.php`, no processor
plugin) and off #266's config schema — the validator's public API (method names
+ params) is stable regardless of how those land.

## What shipped

- `src/ExtractFileValidator.php` — five rules + the composite guard, pure logic:
  - `getExcludedMimes(array $extensions): string[]` — extensions → MIME types via
    the injected guesser, called on a synthetic `dummy.<ext>` filename, de-duped.
  - `isFileSizeAllowed(FileInterface $file, string $maxFilesize): bool`
  - `isPrivateFileAllowed(FileInterface $file, bool $excludedPrivate): bool`
  - `limitToAllowedNumber(array $fileIds, int $numberIndexed): array`
  - `limitBytes(string $extractedText, string $numberFirstBytes): string`
    (multibyte-safe via `mb_strcut`)
  - `isFileIndexable(FileInterface $file, array $excludedMimes, string $maxFilesize, bool $excludedPrivate): bool`
    — composes rules 1+2+3.
- `tests/src/Unit/ExtractFileValidatorTest.php` — one test per rule (data-driven
  where the rule has a boundary surface) + the default-extensions fallback + the
  composite. 21 cases, 24 assertions.
- `composer.json` — `autoload-dev` gains `Drupal\file\` and `Drupal\user\` PSR-4
  shims (see below).

## Access-control decision (made explicit, not inherited)

Per the issue: indexing a private file's contents makes them searchable through
the item that references it, because Search API access control is per item, not
per attachment. **Policy: exclude private files by default**
(`$excludedPrivate = TRUE`), matching `search_api_attachments`. This is the safe
choice — a site that understands the leakage risk opts in by setting
exclude-private off. Documented in `isPrivateFileAllowed()`'s docblock.

## Why these coexist with the server-side budgets (#257)

Different trust boundaries. #257 protects Wayfinder from *any* client and stays
fail-closed regardless of what Drupal does; these rules stop Drupal from
uploading files it already knows will be rejected and control index bloat /
relevance skew the server cannot see. Weakening one because the other exists is
a bug. `limitBytes` in particular is a Drupal-side relevance cap, not a defence
against the upload budget — noted in its docblock.

## Deliberate non-ports (scope discipline)

- `isFileIndexable()` does **not** fold in `file_exists($uri)` or
  `$file->isPermanent()`. Those are processor-side preconditions in the prior
  art (filesystem-existence is non-hermetic; permanence is upstream of the five
  rules), and #262 owns them. They are out of scope for #264's five rules.
- The default excluded-extensions list is copied verbatim from upstream rather
  than "improved"; a site overrides it through processor config (#266).

## Mutation matrix

Every rule is a refusal/bound — its whole value is in not doing work — so each is
mutation-tested: break the guard deliberately, confirm its test catches it,
revert (`/tmp/mutation_test.sh`, pristine-backup + restore, verified with a
post-restore `diff` and a full green run).

| Rule | Mutation | Result |
|---|---|---|
| 1 — excluded MIME mapping | drop the `'dummy.'` prefix passed to the guesser | `testGetExcludedMimesMaps…` failed (Failures: 1); reverted |
| 2 — max file size | `<=` → `<` | boundary case failed (Failures: 1); reverted |
| 3 — private-file policy | drop the `!` negation | two private/public cases failed (Failures: 2); reverted |
| 4 — files per field | slice length `+1` (lets one extra file through) | slice case failed (Failures: 1); reverted |
| 5 — extracted-bytes cap | `mb_strcut` → `substr` | multibyte case failed (Failures: 1); reverted |

The rule-5 multibyte case is the load-bearing one: a 2-byte budget against
`'aあb'` (`'あ'` is 3 UTF-8 bytes) must yield `'a'`. `substr` returns a broken
2-byte sequence, which is exactly the regression the test pins.

## Test-harness shim for `Drupal\file\`

The unit sandbox autoloads `Drupal\Core\` / `Drupal\Component\` (drupal/core
maps them) but not core *module* namespaces, which Drupal's module classloader
resolves at runtime on a real site. `FileInterface` is a core module class and
transitively extends `Drupal\user\EntityOwnerInterface`, so both `Drupal\file\`
and `Drupal\user\` need PSR-4 shims in `autoload-dev` — the same documented
pattern already used for `Drupal\search_api\`. The closure is small and closed
(`EntityOwnerInterface` is standalone; the other parents are under the
already-mapped `Drupal\Core\`). `autoload-dev` is dev-only; a real site
(composer/installers → `web/core/modules/...`) is unaffected. Forward-useful:
#262–#267 all touch `FileInterface`.

## Verification

```
$ cd drupal/search_api_wayfinder && vendor/bin/phpunit
Tests: 155, Assertions: 239  (134 baseline + 21 new), OK
```

CI gate is the `search-api-wayfinder-unit` job (`vendor/bin/phpunit`, hermetic —
no network, no Docker). The 101 PHPUnit deprecations are environmental: Drupal
core's `FileInterface` parent chain emits backcompat `E_USER_DEPRECATED` on load,
counted because `phpunit.xml.dist` has no `<source>` filter; `failOnDeprecation`
is not set and the baseline already runs green with 93. None originate in the
new code.

## Follow-ups

- **#262** wires `ExtractFileValidator` into the extraction processor's
  `isFileIndexable`/`limitToAllowedNumber`/`limitBytes` call sites, adding the
  `file_exists`/`isPermanent` preconditions this issue deliberately left out.
- **#266** exposes the five parameters as processor config (excluded extensions,
  max filesize, exclude-private, files per field, extracted-bytes cap) with
  exportable config schema and the defaults this validator assumes
  (`DEFAULT_EXCLUDED_EXTENSIONS`, exclude-private = TRUE).
