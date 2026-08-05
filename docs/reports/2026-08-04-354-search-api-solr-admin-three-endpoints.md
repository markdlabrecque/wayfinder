# #354 — three `search_api_solr_admin` endpoints stay out of scope (unreachable against Wayfinder)

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-354-three-non-cloud`.
**Status:** **descoped with evidence** — core reload, `analysis/field`, and
`admin/file` are not built, not routed, and not added to any param list; the
descope is recorded in PRD §5 and guarded. `analysis/field`-style analyzer
introspection is recorded as a v2.5 Wayfinder-own candidate rather than built
here.

Found by the 2026-08-04 full-source sweep of `search_api_solr` 4.4.0's
`modules/search_api_solr_admin/`, a shipped submodule the 3-file snapshot did
not include. All three are on the standard (non-cloud) connector, so the
SolrCloud non-goal does not by itself exclude them — but a stronger exclusion
does.

## The evidence: `search_api_solr_admin` cannot see a Wayfinder server

Every route and every command path in the module hard-gates on
`$backend instanceof SolrBackendInterface`, and `WayfinderBackend`
(`drupal/search_api_wayfinder/src/Plugin/search_api/backend/WayfinderBackend.php:41`)
is `extends BackendPluginBase implements PluginFormInterface` — a peer of
`SearchApiSolrBackend`, not a `search_api_solr` connector. Deliberately: Wayfinder
is not Solr, so modelling it as a `SolrConnectorInterface` (cores, configsets,
Solarium query objects) would be a category error.

The gate appears in:

- All three access-check classes — `SolrAdminAccessCheck` (reload-core route),
  `SolrAdminCloudAccessCheck`, `SolrAdminTrustedContextSupportedAccessCheck`
  (`modules/search_api_solr_admin/src/Access/*.php:27`).
- The field-analysis route's access check, `LocalActionAccessCheck`
  (`src/Access/LocalActionAccessCheck.php:25`, shipped in `search_api_solr` core).
- The Drush commands (`SearchApiSolrAdminCommands.php:137`) and the hooks
  (`SearchApiSolrAdminHooks.php:40`).

The command path is the strongest statement of intent. `Utility::getSolrConnector($server)`
(`src/Utility/Utility.php:1265-1272`) — which the reload Drush command
(`SolrAdminCommandHelper::reload()`, `:72-75`) and every command-helper entry
point call — is declared `: SolrConnectorInterface` and throws
`SearchApiSolrException('Server %s is not a Solr server')` when the backend is
not a `SolrBackendInterface`, before it can reach `reloadCore()` /
`getAnalysisQueryField()` / `getFile()`.

So against a Wayfinder server:

- The reload-core and field-analysis **forms** 403 at Drupal's own route access
  check (`AccessResult::forbidden()`), not Solr's.
- The reload **Drush command** throws "Server is not a Solr server" from
  `Utility::getSolrConnector`.
- No HTTP request for any of the three endpoints is ever emitted.

The capture could not see these not merely because that one site happened not to
use the module, but because the module has no path to a non-Solr backend. This is
a strictly stronger exclusion than "zero client evidence in the trace."

## Decision: all three stay out of Search API parity scope

Not built, not routed, not admitted to `SELECT_PARAMS` / `UPDATE_PARAMS`. The
three, individually:

- **Core reload** — `GET /solr/admin/cores?action=RELOAD&core=<core>` (server-level).
  `StandardSolrConnector::reloadCore()` builds a CoreAdmin `createReload()`.
  Unreachable via the gate, and Wayfinder has no reload concept to answer it with
  regardless: config is TOML loaded once at process start (§3/§6), the schema is
  fixed at index creation and refused on incompatible change (open question 4),
  and one process serves one core (open question 1). A 200-no-op would be the
  "silently wrong answer" the project rejects (it would tell an operator their
  config change applied when it did not), so the endpoint stays unrouted rather
  than faking success.
- **Field analysis** — `GET /<core>/analysis/field`. `getAnalysisQueryField()` →
  Solarium `createAnalysisField()`, setting `analysis.fieldtype` and
  `analysis.fieldvalue`. The one with real independent value — Wayfinder has a
  genuine analyzer chain (`schema.rs::tokenize`) and v2.5's admin UI (§5) has no
  "what did the analyzer do to my text?" answer. But as a `search_api_solr_admin`
  parity endpoint it is unreachable (the gate), and building a Solr-wire-shaped
  `/wayfinder/{core}/analysis/field` that no stock client can ask would violate
  §5's "ship what clients demonstrably use." Analyzer introspection is recorded
  as a **v2.5 Wayfinder-own candidate** (a `/ui` or Wayfinder-native surface with
  its own shape — not necessarily Solr's per-component Java-class-name
  breakdown), not a parity endpoint built under #354. It is listed in the v2.5
  section's out-of-scope list so the candidacy is visible.
- **Configset file read** — `GET /<core>/admin/file?file=<name>`. `getFile()`
  serves raw configset files (`schema.xml`, `solrconfig.xml`, …). Wayfinder has
  no configset (§3), so there is nothing honest to return; the four `getFile()`
  callers (`Utility::getServerFiles`, `SolrConfigForm`,
  `SolrConfigSetController`'s config-zip download, and
  `search_api_solr.install`'s requirements check) all read configset XML that
  does not exist in a Wayfinder core, and all sit behind the same
  `SolrBackendInterface` gate regardless.

`uploadConfigset()` is Cloud-only (`StandardSolrCloudConnector.php:253-263`) and
stays out under the SolrCloud non-goal, as #354 already noted.

## Coverage denominator: 75/75 is unchanged

Issue #354 flagged that these "move the coverage denominator" and that
recomputing 75/75 against a widened denominator is part of this work. The
finding above resolves that the other way: because none of the three is
reachable against a Wayfinder server, none belongs in the denominator, so there
is nothing to widen to. None of the three is in any of the 28 committed traces
(`solr-ref/search-api/trace/`) or in any of the contract's 9 endpoints / 75 items
(`coverage/search_api_coverage_contract.json`), and the decision not to build
them moves the coverage fraction by zero.

Verified live:

```
$ cargo run --quiet -- coverage --format json
endpoints: 9/9  request_semantics: 51/51  response_fields: 15/15   # 75/75
```

The PRD records this explicitly (the new §5 subsection states "75/75 is
unchanged"), and the guard pins that wording.

## Changes

- `docs/solr-ref-findings.md`: finding 194 — the `instanceof SolrBackendInterface`
  gate, the `Utility::getSolrConnector` throw, the per-endpoint evidence, and the
  decision.
- `docs/PRD.md`:
  - New §5 subsection "`search_api_solr_admin` — a Solr connector module,
    unreachable against Wayfinder", placed next to the `solr_document` descope so
    the parity picture stays in one place. Records the decision, finding 194, the
    three endpoints individually, and the coverage-denominator statement.
  - v2.5 "Out of scope, explicitly, for this phase": analyzer introspection added
    as a named **candidate** (not a commitment), pointing back at the §5 descope
    so it is not mistaken for a `search_api_solr_admin` parity endpoint.
- `tests/search_api_solr_admin_descope_guard.rs`: new self-expiring guard (see
  below).

No production source, no routing, and no param-list changes — the descope is
exactly "do not build these." `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` are unaffected (docs + one new test
file); the new test file is itself fmt/clippy-clean.

## The guard — `tests/search_api_solr_admin_descope_guard.rs`

Five channels, modelled on `tests/q_op_qt_descope_guard.rs` and
`tests/version_write_descope_guard.rs`:

1. **Source channel.** The reload-core route's access check, the field-analysis
   route's access check, and the `Utility::getSolrConnector` command-path throw
   all still gate on `SolrBackendInterface`; the three connector methods
   (`createReload`, `createAnalysisField`, `<core>/admin/file`) are still real on
   the standard/base connector so the guard stays meaningful on a source upgrade.
2. **`WayfinderBackend` invariant.** Our connector module still does not
   reference `SolrBackendInterface` (positive control: still
   `extends BackendPluginBase`). The day it starts `implements
   SolrBackendInterface`, every admin route stops 403ing and the descope premise
   is gone.
3. **Trace channel.** None of the 28 traces hits `analysis/field`, `admin/file`,
   or `admin/cores` (positive control: the corpus carries `/select` traffic, so
   the absence is real).
4. **PRD channel.** The §5 subsection records the descope, references #354 and
   finding 194, names `SolrBackendInterface`, and states the denominator is
   unchanged.
5. **Executable.** The three endpoints 404 against a built, indexed app — a later
   silent route addition is caught. (Verified empirically when the guard was
   written: axum's default 404 for an unregistered route.)

17 tests, all green. Per CLAUDE.md's mutation rule for guards, the executable arm
was confirmed against the real router before the assertions were written: the
three paths returned 404 with a null body, as an unregistered route must.

## Premise correction

Issue #354's framing — "All three are on the standard (non-cloud) connector, so
the SolrCloud non-goal does not excuse them" — is correct at the connector layer
but misses the layer that decides reachability: a Wayfinder site runs no
`search_api_solr` connector at all, because `search_api_solr_admin` is hard-gated
on `SolrBackendInterface` and `WayfinderBackend` is a separate backend. The
issue's own lean ("`analysis/field` is worth building") is therefore overturned
*for the parity question* — building a Solr-wire-shaped endpoint no stock client
can ask would violate §5. The operator value the issue rightly identified is
preserved by recording analyzer introspection as a v2.5 Wayfinder-own candidate,
where it can be scoped on its own merits (and its own shape) rather than smuggled
in as parity.

## Verification

- `cargo test --test search_api_solr_admin_descope_guard` — 17 passed.
- `cargo test --test search_api_coverage` — 8 passed (denominator intact).
- `cargo run --quiet -- coverage --format json` — 9/9 + 51/51 + 15/15 = 75/75.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` — clean
  (no production source touched; the new test file is itself clean).

## Follow-ups

- If analyzer introspection is wanted for v2.5, open a v2.5 issue (not a parity
  one): the honest shape is over Wayfinder's own tokenizer/filter names, not
  Solr's per-component Java-class breakdown, and it would be a `/ui` or
  Wayfinder-native surface. The candidacy is recorded in the v2.5 section.
- The guard fires the day any of {the `instanceof` gate leaves the source,
  `WayfinderBackend` implements `SolrBackendInterface`, a trace carries one of
  the three} stops holding. The fix then is to revisit this decision (#354), not
  to weaken the guard.
