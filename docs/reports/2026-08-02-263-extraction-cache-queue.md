# Issue #263 — extraction cache and queue worker

Date: 2026-08-02
Branch: `markdlabrecque/issue-263-extraction-cache-queue`
Builds on: `docs/reports/2026-08-04-262-file-extraction-tracer.md` (#262, the
file-extraction tracer this caches and queues) and #264 (the indexability
rules whose `ExtractFileValidator` stays untouched here).
Part of epic #268 (document text extraction, server and Drupal).

## What shipped

The second slice of the `search_api_attachments` port into
`search_api_wayfinder`: extracted text is cached, extraction can run on cron
instead of inline, and a changed or deleted file reindexes every item that
referenced it. Without this, every reindex re-uploads and re-extracts every
file — unusable on a site with thousands of PDFs. The tracer (#262) made one
file extract end-to-end; this slice makes the *Nth* reindex cheap and correct.

- **`ExtractionCacheInterface` + `KeyValueExtractionCache`**
  (`src/Cache/`): one cache backend, keyvalue-backed, keyed by file **content
  hash** (`sha256:` prefix). Keying by content hash — not file id — is the
  load-bearing decision: identical content (the same file referenced by many
  items, or a second file with the same bytes) shares one entry, and a changed
  file (new hash) naturally misses, so "changed file re-extracts" falls out of
  the keying with no explicit eviction. Derived in shape from
  `search_api_attachments`' `AttachmentsCacheInterface`/`KeyValue`, GPL-2.0-or-
  later (same licence); the keying is content-hash here where upstream is
  file-id-based with explicit eviction. A second backend is **not** shipped —
  the issue's "ship one unless a second is demonstrably needed."
- **`FileReferenceMapInterface` + `FileReferenceMap`** (`src/`): the explicit
  file→item mapping populated by the processor during indexing and consulted by
  the invalidator. Kept explicit (rather than derived from the entity reference
  on demand) because #265 (linked-file discovery) indexes files referenced
  only by a URL in content — no reference field to query — and reuses this map.
  Item ids are stored in Search API's combined form (`datasource/raw`); the
  invalidator splits them for `trackItemsUpdated()`.
- **`ExtractionInvalidator`** (`src/`): on file update/delete, reads the map,
  loads the referencing indexes, groups items per (index, datasource), and
  calls `IndexInterface::trackItemsUpdated()`. All logic lives here; the
  `.module` hooks are two-line forwarders so this stays unit-testable.
- **`ExtractorQueue`** queue worker (`src/Plugin/QueueWorker/`): on cron,
  loads the file, ensures the extraction is cached (extracting on a miss), and
  marks the referencing item for reindex so the next index pass hits the cache
  instead of re-queuing. Id `wayfinder_extraction`, `#[QueueWorker]`
  attribute, `ContainerFactoryPluginInterface` for DI. Derived in shape from
  `search_api_attachments`' `ExtractorQueue`; transport is the Wayfinder
  backend rather than an external text-extractor plugin.
- **`FileExtraction` processor** (#262): rewired its per-file loop to
  `extractOrGetFromCache()` (cache probe → queue on miss → extract+cache), and
  to populate the file→item map. Gained `extraction_mode` config (`inline`
  default | `queue`). The tracer's inline behaviour is the default; queue mode
  is opted into via config the admin form in #266 will surface.
- **`search_api_wayfinder.module`** + **`search_api_wayfinder.services.yml`**:
  the file lifecycle hooks (`hook_file_update`/`_delete` → invalidator) and the
  DI wiring for the cache, map, invalidator, and two private keyvalue
  collections. The queue worker is a plugin auto-discovered by its attribute;
  the queue is created on demand by core's queue factory.

## Acceptance

Both required tests, red-first, now green:

- **A second extraction of the same file hits the cache and does not call the
  client.** `FileExtractionTest::testASecondExtractionOfTheSameFileHitsTheCacheAndSkipsTheClient`
  indexes two items referencing the same file; `extractContentFromFile` runs
  exactly **once** and both items receive the text.
- **A changed file invalidates and re-extracts.**
  `FileExtractionTest::testAChangedFileIsReExtractedAfterItsContentChanges`
  rewrites the file's bytes between two index passes; the new hash misses and
  the client is called again, yielding the new text.

`vendor/bin/phpunit` green: **208 tests, 358 assertions** (175 baseline +
33 new), exit 0. The PHPUnit deprecation count rose with the test count at the
suite's existing per-test rate; CI runs `vendor/bin/phpunit` (no
`--fail-on-deprecation`), so it passes as before.

## Mutation tests (guard code)

Per the repo rule that code whose whole value is *failing correctly* gets
mutation-tested, each load-bearing guard was broken deliberately, confirmed to
be caught, and reverted:

- **Cache content-hash keying** → `file:<id>` keying: caught by
  `testIdenticalContentHitsAcrossDifferentFileObjects`,
  `testChangedContentMissesBecauseTheHashDiffers`,
  `testKeyIsSha256OfTheFileContents`.
- **Map reference dedup** → dedup removed: caught by
  `testRecordingTheSameReferenceTwiceDeduplicates`.
- **Invalidator skip-missing-index guard** → guard inverted: caught by
  `testOnFileUpdateSkipsAndLogsAMissingIndexButContinues` (and others).
- **Queue worker failure re-throw** → exception swallowed: caught by
  `testProcessItemLogsAndRethrowsOnExtractionFailure`.

## Design decisions (hard to change later)

1. **One cache backend, content-hash keyed.** `keyvalue` is always available
   on a Drupal site; a file-based backend needs a path and offers no
   demonstrated benefit, so it is not shipped. Content-hash keying subsumes
   invalidation at the cache layer; the stale entry under an old hash is simply
   never read again. Cost (ponytail): the file is read once to hash on every
   probe, even on a hit — the same trade-off `search_api_attachments` makes,
   and a net win because a hit skips the upload and the much slower server-side
   parse.
2. **Failure semantics split by mode.** Inline (#262's contract): extraction
   failure is logged and skipped so one bad attachment never fails an index
   batch. Queue: failure is logged and **re-thrown** so cron leaves the item
   for retry — there is no batch to protect on cron, so retrying is the
   queue-appropriate answer to a transient server failure. A permanently-
   unsupported file retried indefinitely is a known limitation shared with
   `search_api_attachments`, addressed when a real site hits it.
3. **Invalidation runs on every file update**, including metadata-only saves.
   Because the cache is content-hash keyed, an unchanged-content save still
   reindexes but the re-extraction is a cache hit — cheap. A narrower
   "content-actually-changed" guard (comparing old and new hash) is a #266
   refinement, not a correctness fix, and is left out deliberately to keep
   this slice minimal.
4. **The file→item map is explicit and idempotent.** Recording the same
   (index, item, file) twice stores one entry, so reindexing an unchanged item
   does not grow the map. `forgetFile()` on delete stops dead entries leaking.

## Trust boundaries

This is Drupal-side index-time behaviour. It does **not** substitute for the
server-side extraction budgets in #257, which protect Wayfinder from any
client and stay fail-closed regardless of what Drupal does. The cache stops
Drupal re-uploading files it has already parsed; #257 bounds what happens when
it does upload. Weakening one because the other exists is a bug — same stance
as #264's indexability rules.

## Out of scope / follow-ups

- **#265 (linked files)** reuses `FileReferenceMapInterface` directly: it
  records URL-discovered references with no entity-reference field to derive
  them from. The interface and the invalidator are shaped for that already.
- **#266 (settings form / config schema)** will surface `extraction_mode` and
  optionally a content-change guard for invalidation. No admin UI is built
  here; the config key has a tested default.
- **Integration coverage** is a manual follow-up (`tests/integration/run.sh`,
  `WAYFINDER_INTEGRATION=1`) — a live Wayfinder + Drupal proving the cache hit
  across a real reindex. The unit tests are the acceptance gate per the issue
  ("`vendor/bin/phpunit` green"), matching the tracer's stance.
- The `ExtractionCacheInterface` allows a second backend later; one ships now.

## Commands / evidence

```sh
cd drupal/search_api_wayfinder
composer install --no-interaction --no-progress
vendor/bin/phpunit         # 208 tests, 358 assertions, exit 0
```

No Rust files were touched; the `search-api-wayfinder-unit` CI job is the only
gate this PR exercises.
