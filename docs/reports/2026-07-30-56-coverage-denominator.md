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
every parameter to belong to a semantic item, requires every cited semantic
trace to contain that variant's captured value, and checks that each response
field exists in a cited response. `json.nl=flat` now carries all 28 flat
occurrences, including MLT `00022.json`. Endpoint provenance is exact: each
endpoint item cites every frozen exchange with its method and normalized shape.

Volatile/emitted-only fields such as `responseHeader.status`, `response.start`,
`response.maxScore`, and `response.numFoundExact` are intentionally excluded:
the captured Search API paths have no direct client consumer for them.

## Numerator derivation

The report never reads a contract `covered` value; serde rejects one.

- Endpoints use the single `search_api_routes!` table that builds the Axum
  router, including the method rule shared with `/update` validation.
- Request semantics and response fields run compact probes through a real,
  strict-parameter in-process router and inspect its rendered JSON. There is
  no Boolean support map or rendered-field membership list: removing a
  handler behavior, strict parameter, or response key changes the numerator.
  The probe creates and removes its own fixed schema, index, and corpus under
  the system temporary directory; it does not use a network, Docker, user
  schema, index, or environment configuration.
- `json.nl=flat` is uncovered: the probe evaluates the update, select, MLT,
  and admin paths represented by its complete provenance, and strict MLT (as
  well as update) rejects it. `UPDATE_PARAMS` remains unchanged.

The CLI's fixed in-process probe and contract ordering make its report and
uncovered lists deterministic.

The recomputed fraction remains **41/72**, but its evidence is corrected:
`request.json-nl.flat` is now uncovered and live `select.fl.wildcard-plus-score`
is covered because the probe observes the rendered `score` field.

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
- `request.json-nl.flat`
- `request.json-nl.repeated-map-and-flat`
- `request.omitHeader`
- `request.timezone.utc`
- `select.facet.local-key`
- `select.facet.per-field-missing`
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
- `cargo test --test search_api_coverage` — pass (5 tests).
- `cargo fmt --check` — pass.
- `cargo clippy --all-targets -- -D warnings` — pass (no warnings).
- `cargo test` — pass (497 tests, 0 failed across unit, integration, and doc-test targets).

### Mutation evidence

Temporarily removed `"hl"` from the live `SELECT_PARAMS` allowlist, then ran:

```sh
cargo test --test search_api_coverage \
  coverage_command_requires_complete_deterministic_contract_schema_and_output
```

The command exited `101`: all highlighting semantic items and the
`select.highlighting` response field became uncovered, changing the request
subtotal from `27/48` to `23/48`, the response subtotal from `9/15` to `8/15`,
and the overall fraction from `41/72` to `36/72`. The unchanged expected
coverage test failed on the request-semantic subtotal. The exact live source
was restored before the final gates.

Review round 1 remediation: complete. The reviewer's follow-up to pin each
client-source citation against a vendored Search API source snapshot is
recorded as non-blocking follow-up work; this issue retains auditable
path+symbol citations and does not modify `drupal/`.

Review verdict: implementation self-audit passed; independent pipeline review
is pending. No Drupal or CI files were changed.
