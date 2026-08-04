# 2026-08-04 — Vendor the full `search_api_solr` 4.4.0 source (#368)

Issue: **#368**. Spec: `docs/specs/PREP-1-vendor-source.md`. PR: closes #368.

Blocked the 2026-08-04 source-sweep batch (#350–#355, #357–#362): every issue in
it cites line numbers in files that were not in the tree (`src/Hook/...`,
`src/Solarium/Autocomplete/Query.php`, `modules/**`, `config/install/**`).

## What changed

- Vendored the full `search_api_solr` 4.4.0 module tree under
  `coverage/search_api_solr_4.4.0_source/` (407 files), preserving the module's
  own layout so the three previously-vendored files keep their paths and bytes.
- Added `coverage/search_api_solr_4.4.0_source_provenance.md` recording the
  source-of-record, archive checksum, retrieval date, and what was excluded.
- Extended `coverage/search_api_solr_4.4.0_source_evidence.json`'s `files[]`
  manifest to hash-pin all 407 files (it previously pinned only the three
  citation-bearing files). `upstream`, `citations`, and `exclusions` are
  unchanged.
- Relaxed two assertions in
  `tests/search_api_coverage.rs::client_consumption_snapshot_is_hash_pinned_complete_and_auditable`
  so the full tree can live in the snapshot dir:
  - `evidence.files == exactly these 3` → the three citation-bearing files are
    still pinned to their exact 4.4.0 hashes (now a subset check); the rest are
    tamper-evident via the existing `source_file_paths == expected_files`
    invariant, which now spans the whole tree.
  - `assert!(file.path.starts_with("src/"))` per file → removed; the snapshot is
    the full module tree, and the normal-path-components check already guards
    against escaping the snapshot root.

No Rust behaviour changed; the server binary is untouched. The test's *result*
is unchanged (green → green) — only its pinned expectations grew with the
snapshot. This is the one place PREP-1's "a changed test result means something
is wrong" needed a correct, audit-strengthening accommodation: the snapshot dir
grew (the point of the issue), so the manifest that pins it grew with it.

## Source of record

`https://git.drupalcode.org/project/search_api_solr`, git tag `4.4.0`, archive
sha256 `5cfcb17d7a325a01eb04f09ca12b6f0d3012ebe0fcfea431ee04a592507c0bce` — the
same value already pinned as `upstream.archive_sha256` in the evidence JSON and
asserted by the coverage test. Vendored bytes come from that exact archive.

The drupal.org release tarball
(`https://ftp.drupal.org/files/projects/search_api_solr-4.4.0.tar.gz`,
`cb92be9e…`) packages the identical source; it differs only in Drupal.org's
release packaging (an injected `version: '4.4.0'` block in `*.info.yml` and an
added `LICENSE.txt`).

## Verification (both required by the spec)

1. **Version + byte-identity.** The three already-vendored files are
   byte-identical to this download, with unchanged sha256 (pinned in the
   evidence JSON). Version 4.4.0 is confirmed by the git tag name, the injected
   `version:` line in the release tarball's `info.yml`, and the three matching
   hashes. Existing line-number citations resolve unchanged
   (`SearchApiSolrBackend.php:2726` spot-checked).

2. **`SearchApiSolrHooks.php` cron path for #352.** `SearchApiSolrHooks::cron()`
   contains exactly what #352 describes:
   - L143–147 gate — `$is_drupal_only_writeable[$server->id()]` (L144),
     `last_build < $last_update_on_server` (L145),
     `last_build < ($request_time - 1800)` (L147).
   - L158–160 — `$connector->getSuggesterQuery()`,
     `$query->addParam('suggest.buildAll', TRUE)`,
     `$connector->fireAndForget($query)`.

   `getSuggesterQuery()` is therefore *called* at L158 — overturning finding 154
   / the #291 report's "defined but never called anywhere in the backend", which
   was true of the three-file snapshot and false of the full module. Correcting
   finding 154 is #352's job; this PR supplies the evidence.

## Exclusions

- `tests/` — named by the spec; the module's own test suite; uncited.
- `jump-start/` — 22 MB of per-Solr-version (6–10) pre-built example config-sets
  dominated by duplicated multi-MB dictionary files; uncited. The module's
  config-generation templates that a finding *would* cite live in
  `solr-conf-templates/` (retained). Re-add from the pinned archive if a future
  finding needs it.
- `logo.png`, `docs/multisite.png` — the only binary files; the evidence system
  hash-pins files as UTF-8 text, and no finding cites them.
- `LICENSE.txt` is absent from the git-tag archive (the release tarball adds it).

## Gates

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` all clean. `tests/finding_citations.rs` green.
