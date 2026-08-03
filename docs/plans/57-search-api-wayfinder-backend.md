# #57 (re-scoped): search_api_wayfinder — standalone Search API backend

**Date:** 2026-07-29. **Supersedes** the 2026-07-29 decision record in the original
issue body (subclass `StandardSolrConnector`). The escape hatch priced there is now
the plan: a custom Search API `BackendInterface` implementation with **no dependency
on `search_api_solr` and no Solarium**. Copying code *from* `search_api_solr` at the
method level is allowed where it saves time (both modules are GPL-2.0-or-later);
depending on it is not.

This document is the handoff spec for the implementing agent. Read the whole thing
before writing code.

## Goal

A Drupal module `search_api_wayfinder` providing a `@SearchApiBackend` plugin
("Wayfinder") that talks Solr wire format directly to a running Wayfinder server
over Guzzle (Drupal's `http_client` service). Sites configure a Search API server
with this backend, index content into Wayfinder, and run fulltext searches with
filters, sorts, and facets — with zero `search_api_solr` code installed.

**Scope ceiling: only what Wayfinder already implements.** The server's whole
surface is:

| Endpoint | Params (from `SELECT_PARAMS` etc. in `src/lib.rs`) |
|---|---|
| `POST /solr/{core}/update` | JSON add / delete-by-id / delete-by-query / commit; `commit`, `commitWithin`, `overwrite`, `softCommit`, `wt` |
| `GET /solr/{core}/select` | `q`, `df`, `fq`, `defType=edismax`, `qf`, `pf`, `mm`, `tie`, `boost`, `bq`, `fl`, `rows`, `start`, `sort`, `facet`, `facet.field`, `facet.query`, `facet.limit`, `facet.mincount`, `facet.sort`, `facet.missing`, `facet.range`(+start/end/gap), `stats`, `stats.field`, `hl`, `hl.fl`, `hl.snippets`, `hl.fragsize`, `hl.simple.pre/post`, `hl.method`, `json.nl`, `wt` |
| `GET /solr/{core}/mlt` | `q`, `df`, `fl`, `rows`, `start`, `mlt.*` |
| `GET /solr/{core}/admin/ping` | `wt` |
| `GET /solr/admin/info/system`, `GET /solr/{core}/admin/system` | `wt`, `json.nl` |

Anything not in that table (spellcheck, autocomplete, grouping, spatial, cursors,
atomic updates, `{!tag}`/`{!ex}` local params, terms component) is **out of scope**
and must not appear in the module — no stub methods "for later". New Wayfinder
features get added to the module when they land in the server, not before.

**`strict_params = true` is the tripwire:** Wayfinder 400s any param it does not
support, so if the module emits an unsupported param the integration harness fails
loudly. Do not work around such a 400 by widening anything server-side — it means
the module is emitting out-of-scope traffic.

## What exists already

- **Worktree** `/Users/mark/Projects/wayfinder-57-search-api-wayfinder/` (branch of
  this repo), uncommitted. Decision: **keep the integration harness, drop the
  connector.** Reusable: `drupal/search_api_wayfinder/tests/integration/`
  (docker-compose, Drupal site, `setup_server_index.php`, `create_content.php`,
  `run_queries.php`, `run.sh`, `schema.toml`), `composer.json` scaffolding,
  `.github/workflows/ci.yml` changes. Discard: `src/Plugin/SolrConnector/`,
  `tests/src/Unit/WayfinderConnectorTest.php`, the `search_api_solr` composer
  requirement, the schema.yml keyed to the connector.
- **`presets/search-api.toml`** — a Wayfinder core preset already shaped for Drupal
  traffic: it defines the `search_api_solr` dynamic-field prefixes (`ss_*`, `sm_*`,
  `tm_*`, `ts_*`, `its_*`, `itm_*`, `ds_*`, `dm_*`, `fts_*`, …). Read its comments:
  booleans (`bs_*`/`bm_*`) map to Wayfinder `string` (values are the literal strings
  `true`/`false`), `sort_*` maps to `string` (no collation).
- **Fixtures** in `solr-ref/responses/` are ground truth for what Wayfinder's
  responses look like; `docs/solr-ref-findings.md` records learned Solr facts.

## Locked decisions

1. **Keep `search_api_solr`'s dynamic-field naming convention.** The
   `presets/search-api.toml` core preset, the captured fixtures, and the
   differential harness are all built around that naming. The module's field mapper
   emits `{type_prefix}{s|m}_{field_name}` exactly as `search_api_solr` does
   (copy its mapping table). Inventing a new naming scheme would orphan the preset
   and the compatibility evidence.
2. **One Wayfinder core per Search API *server*; multiple indexes share it** the
   way `search_api_solr` does: every document carries `index_id` (and
   `ss_search_api_language`, `ss_search_api_datasource`), every query gets
   `fq=index_id:"<id>"`, delete-all is delete-by-query on that filter. Document id
   is `{index_id}-{item_id}` — we deliberately drop `search_api_solr`'s site hash
   (single-site assumption; note it with a `ponytail:` comment naming the upgrade:
   reintroduce a hash component if multi-site-one-core ever matters).
3. **Fulltext queries go through edismax** (`defType=edismax`, `qf` from the
   query's fulltext fields with their boosts, `mm` for AND/OR conjunction), because
   that is what `search_api_solr` sends and what Wayfinder's edismax support
   (issue #7) was built against. Filters go through `fq` in Lucene syntax.
4. **No `search_api_facets_operator_or`.** OR facets require `{!ex}`/`{!tag}` local
   params, which Wayfinder does not implement. Declare plain `search_api_facets`
   only. **Superseded by #298** (wave 4 of the parity batch): the server grew
   `{!ex}`/`{!tag}` in #295, and `WayfinderBackend::getSupportedFeatures()` now
   advertises `search_api_facets_operator_or`; `QueryBuilder::buildFacets()` emits
   `{!ex=facet:<field>}` on OR facets and `build()` tags the matching `fq`. Kept
   as the original planning record.
5. **Module lives in this repo at `drupal/search_api_wayfinder/`.** Composer deps:
   `drupal/search_api` only (plus core). `core_version_requirement: ^10.3 || ^11`.
6. **Highlighting is optional-but-in-scope**: Wayfinder implements `hl`, and the
   backend passes it through when the query asks (populating `highlighted_fields`
   extra data as `search_api_solr` does). Sites can also just use Search API's
   core `highlight` processor, which needs nothing from the backend.
7. **`stats`/`stats.field` stays server-only for now.** Search API core has no
   consumer for it; do not add backend surface nobody calls.

## Architecture

```
drupal/search_api_wayfinder/
├── search_api_wayfinder.info.yml        # deps: search_api:search_api only
├── composer.json                        # require: drupal/search_api
├── config/schema/search_api_wayfinder.schema.yml
├── src/
│   ├── Plugin/search_api/backend/
│   │   └── WayfinderBackend.php         # @SearchApiBackend; thin — delegates
│   ├── WayfinderClient.php              # HTTP: select/update/mlt/ping/system over Guzzle
│   ├── QueryBuilder.php                 # QueryInterface → select/mlt param arrays
│   ├── DocumentBuilder.php              # Search API items → /update JSON commands
│   ├── ResponseParser.php               # response JSON → ResultSet population
│   └── FieldMapper.php                  # SA field/type → dynamic field name (+ value formatting)
└── tests/
    ├── src/Unit/                        # QueryBuilder/DocumentBuilder/ResponseParser/FieldMapper
    └── integration/                     # harness carried over from the worktree
```

Design rule: **all translation logic lives in plain classes that take arrays/value
objects, not Drupal services**, so unit tests run under bare PHPUnit with the
autoload-dev shim the worktree already set up (`Drupal\search_api\` mapped to the
vendor path). `WayfinderBackend` is glue: config form, feature flags, and calls
into the four classes. `WayfinderClient` wraps `http_client` and converts non-200
Solr error envelopes into `SearchApiException` with the envelope's `error.msg`.

### Backend plugin contract (what to implement, nothing more)

- `defaultConfiguration()` / `buildConfigurationForm()` / validate / submit:
  scheme, host, port, path, core, timeout(s). Mirror `search_api_solr`'s connector
  form fields, minus what Wayfinder lacks (no auth v1, no jmx, no solr_version
  override).
- `getSupportedFeatures()`: `['search_api_facets', 'search_api_mlt']`.
- `supportsDataType()`: the default SA types only (`text`, `string`, `integer`,
  `decimal`, `date`, `boolean`); return FALSE for everything else (no location,
  no `solr_*` types).
- `isAvailable()`: `GET admin/ping`, 200 ⇒ TRUE, anything else FALSE (never throw).
- `viewSettings()`: server URL + version string from `{core}/admin/system`.
- `indexItems()` / `deleteItems()` / `deleteAllIndexItems()` / `search()`.
- `removeIndex()` = delete-by-query on `index_id`.
- Explicitly not implemented: autocomplete interfaces, `getBackendDefinedFields`,
  datasource-specific retrieval beyond the `deleteAllIndexItems` datasource filter
  (`fq` on `ss_search_api_datasource`).

### Query translation (the hard 40%)

`QueryBuilder::build(QueryInterface $query): array` (param name ⇒ value(s)):

- **Keys** (`$query->getKeys()`): parsed-keys nested array → edismax `q`. Plain
  conjunction of terms; quoted phrases; `#negation` via `-`; `#conjunction: OR`
  joined with explicit `OR` (copy `search_api_solr`'s flattening approach).
  Keys NULL ⇒ `q=*:*` (match-all, no defType).
- **`qf`**: fulltext fields ∩ index fields, mapped names, `^boost` suffix from
  field boosts.
- **Condition groups** (`$query->getConditionGroup()`): recursive translation to
  one `fq` per top-level member (AND) or a single parenthesised `fq` (OR),
  matching `search_api_solr`'s `createFilterQueries` semantics. Operators to
  support: `=`, `<>`, `<`, `<=`, `>`, `>=`, `BETWEEN`, `NOT BETWEEN`, `IN`,
  `NOT IN`; NULL value + `=`/`<>` ⇒ `-field:[* TO *]` / `field:[* TO *]`.
  Ranges use `[a TO b]`. Copy `search_api_solr`'s value escaping
  (`Utility::escape`-equivalent) verbatim — escaping bugs are the classic
  injection/correctness hole here, and the copied version is battle-tested.
- **Value formatting** by field type: dates ⇒ `1970-01-01T00:00:00Z` (from epoch
  int), booleans ⇒ `"true"`/`"false"` **strings** (Wayfinder's preset maps `bs_*`
  to `string` — this is the one place the mapper must know the preset's
  divergence), text/string quoted-and-escaped, numerics bare.
- **Sorts**: `search_api_relevance` ⇒ `score`, `search_api_id` ⇒ `id`, otherwise
  mapped single-value field name (`sort_*` mapping for text fields, copied from
  search_api_solr); `sort=f1 asc,f2 desc`.
- **Paging**: `start`/`rows` from offset/limit.
- **`fl`**: `id index_id score` and nothing else v1 — Search API loads entities by
  id; returning stored fields is not needed. (`ponytail:` comment; upgrade is
  honouring `search_api_retrieved_field_values`.)
- **Facets** (`$query->getOption('search_api_facets')`): per facet —
  `facet=true`, `facet.field={mapped}`, `facet.limit`, `facet.mincount`,
  `facet.missing`; `facet.sort` from the facet's sort if given. Parse response
  `facet_counts.facet_fields` (send no `json.nl` and parse Solr's default
  flat-array shape, which the fixtures show).
- **MLT** (`$query->getOption('search_api_mlt')`): route to `GET /mlt` with
  `q=id:"{index_id}-{item_id}"` and `mlt.fl` from the option's fields (mapped);
  same `fl`/`rows`/`start` handling.

**Premises to verify before implementing** (per working agreement — ticket text
lies; fixtures don't): (a) the exact `facet_counts` JSON shape Wayfinder emits
without `json.nl` — read a fixture, don't guess; (b) whether Wayfinder's edismax
accepts multi-clause `q` with explicit `OR` — check `edismax.rs` tests or run one
query against the dev server; (c) the `admin/system` response fields available for
`viewSettings()` — read the fixture.

## Indexing translation

`DocumentBuilder`: item ⇒ `{"add": {"doc": {...}}}` with `id`, `index_id`,
`ss_search_api_language`, `ss_search_api_datasource`, then each field via
`FieldMapper` (multi-value fields ⇒ JSON arrays into `*m_*` names). POST body is
Solr's command-object form (Wayfinder parses both bare-array and command form —
use command form, it carries deletes too). Send `commitWithin` from a config
value defaulting to 1000ms; `indexItems` returns the ids it sent on 200.
`deleteItems` ⇒ `{"delete": [ids...]}`; `deleteAllIndexItems` ⇒
`{"delete": {"query": "index_id:\"<id>\""}}` (+ datasource clause when given)
followed by the same commit policy.

## Milestones (tracer-bullet order — each lands green before the next starts)

1. **M1 — vertical slice.** Module skeleton, backend plugin registers and its
   config form saves; `isAvailable` pings; `indexItems` writes one real node;
   `search()` with plain fulltext keys returns it through the integration
   harness. This is real kept code touching every layer.
2. **M2 — filters, sorts, paging.** Full condition-group translation, operators,
   NULL/range/IN, escaping, value formatting, sorts, offset/limit. Unit-test
   heavy: this milestone is mostly pure functions.
3. **M3 — facets.** Feature flag, param generation, response parsing into
   `search_api_facets` extra data; verified in the harness with the `facets`
   contrib module's plain field facet.
4. **M4 — MLT + highlighting.** `/mlt` routing; `hl` pass-through populating
   `highlighted_fields`.
5. **M5 — polish + report.** `viewSettings` version handshake, config schema
   complete, README (install, preset pointer: use `presets/search-api.toml` to
   create the core), CI job, `docs/reports/` entry.

## Testing

- **Unit (hermetic, CI gate):** bare PHPUnit against the four translation classes,
  using the worktree's autoload-dev shim pattern. Expected wire values derived
  from `solr-ref/responses/` fixtures where one exists — never from what the
  builder happens to emit. Escaping and condition-group translation get
  mutation-tested (break the escaper, confirm red, revert).
- **Integration (env-gated, not in default CI):** the carried-over harness —
  docker-compose runs Wayfinder with the `presets/search-api.toml` schema + the
  Drupal site; scripts create content, index, and assert on query results
  end-to-end. Gate behind an env var the same way `WAYFINDER_DIFF_SOLR=1` gates
  the differential harness.
- Tests before implementation, red for the right reason first (repo standard).

## Process

- Branch `57-search-api-backend` off `main` (the old worktree branch keeps its
  harness available for copy; do not build on the connector commits). Claim #57
  (`gh issue edit 57 --add-assignee @me` + comment) before starting.
- Hot files: this work is almost entirely under `drupal/`, so contention with
  Rust-side branches is limited to `.github/workflows/ci.yml` — coordinate that
  file if anything else is in flight.
- One PR per milestone is acceptable; M1 must be its own PR (the tracer bullet
  gets reviewed alone). `Closes #57` goes on the final milestone's PR only.
- Every deliberate descope above (site hash, `fl`, OR-facets, stats) is either a
  `ponytail:` comment or a README "not supported" line — no silent gaps.

## Acceptance (replaces the issue's original list)

- [ ] Module installs on Drupal ^10.3/^11 with only `search_api` as a dependency;
      "Wayfinder" appears as a backend choice; `composer why drupal/search_api_solr`
      inside the harness site proves it absent.
- [ ] Index + fulltext search + filter + sort + facet round-trip green in the
      integration harness against a real Wayfinder.
- [ ] MLT and highlighting round-trips green.
- [ ] Unit suite green and hermetic; escaper and condition translation
      mutation-tested.
- [ ] No request the module emits draws a `strict_params` 400 from Wayfinder.
- [ ] `docs/reports/` entry, including a list of every method-level copy taken
      from `search_api_solr` and why.
