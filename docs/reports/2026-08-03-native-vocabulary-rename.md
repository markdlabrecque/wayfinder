# Issue #325 — retire the literal token "solr" from Wayfinder's own API surface

Branch `325-native-vocabulary`, three commits off `main`: `a005069`, `cdac2f0`, `357df14`.

## What was built

Issue #325 is two things bundled together: (1) a `wayfinder_*` native Drupal
data-type vocabulary alongside the existing `solr_*` compat aliases, and (2) a scope
expansion — "Wayfinder becomes a standalone project" — that keeps the
Solr-compatible **wire shape** (URL path structure, param names `q`/`fq`/`facet.field`/…,
JSON envelope keys and ordering, the `search_api_solr` field-naming convention
`zs_*`/`tus_*`/`twm_suggest`/`ts_title`/…) but renames every place the literal token
"solr" appears in **Wayfinder's own API surface**. This report covers the delivered
work, which is scope (2) — the vocabulary rename. Naming scheme:

| Old | New |
|---|---|
| `/solr/{core}/…` | `/wayfinder/{core}/…` |
| `solr-spec-version` / `solr-impl-version` | `wayfinder-spec-version` / `wayfinder-impl-version` |
| `solr_home`, `/var/solr/data` | `wayfinder_home`, `/var/wayfinder/data` |
| `drupal-4.4.0-solr-9.x-0` (`core.schema`) | `drupal-4.4.0-wayfinder-9.x-0` |
| `solr.StrField` etc. (field-type `class` values) | `wayfinder.StrField` etc. |
| `org.apache.solr.update.DirectUpdateHandler2` | `dev.wayfinder.update.DirectUpdateHandler2` |
| `"description": "SolrCore"` | `"WayfinderCore"` |
| `solr-mbeans` (envelope key) | `wayfinder-mbeans` |
| `Basic realm="solr"` | `Basic realm="wayfinder"` |
| `reported_solr_version` (TOML key) | `reported_server_version` + `#[serde(alias = "reported_solr_version")]` |

Internal identifiers also renamed (no external observer, but reachable via
`src/lib.rs`'s `pub mod extract;`): `solr_class_for_builtin` →
`field_class_for_builtin`; `SOLR_CELL_DEFAULT_FMAP`/`_UPREFIX` →
`EXTRACT_DEFAULT_FMAP`/`_UPREFIX`; `solr_cell_fields`/`solr_cell_index` →
`extract_cell_fields`/`extract_cell_index`; `extract::solr_cell_source_fields` →
`extract_source_fields` (this one is a breaking rename of a public API per the
ticket's own note, since the test suite imports `wayfinder::extract::*`).

Touched 45 files (`git diff main..HEAD --stat`): `src/lib.rs`, `src/config.rs`,
`src/coverage.rs`, `src/admin_ui.rs`, `src/extract.rs`, `templates/ping.html`,
`README.md`, `docs/PRD.md`, `docs/deployment.md`, the Drupal module
(`WayfinderBackend.php`, `WayfinderClient.php`, its README and integration
scripts/tests), `bench/run.sh`, `coverage/search_api_coverage_contract.json`,
`coverage/search_api_solr_4.4.0_source_evidence.json`, and every Rust test file
that constructs a `/solr/…` URI or asserts a renamed response key.

Deliberately **not** renamed, because they record real Solr or real
`search_api_solr`, verified against the tree: everything under `solr-ref/`
(`git diff main..HEAD -- solr-ref/` is empty — confirmed), the sha256-pinned
excerpts and source paths/symbols in
`coverage/search_api_solr_4.4.0_source_evidence.json`, the `search_api_solr`
data-type names (the compat-alias half of the ticket, not touched by this
delivered slice), and prose describing real Solr's taxonomy — e.g.
`src/lib.rs:1675,1687`, `solr.StandardTokenizerFactory`/`solr.BinaryField`/
`solr.BoolField`, confirmed present and unchanged.

## Process

Five parallel agents on disjoint file sets, then an integration pass, then two
rounds of review.

## Review outcome — round 1 bounced with 7 must-fix, all fixed in `cdac2f0`

1. `bench/run.sh` — five `$WF_BIND` URLs still pointed at `/solr` (ping check,
   `index_corpus`, `warm_up_pass`, `run_cold_query_pass`, `run_query_load`), so the
   benchmark harness's Wayfinder leg spun to a ping timeout and never got past cold
   start. `bench/` was owned by no agent. Confirmed by diff: `cdac2f0` rewrites
   exactly those five call sites from `/solr` to `/wayfinder`.
2. `docs/deployment.md` — the published Caddyfile's `@public_health` matcher
   (`path /ui/ping /solr/content/admin/ping`) no longer matched the renamed
   endpoint on the Wayfinder leg, so an operator copying it verbatim would have
   proxied the unauthenticated, health-disclosing ping endpoint to the public
   internet. Security-relevant. Fixed to `/wayfinder/content/admin/ping`.
3. `docs/PRD.md` divergence 5 claimed `core.schema` "is not a divergence: it is
   compared exactly" — `tests/differential.rs`'s `admin_system` entry in
   `EXPECTED_DIVERGENCES` now says the opposite (the rename makes `core.schema`
   diverge from the fixture's pinned `"drupal-4.4.0-solr-9.x-0"`). Per this
   project's CLAUDE.md ("divergence is a bug unless the PRD documents it as
   deliberate") that left a newly undocumented divergence. Fixed; `cdac2f0`/`357df14`
   restate both `core.schema` and the `lucene.solr-spec-version`/`-impl-version`
   keys as documented, permanent divergences in both files, and PRD divergence 9
   (auth realm) was also corrected.
4. `README.md` — stale auth-exemption path list (`/solr/<core>/admin/ping`),
   `realm="solr"` in the 401 doc text, and the `wayfinder.toml` config sample
   using `reported_solr_version`.
5. `templates/ping.html` — shipped, rendered HTML body text naming
   `/solr/{{ core_name }}/admin/ping`, a path that now 404s.
6. `drupal/search_api_wayfinder/README.md` — wrong default base path
   (`http://localhost:8983/solr/<core>`) and no upgrade note.
7. `#[serde(alias = "reported_solr_version")]` had **zero** test coverage: all
   tests passed with the attribute deleted. Confirmed independently in this
   review by deleting the attribute and re-running `tests/admin_info_system.rs`:
   `server_config_admin_accepts_the_legacy_reported_solr_version_key` fails with
   `unknown field \`reported_solr_version\`, expected \`reported_server_version\`
   in \`admin\`` — the attribute was restored immediately after (working tree is
   clean; `git diff --stat -- src/config.rs` shows nothing).

All seven, plus five five-minute items, fixed in `cdac2f0`. Three further
follow-ups from round 2 landed in `357df14` (PRD wording precision on the impl
version string, `/solr/{core}/update/extract` → `/wayfinder/…` in the PRD's
tracer-bullet section, and the remaining `/solr/<core>/…` examples in the PRD's
"done when" checklist).

## Two orchestrator instructions the reviewer overruled

- **Moving `admin_system`/`admin_info_system` from `EXPECTED_DIVERGENCES` to
  `ACCEPTED_DIVERGENCES`.** The orchestrator proposed this as a permanent-divergence
  cleanup; the implementing agent refused, and the reviewer adjudicated the agent
  right. Confirmed by reading `tests/differential.rs` directly: the comment at
  lines 1154–1159 states `accepted_divergence_reason` short-circuits **before**
  `diff()` runs, while `EXPECTED_DIVERGENCES` entries still go through the real
  differ and only suppress the failure — and names `admin_system`/
  `admin_info_system` explicitly as permanent-but-still-diffed entries that
  belong in `EXPECTED_DIVERGENCES` for that reason. Moving them to
  `ACCEPTED_DIVERGENCES` would have *reduced* checking, not just relabeled it.
  `admin_system`'s entry (lines 1919–1934) is in fact still in
  `EXPECTED_DIVERGENCES` on this branch.
- **Flagging `solr.StandardTokenizerFactory`/`solr.BinaryField`/`solr.BoolField`
  in `src/lib.rs` as a possible missed rename.** Adjudicated correct as-is:
  confirmed at `src/lib.rs:1675` and `:1687` — these name classes Wayfinder
  deliberately does not emit, in doc-comment prose about real Solr's own
  field-type taxonomy, not Wayfinder's reported values.

## Evidence — independently re-run on the final tree, not just inspected

- `cargo test`: **1189 passed, 61 suites**, re-run clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --check`: clean.
- `cargo test --manifest-path bench/Cargo.toml`: **74 passed, 19 suites**.
- `bash -n bench/run.sh`: OK.
- Drupal PHPUnit: **318 tests, 549 assertions, OK** — but only after
  `composer dump-autoload`; a stale `vendor/composer/autoload_psr4.php` produces
  spurious `LinkedFileExtractionTest` errors locally, pre-existing and unrelated
  to this branch.
- The `#[serde(alias = "reported_solr_version")]` mutation test above (attribute
  deleted, target test fails with the exact "unknown field" message, attribute
  restored).

**One claim in the source material for this report does not hold and is corrected
here rather than repeated: `cargo run -- coverage --format json` does not
reproduce `coverage/search_api_coverage_contract.json` byte-identically.** That
JSON file is the frozen *input* denominator (`CONTRACT`, `include_str!`'d into
`src/coverage.rs` and `tests/search_api_coverage.rs`) with shape `{traces,
captured_parameters, endpoints, request_semantics, response_fields}`. Running
the CLI command produces a *different*, derived structure — `{traces, endpoints,
request_semantics, response_fields, overall}` — that cross-checks the frozen
contract against live route/probe evidence; it is not a regeneration of the
contract file and the two are not expected to be equal. What **is** true, and
was verified by actually running the command on this tree: the `overall` field
reports `{"covered": 75, "uncovered": 0, "total": 75, "fraction": "75/75"}`,
matching `tests/search_api_coverage.rs`'s pinned `EXPECTED_FRACTION = "75/75"`.
`endpoint_covered()` (`src/coverage.rs:311`) does a literal
`route.path == path && (route.accepts_method)(method)` against `ROUTES`, so a
missed rename in a route literal would surface as an uncovered endpoint id — this
did not happen here.

The `core.schema` rename was checked against the pinned `search_api_solr` 4.4.0
source, not assumed: `SolrConnectorPluginBase.php` has exactly three
`explode('-', …)` consumers of that value, reading `$parts[1]`, `$parts[3]`, and
`$parts[4]`. `$parts[2]` — the only segment the rename touches (`solr` →
`wayfinder`) — is read by none of them, and the split arity is 5 both before and
after. The one remaining consumer renders the whole string for display and
compares nothing.

## Follow-ups left open

- **The Drupal stored-config migration is a documented manual step, not a
  `hook_update_N`.** Search API merges stored plugin config over
  `defaultConfiguration()`, so a server saved before this change keeps
  `path = /solr` and 404s against a renamed server at the first request. No
  update hook was written: the stored value is legitimately operator-owned (a
  proxy may deliberately keep `/solr`), and a hook would silently break a site
  intentionally pointed elsewhere. `drupal/search_api_wayfinder/README.md` now
  has an explicit "Upgrading" section with the manual steps (confirmed present
  in `cdac2f0`'s diff). Nothing in code enforces this migration.
- **Three surfaces this rename touched are backed by inspection only, never
  execution**, called out by the reviewer under the 2-round cap:
  - `bench/run.sh` needs Docker and a real `solr:9` container; no test exercises
    its URLs, which is exactly why must-fix 1 above was silent through both the
    test suite and `bash -n` and would still be silent today if run again.
  - The published Caddyfile in `docs/deployment.md` was checked by reading
    `public_auth_path` in `src/lib.rs` against the matcher text, but was never
    deployed to an actual Caddy instance.
  - `drupal/search_api_wayfinder/tests/integration/run.sh` is gated on
    `WAYFINDER_INTEGRATION=1` and Docker, and was not run in either review round.

  These are untested surfaces, not known defects — but the review process
  capped at 2 rounds per this project's default, and none of the three got
  execution-level verification in that window.

## Gaps against the ticket, for the record

Issue #325's first half — native `wayfinder_*` Drupal data-type ids as a
compat-alias addition alongside `solr_*` (new data-type plugins under
`src/Plugin/search_api/data_type/`, widened `FieldMapper`/`supportsDataType()`
predicates) — is **not** part of this branch's diff and remains open. This
report covers only the "retire the literal token 'solr' from Wayfinder's own API
surface" scope-expansion half. The issue itself is still open on GitHub
(`gh issue view 325`) for that reason.
