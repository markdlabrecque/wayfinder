# Issue #55 — capture the search_api_solr contract (v1.5)

## What was captured

A stock, unmodified Drupal site (Search API + search_api_solr, no
`search_api_wayfinder` connector in the loop) was stood up in Docker against a
real `solr:9`, with an mitmproxy reverse proxy sitting between the module and
Solr so every request/response pair the module actually sends could be
recorded verbatim, then frozen as fixtures.

### Version pins

| Component | Version |
|---|---|
| Drupal core | 11.3.2 |
| `drupal/search_api` | 1.41.0 |
| `drupal/search_api_solr` | 4.4.0 |
| `drush` | 13.7.6 |
| Solr | `solr:9` image, resolved at capture time to Solr **9.10.1** (Lucene 9.12.3) |

The Solr version pin doubles as the answer to PRD open question 2 (finding 78):
`search_api_solr` reads `lucene.solr-spec-version` off `admin/system` and
reports the full `major.minor.patch`, not a bare major version.

### Environment

- Docker Compose stack, deliberately separate container names/ports from the
  canonical `wayfinder-solr-ref` stack: `wf55-solr` (host port 8996),
  `wf55-mitm` (host port 8997, mitmproxy reverse-proxying to `wf55-solr`),
  `wf55-drupal` (host port 8998). All torn down after capture; nothing left
  running.
- Drupal site: SQLite-backed (no DB container needed), `standard` install
  profile, two content types used (`article`, `page`) with a representative
  field mix — fulltext (`body`), string (`field_sku`, `field_topics`),
  integer (`field_rating`, `field_priority`), boolean (`field_featured`,
  `field_archived`), date (`field_event_date`, `field_published_on`), and
  multi-value string (`field_keywords`, `field_topics`).
- Search API server pointed at the mitm proxy (not Solr directly) — that
  indirection is what makes the trace real traffic rather than synthesized
  requests.
- Config set generated via `drush search-api-solr:get-server-config`
  (the module's own export, unmodified) and deployed onto the Solr core to
  make the corpus actually indexable/searchable end to end.
- 6-document corpus indexed across the two bundles; searches driven through
  the module's own PHP query API (`\Drupal\search_api\Entity\Index::query()`,
  parse modes, facet/spellcheck/MLT/terms options) plus a few direct
  connector calls for the admin/handshake endpoints — not the Wayfinder code,
  not hand-crafted Solr requests.

## Fixture layout

```
solr-ref/search-api/
  configset/       # the module's generated schema.xml, solrconfig.xml, and
                    the rest of the export, frozen verbatim (Wayfinder never
                    parses these — reference material only, per PRD §3)
  trace/           # 28 request/response JSON pairs, one per HTTP exchange,
                    numbered by capture sequence
  manifest.tsv     # file / seq / method / endpoint / status / q-prefix for
                    each trace file
  capture.sh       # rerun script: brings the whole stack up, rebuilds the
                    site from the pinned versions above, redrives the same
                    capture, tears itself down on exit
  build/           # docker-compose.yml, the mitmproxy capture addon, and the
                    PHP scripts capture.sh drives (drupal-site/ and
                    mitm-captures/ under build/ are gitignored — ephemeral)
```

Existing `solr-ref/responses/`, `solr-ref/manifest.tsv`, and
`solr-ref/manifest-errors.tsv` were not touched.

## The two required findings

Appended to `docs/solr-ref-findings.md` as entries 76-78 (next available
number at time of writing):

- **76** — which endpoints/params the module actually used: `update` (batched
  JSON, `commitWithin` not explicit `commit`), `select` (carries all
  fulltext/filter/facet/sort/spellcheck/highlight traffic — no dedicated
  facet/spellcheck handler), `mlt`, `terms`, `schema/fieldtypes`, and the
  admin/handshake set (`admin/info/system`, `<core>/admin/system`,
  `<core>/admin/luke`, `<core>/admin/mbeans`). No SolrCloud/collections calls
  observed, consistent with the PRD non-goal.
- **77** — how the module drives edismax (qf/pf/mm), for comparison against
  issue #7/PR #53: the module's multi-term AND-conjunction edismax queries
  wrap the `{!edismax qf=...}` local-params clause in an outer `(...)` group,
  and a genuine Solr parsing quirk means only the *first* token after `}` is
  actually handed to edismax — subsequent `+"term"` clauses fall through to
  the outer default query parser's default field and typically fail to
  match. Confirmed with isolated curl probes (`trace/00006.json` single-term
  vs `trace/00008.json` two-term, same fq, same corpus). Flagged as real
  captured Solr behaviour to match faithfully, not a bug to paper over.
- **78** — the PRD open-question-2 finding on Solr version reporting (see
  table above).

## Acceptance criteria status

- [x] Generated config set frozen in-repo with recorded version pins
- [x] HTTP trace (28 request+response pairs) frozen with manifest + rerun
      script
- [x] Trace covers indexing, the module's search feature surface (edismax
      fulltext AND/OR, direct parse mode, sorts on string/integer/date,
      filters on string/range/boolean/multi-value, facets, spellcheck, MLT,
      terms), and admin/handshake calls
- [x] Findings entries for endpoints/params (76) and edismax qf/pf/mm (77)
- [x] Existing fixtures untouched (`solr-ref/responses/`, `solr-ref/manifest.tsv`,
      `solr-ref/manifest-errors.tsv` unmodified); `cargo test` unaffected —
      this work touched no Rust source, only `solr-ref/search-api/` and
      `docs/`
- [x] All capture-only Docker containers torn down

## Notes / judgment calls for reviewers

- The connector's jump-start default config (`path: /`) needed to point at
  the mitm proxy's root rather than `/solr` — `capture.sh` documents and
  applies this fix inline; it is an artifact of proxying, not a module
  finding.
- `admin/ping` calls happened during interactive setup/debugging before the
  trace counter was reset for the frozen run, so they exist in this capture's
  history but are not in the committed `trace/` (which starts clean at the
  indexing call). `admin/ping` is still exercised — just not double-counted
  in the frozen numbering; re-running `capture.sh` would recapture it
  identically since it re-triggers `pingCore()` before the reset point too,
  if a fixture is wanted, that line can be moved after the reset.
- `search_api_autocomplete` (a separate contrib module) was intentionally
  **not** installed — the issue's "autocomplete/spellcheck if the module
  emits them" is satisfied by `search_api_solr`'s own native `terms` and
  `search_api_spellcheck` option support, which needs no extra module.
