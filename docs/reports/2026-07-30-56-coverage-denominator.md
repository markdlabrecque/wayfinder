# Issue #56 — Search API coverage denominator

## Result

`wayfinder coverage --format json` is a hermetic, deterministic report over
issue #55's frozen `search_api_solr` 4.4.0 capture.

| Bucket | Covered | Total | Fraction |
|---|---:|---:|---:|
| Endpoints | 5 | 9 | 5/9 |
| Request semantics | 27 | 48 | 27/48 |
| Client-consumed response fields | 9 | 15 | 9/15 |
| **Overall** | **41** | **72** | **41/72** |

## Denominator and provenance

`coverage/search_api_coverage_contract.json` is the sole checked-in derived
contract. It contains no coverage classification. It is derived from all 28
files in `solr-ref/search-api/trace/` and
`solr-ref/search-api/manifest.tsv`:

- 28 traces, normalized to 9 method-and-endpoint shapes;
- all 43 distinct captured request parameter names and their decoded
  value/trace occurrence provenance;
- 48 material request/body semantic variants; and
- 15 client-consumed response fields, each with a frozen trace plus the
  `search_api_solr` 4.4.0 PHP source and method that consumes it.

The integration guard independently reparses every frozen URL and JSON body,
compares the manifest, checks the complete parameter occurrence map, requires
every parameter to belong to a semantic item, and checks that each response
field exists in a cited response. Endpoint provenance is exact: each endpoint
item cites every frozen exchange with its method and normalized shape.

Volatile/emitted-only fields such as `responseHeader.status`, `response.start`,
`response.maxScore`, and `response.numFoundExact` are intentionally excluded:
the captured Search API paths have no direct client consumer for them.

## Numerator derivation

The report never reads a contract `covered` value; serde rejects one.

- Endpoints use the single `search_api_routes!` table that builds the Axum
  router, including the method rule shared with `/update` validation.
- Request semantics require both the real strict-parameter allowlist
  (`SELECT_PARAMS`, `UPDATE_PARAMS`, or `MLT_PARAMS`) and a concrete semantic
  implementation classification. The strict-router guard exercises accepted
  parameters, an unsupported parameter, the unrouted `/terms` endpoint, MLT
  response rendering, and the duplicate-key update limitation.
- Response fields use `RenderedResponseField` keys at their actual response
  insertion sites. The report recognizes only those fields; an endpoint,
  parameter name, comment, or dead renderer cannot make a response field
  covered.

The CLI uses only `include_str!` data and compiled code. It has no network,
Docker, schema, index, or environment dependency, and its report ordering and
uncovered lists are stable.

## Uncovered backlog

### Endpoints

- `GET /solr/{core}/admin/luke`
- `GET /solr/{core}/admin/mbeans`
- `GET /solr/{core}/schema/fieldtypes`
- `GET /solr/{core}/terms`

### Request semantics

- `admin.mbeans.stats`
- `mlt.filters`
- `mlt.fl.wildcard-plus-score`
- `mlt.match-include-and-offset`
- `mlt.maxntp`
- `request.json-nl.repeated-map-and-flat`
- `request.omitHeader`
- `request.timezone.utc`
- `select.facet.local-key`
- `select.facet.per-field-missing`
- `select.fl.wildcard-plus-score`
- `select.highlight.merge-contiguous`
- `select.highlight.require-field-match`
- `select.highlight.wildcard-fields`
- `select.q.local-params-edismax`
- `select.spellcheck.collate`
- `select.spellcheck.dictionaries`
- `select.spellcheck.enable`
- `select.spellcheck.query`
- `terms.enumeration`
- `update.json-command-add-batch`

### Response fields

- `admin.luke.index`
- `admin.mbeans.solr-mbeans`
- `schema.fieldtypes.fieldTypes`
- `select.spellcheck.collations`
- `select.spellcheck.suggestions`
- `terms.terms`

## V2 implications

The captured client needs `/terms`, `/schema/fieldtypes`, `/admin/luke`, and
`/admin/mbeans`, plus spellcheck, before the corresponding Search API paths
can be complete. Local-param edismax, wildcard projection/highlighting,
per-field facet overrides, MLT filters/limits, request-header controls, and
lossless duplicate update commands remain explicit feature work rather than
being counted because a route or parameter name exists.

## Verification

Implemented and locally audited on branch `56-coverage-denominator`.

- `cargo test --test search_api_coverage_endpoint_provenance` — pass (1 test).
- `cargo test --test search_api_coverage` — pass (4 tests).
- `cargo fmt --check` — pass.
- `cargo clippy --all-targets -- -D warnings` — pass (no warnings).
- `cargo test` — pass (496 tests, 0 failed across unit, integration, and doc-test targets).

Review verdict: implementation self-audit passed; independent pipeline review
is pending. No Drupal or CI files were changed.
