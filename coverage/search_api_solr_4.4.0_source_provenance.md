# Provenance — `coverage/search_api_solr_4.4.0_source/`

This records where the vendored Search API Solr source came from so an auditor
can confirm they are reading the same bytes that a finding was derived from. It
is metadata about the snapshot, not part of the upstream module.

## Source

| field | value |
|---|---|
| module | Search API Solr |
| version | 4.4.0 |
| upstream project | https://git.drupalcode.org/project/search_api_solr |
| source-of-record | git tag `4.4.0` of the above project |
| archive url | https://git.drupalcode.org/project/search_api_solr/-/archive/4.4.0/search_api_solr-4.4.0.tar.gz |
| archive sha256 | `5cfcb17d7a325a01eb04f09ca12b6f0d3012ebe0fcfea431ee04a592507c0bce` |
| retrieved | 2026-08-04 |

The `archive_sha256` above is the same value pinned as
`upstream.archive_sha256` in `search_api_solr_4.4.0_source_evidence.json`, and
is asserted verbatim by
`tests/search_api_source_evidence.rs::source_evidence_is_hash_pinned_complete_and_auditable`.
The vendored bytes are extracted from that exact archive.

### Equivalent canonical release

The drupal.org release tarball packages the same release:

| field | value |
|---|---|
| release url | https://ftp.drupal.org/files/projects/search_api_solr-4.4.0.tar.gz |
| release sha256 | `cb92be9e8d2cb7a1107444cd9b3629dca093bf82fd659ed0541a1d0fc447c7ae` |

The git-tag archive and the release tarball hold byte-identical source; they
differ only in Drupal.org's release packaging, which (a) injects a `version:
'4.4.0'` block into every `*.info.yml` (the raw git tree has no such line) and
(b) adds `LICENSE.txt`. Version 4.4.0 is therefore confirmed three ways: the git
tag name, the injected `version:` line in the release tarball's
`search_api_solr.info.yml`, and the three pre-existing files being byte-identical
to this download (their hashes are pinned in the evidence JSON).

## What is and is not vendored

The snapshot holds the full module tree as it ships, minus three categories.

Included (407 files): `src/**`, `modules/**` (all five submodules), `config/**`
(`install/`, `optional/`, `schema/`), `solr-conf-templates/**`, the module's own
`docs/**`, and every top-level file (`*.install`, `*.module`, `*.routing.yml`,
`composer.json`, `README.md`, `.gitlab-ci*`, etc.).

Excluded:

| path | reason |
|---|---|
| `tests/` | Named by the spec; the module's own test suite. Not cited by any finding and would inflate the tree. |
| `jump-start/` | 22 MB of per-Solr-version (6–10) pre-built example config-sets whose bulk is duplicated multi-MB dictionary files (e.g. `nouns_nl.txt`). Nothing cites it; the module's config-generation templates that a finding *would* cite live in `solr-conf-templates/`, which is retained. Re-add from the pinned archive if a future finding needs it. |
| `logo.png`, `docs/multisite.png` | Binary images. The evidence system hash-pins files as UTF-8 text (`read_to_string`); these are the only non-text files in the tree and no finding cites them. |

`LICENSE.txt` is simply absent from the git-tag archive (the release tarball
adds it); it is the GPL text and is not cited source.

## Verification (issue #368)

1. The three files already vendored before this change are byte-identical to the
   same files in this download, with unchanged sha256 (pinned in the evidence
   JSON). Existing citations resolve to the same content at the same line
   numbers.
2. `src/Hook/SearchApiSolrHooks.php` contains the cron path issue #352 describes
   — `SearchApiSolrHooks::cron()`:
   - L143–147: the gate — `$is_drupal_only_writeable[$server->id()]` (L144),
     `last_build < $last_update_on_server` (L145), and
     `last_build < ($request_time - 1800)` (L147).
   - L158–160: `$connector->getSuggesterQuery()`,
     `$query->addParam('suggest.buildAll', TRUE)`,
     `$connector->fireAndForget($query)`.

   `getSuggesterQuery()` is therefore *called* (L158), overturning the #291
   report's claim that it is "defined but never called anywhere in the backend"
   — true of the old three-file snapshot, false of the full module.
