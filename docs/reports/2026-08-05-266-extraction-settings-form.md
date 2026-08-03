# Issue #266 — Extraction settings form and config schema

Date: 2026-08-05
Branch: `markdlabrecque/issue-266-extraction-settings-form`
Part of epic #268 (document text extraction). Blocked by #264 (the indexability
rules this exposes). Prior art: `search_api_attachments`'
`src/Form/TextExtractorFormSettings.php` (GPL-2.0-or-later, same licence as this
module) — but only the *settings* half: that form is 407 lines mostly because it
chooses between six extraction backends. We have one extractor, so the backend-
selection UI is deliberately not ported.

## What shipped

The indexability rules and extraction settings from #264 are now exposed as
processor configuration, with a config schema so they export/import cleanly and
sensible defaults so the processor is useful with no configuration.

- `src/Plugin/search_api/processor/FileExtractionProcessorBase.php`
  - `implements PluginFormInterface` — search_api only renders a processor's
    config form when the plugin implements this interface (it checks
    `$processor instanceof PluginFormInterface` in `IndexProcessorsForm`).
  - `defaultConfiguration()` — six keys, each feeding an ExtractFileValidator
    rule (#264) or the cache/queue mode (#263):
    - `extraction_mode` (`string`, `inline`|`queue`, default `inline`)
    - `excluded_extensions` (`string`, space-separated, default the #264 constant)
    - `max_filesize` (`string` byte string, `0` = no limit, default `0`)
    - `excluded_private` (`boolean`, default `true` — the safe access-control default)
    - `number_indexed` (`integer`, `0` = no limit, default `0`)
    - `number_first_bytes` (`string` byte string, `0` = no limit, default `0`)
  - `buildConfigurationForm()` — one control per setting, shared by both
    processors (attached-file #262 and linked-file #265 run the same rules).
  - `validateConfigurationForm()` — byte-size fields must parse via
    `Drupal\Component\Utility\Bytes::validate()`.
  - `submitConfigurationForm()` — stores every key; normalises
    `excluded_extensions` to the canonical lowercase/de-duped/dot-stripped
    space-separated list that `ExtractFileValidator::getExcludedMimes()` will
    `explode(' ', ...)`.
- `src/Plugin/search_api/processor/FileExtraction.php` — dropped the now-
  redundant `defaultConfiguration()` override; the base owns it.
- `config/schema/search_api_wayfinder.schema.yml` —
  `plugin.plugin_configuration.search_api_processor.wayfinder_file_extraction`
  and `…wayfinder_linked_file_extraction`, both extending
  `search_api.default_processor_configuration` (so the inherited `weights` key
  keeps its schema) and declaring every key with the right scalar type.

## Why the form lives on the base class

Both `FileExtraction` and `LinkedFileExtraction` apply the same indexability
rules and extraction limits, so duplicating the form on each subclass would be
two copies to keep in sync. One form + one set of defaults on the shared base
means a future wiring change reads config identically for both processors. Every
test runs against both processors to pin that neither subclass can drift.

## Why `Bytes::validate`, not `Bytes::toNumber`, in the form validator

`Bytes::toNumber()` throws a `TypeError` on PHP 8 for inputs like `huge` (a unit
letter with no leading number — the `e` is a valid suffix letter, so it enters
the multiply branch with an empty numeric operand). The validator must *refuse*
bad input, not crash the form build, so it uses Drupal's own format validator
`Bytes::validate()` instead, which returns `false` and never throws. (`0` and
empty are always accepted as "no restriction", matching the validator's
semantics.) Note this differs from `ExtractFileValidator::isFileSizeAllowed()`,
which calls `toNumber()` directly — that is safe because the form has already
rejected unparseable values before they reach config; the validator trusts its
caller, the form does not.

## Scope: form + schema + defaults only

The acceptance for #266 is the config schema round-tripping and a settings form
with sensible defaults. It does **not** wire `ExtractFileValidator` into
`addFieldValues()` — that call-site work (the `isFileIndexable` /
`limitToAllowedNumber` / `limitBytes` application, plus the `file_exists` /
`isPermanent` preconditions) was explicitly left to a separate change in #264's
follow-up, and currently neither processor applies the rules at index time. This
issue stores exactly the values that wiring change will read; the form is a real,
validating contract, not dead UI, because the next slice consumes it. Flagged as
a follow-up rather than silently broadening scope.

## Mutation matrix

Validation/canonicalisation code is guard code — its whole value is in refusing
bad input — so per CLAUDE.md each is mutation-tested: break it, confirm a test
catches it, revert.

| Guard | Mutation | Result |
|---|---|---|
| byte-size validation | drop the `Bytes::validate` guard (never error) | `…RejectsUnparseableByteSizes` failed (Failures: 1); reverted |
| extension normalisation | drop `strtolower` (case not canonicalised) | `…StoresAndNormalises…` failed (Failures: 1); reverted |
| extension normalisation | drop the `$ext => $ext` de-dup | `…StoresAndNormalises…` failed (Failures: 1); reverted |
| schema key coverage | remove `excluded_private` from one mapping | `…CoversEveryDefaultConfigurationKey…` failed (Failures: 1); reverted |
| schema type correctness | declare `number_indexed` as `string` | `…CoversEveryDefaultConfigurationKey…` failed (Failures: 1); reverted |
| future-proofing | add an unschemad key to `defaultConfiguration()` | `…CoversEveryDefaultConfigurationKey…` failed (Failures: 1); reverted |

The last row is the load-bearing one for the issue's stated risk ("an unschemad
processor config breaks config export tests"): a future setting added to
`defaultConfiguration()` without a matching schema entry fails this test
immediately, before it can reach config export.

## Testing the schema without a kernel

"Round-trips through export/import" is normally a kernel/functional assertion
(Drupal's `ConfigSchemaValidator` / `SchemaCheckTrait`), but this module ships
only hermetic unit tests (no kernel bootstrap, no DB, no Docker). The
`ProcessorConfigSchemaTest` is a hermetic proxy: it parses the schema YAML and
asserts every `defaultConfiguration()` key is declared with the right type. That
is the precise condition a missing schema entry violates on export, so a green
run is the evidence the config will round-trip.

## Verification

```
$ cd drupal/search_api_wayfinder && vendor/bin/phpunit
Tests: 245, Assertions: 489  (236 baseline + 9 new), OK
```

New tests: `tests/src/Unit/ExtractionSettingsFormTest.php` (6) and
`tests/src/Unit/ProcessorConfigSchemaTest.php` (3). CI gate is the
`search-api-wayfinder-unit` job (`vendor/bin/phpunit`, hermetic).

## Follow-ups

- Wire `ExtractFileValidator` into `FileExtraction`/`LinkedFileExtraction`
  `addFieldValues()`: read the six config keys, apply `isFileIndexable` /
  `limitToAllowedNumber` / `limitBytes`, and add the `file_exists` / `isPermanent`
  preconditions #264 deliberately left out. This form is the stable contract that
  change reads.
