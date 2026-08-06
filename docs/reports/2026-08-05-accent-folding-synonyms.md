# #389 — accent folding, analyzer split, UAX #29, delimiters, and synonyms

**Status:** implemented and independently approved.

## Scope and premise

`SPEC-389.md` changed the binding premise during implementation: Solr compatibility no longer controls this work. The committed `an389_*` captures and findings 195–204 remain reference material, while the delivered behavior prioritizes search quality.

## Delivered behavior

1. **Analyzer split:** index and query analyzers are registered independently. Query paths use query analyzers and retain an index-analyzer fallback.
2. **Unicode accent folding:** index, query, dynamic-text, and suggest chains use NFKD decomposition, combining-mark removal, and explicit expansions for letters that do not decompose. Analyzer migrations fail closed when reindexing is required.
3. **UAX #29 tokenization:** text and suggest chains preserve meaningful one-character and CJK terms with original offsets.
4. **Word delimiters and synonyms:** symmetric delimiter graphs expose parts and catenations without joining whitespace-separated words. Graph positions preserve stopword gaps, symbols remain safe/searchable, and mid-delimiter autocomplete remains useful.

Synonyms are configurable rather than bundled from Solr's large table. Each core exposes a dedicated `GET`/`POST /ui/synonyms` page with client-side filtering and add/edit/delete controls. Valid symmetric groups are stored in `data/synonyms.txt`, replaced atomically, and hot-reloaded for subsequent queries without restart or reindex. Synonyms remain query-side only and never enter index terms. The editor rejects cross-origin browser submissions and invalid or multi-position analyzer members.

## Compatibility and migrations

The authorized cleanup changed only expectations directly contradicted by the approved features. Current `main` retired the differential manifest and harness before integration, so those upstream deletions remain intact; existing fixture JSON and findings remain as references. Phase 4 advances the indexed analyzer contract to v6; affected older indexes require reindexing. Synonym-only edits do not.

## Evidence

Red-first and implementation commits include:

- `39c92c0` — query analyzer reachability
- `7d2a3b8`, `c329f72`, `c677782`, `b80540c` — accent folding and migration coverage
- `89e1042`, `66d9391`, `6f06d99`, `cc55dbe` — UAX #29 and migration hardening
- `eaf8646`, `c1096db`, `c0b2d28`, `303b1c7`, `4626825`, `5397430`, `4224685` — delimiter graphs, configurable synonyms, UI, and review fixes

Accent folding was mutation-tested independently for combining-mark stripping and explicit expansion behavior. Phase 3, Phase 4, and the final current-main integration received independent review; post-integration findings were repaired and the final verdict was **approved with no remaining findings**.

Final gate:

```text
cargo fmt --check                         PASS
cargo clippy --all-targets -- -D warnings PASS
cargo test                               PASS
```

The suite's pre-existing issue #362 measurement test remains ignored; no #389 test was skipped.

## Deliberate limits

- Synonym groups are symmetric and must canonicalize to one analyzed position.
- Multi-token and directional synonym mappings are deferred until explicit graph semantics are designed.
- Reframing the repository-wide Solr compatibility contract and differential harness belongs to the separate in-flight ticket noted by `SPEC-389.md`.
