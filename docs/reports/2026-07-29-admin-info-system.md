# Report: /admin/info/system version handshake (issue #59)

- Branch: `59-admin-info-system`
- Commit: `15a5a0f` (`feat(admin): add /admin/info/system version-handshake endpoint`, `Closes #59`)
- Scope: `src/lib.rs`, `src/config.rs`, `tests/admin_info_system.rs`,
  `tests/differential.rs`, `solr-ref/responses/admin_info_system.json`,
  `solr-ref/responses/admin_system.json`, `solr-ref/manifest.tsv`,
  `solr-ref/manifest-errors.tsv`, `solr-ref/capture.sh`, `docs/PRD.md`.
  `docs/solr-ref-findings.md` was deliberately **not** touched (see below).

## What was built and why

Two new routes in `src/lib.rs`:

- `/solr/admin/info/system` — server-level, no core segment.
- `/solr/{core}/admin/system` — core-scoped.

Both serve a Solr-wire-compatible envelope so `search_api_solr`'s
`SolrConnector::getSolrVersion()` gets a coherent version handshake from
Wayfinder. That method reads `lucene.solr-spec-version`, falling back from the
core-scoped path to the server-level one when the core-scoped call fails —
both routes exist because Drupal's connector can hit either depending on
context.

Envelope shape:

- `lucene.solr-spec-version` / `-impl-version` come from a new `[admin]`
  config section, `reported_solr_version` (String, default `"9.0.0"`).
- `core.schema` is a fixed constant, `CORE_ADMIN_SCHEMA =
  "drupal-4.4.0-solr-9.x-0"` — not a placeholder. search_api_solr's
  `SolrConnectorPluginBase.php` does `explode('-', $schema)` and indexes into
  `parts[1]` (module version), `parts[3]` (Solr branch), `parts[4]`, so the
  dash-part shape and the literal value both matter, not just presence of the
  key. Documented in `docs/solr-ref-findings.md` finding 78 (from issue #55).
- `jvm` / `system` / `security` are static, deliberately non-introspected
  placeholders, documented as such in code — no fixture consumer reads their
  values, only the shape.

Fixtures `solr-ref/responses/admin_info_system.json` and `admin_system.json`
are verbatim copies of the already-captured
`solr-ref/search-api/trace/00023.json` / `00026.json` — ground truth from
issue #55's Search API capture, a real search_api_solr Drupal site against
real Solr 9.10.1.

## Version choice and rationale

PRD open question 2 ("which Solr version to report") is resolved:
`reported_solr_version` defaults to `"9.0.0"`, chosen as the lowest version in
the 9.x branch the Search API capture's generated `schema.xml` already
targets. This was verified directly, not assumed: grepping a clone of
`search_api_solr`'s source confirmed every `version_compare()` gate in that
module that unlocks a feature Wayfinder does not implement sits at or below
Solr 8.x, so any 9.x report is safe, and `9.0.0` is the literal lowest such
value.

The value is deliberately unclamped — an operator who overrides it owns the
compatibility risk (covered by a mutation test asserting an implausible
override is still accepted and reported verbatim).

## Divergence handling

`tests/differential.rs`:

- `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` gained an `admin_info_system` entry.
- `EXPECTED_DIVERGENCES` gained an `admin_system` entry.

Both are field-level, permanent divergences (the deliberately-configured
version string, and unreproducible host JVM/OS stats) — same category as the
pre-existing `ping` entry's `rid` counter. They are **not** in
`ACCEPTED_DIVERGENCES`, which is reserved for whole-entry pass/fail waivers
(e.g. 404-shape mismatches); this placement matches existing precedent.

`docs/PRD.md` §2 gains ratified-divergence 5 documenting exactly which fields
diverge (`lucene.solr-spec-version`/`-impl-version`, `jvm.*`/`system.*`) and
states explicitly that `core.schema` is **not** a divergence — it is compared
exactly against `"drupal-4.4.0-solr-9.x-0"` because getting that value wrong
breaks the client, not just cosmetics. PRD §6 gains the `[admin]
reported_solr_version` tuning knob. PRD §10 open question 2 is struck with its
resolution and a pointer to ratified-divergence 5.

## File-ownership correction

The issue's original file-ownership table assumed both fixtures belonged in
`solr-ref/manifest.tsv`. In practice:

- `solr-ref/manifest.tsv` gained one core-relative row (`admin_system`) — the
  core-scoped route is a plain core-relative GET, so it belongs there.
- `solr-ref/manifest-errors.tsv` gained one server-level row
  (`admin_info_system`) — the differential harness's `manifest.tsv` path GETs
  every row against a specific core, and the server-level route has no core
  segment, so it does not fit that harness's contract and belongs in
  `manifest-errors.tsv` instead.

This is called out explicitly because it corrects the issue text rather than
following it as written, per this repo's "don't paper over a wrong ticket
premise" convention.

## Tests added

`tests/admin_info_system.rs` (new, 20+ tests), including:

- Config defaults, override, and reject-unknown-key.
- Default-version-is-9.0.0 and configured-version-override.
- An unclamped-implausible-version mutation test (confirms no clamping
  exists).
- Top-level-key-shape checks for both routes.
- `strict_params` mutation-test coverage for both new routes (added in round
  1 remediation, see below).
- Method-agnosticism.
- Core-not-found 404 and a core-scoping sanity check.
- `core.schema` dash-part-shape check, plus (added in round 2 remediation) an
  exact-literal-value assertion.
- Pinned exact-value checks for `responseHeader`, `mode`, `solr_home`,
  `core_root`, and the Lucene spec version (added in round 2 remediation).

## Review history

**Round 1** (reviewer, Opus): 3 must-fix items —

1. `core.schema` placeholder didn't have the right dash-part shape.
2. Vague `EXPECTED_DIVERGENCES` reason strings.
3. Missing mutation-test coverage for `check_params`/`strict_params` on the
   new routes.

All three fixed by the implementor, verified independently by the
sub-orchestrator (git diff read plus a full gate run: 441 passed, fmt clean,
clippy clean) before proceeding to round 2.

**Round 2** (reviewer, Opus, final round per the pipeline's max-2-rounds
rule): BOUNCE with 3 must-fix items —

1. The PRD didn't document the divergence, per CLAUDE.md's "undocumented
   divergence is a bug" rule — open question 2 was still open, no
   ratified-divergence entry existed.
2. The `EXPECTED_DIVERGENCES` reason strings claimed fields like `mode` /
   `solr_home` / `core_root` / `core.schema` were "compared exactly and do
   match," but nothing in `tests/admin_info_system.rs` actually pinned those
   literal values — only presence/shape was checked, so e.g. changing
   `CORE_ADMIN_SCHEMA` to any other 5-dash-part string would have stayed
   green.
3. `solr-ref/capture.sh`'s new `cap admin_system` / `capx admin_info_system`
   lines would have captured the *wrong* core's schema (the tracer-bullet
   `content` core, not the Search API capture's `search_api_capture` core)
   and silently corrupted the committed ground-truth fixture on any future
   re-run of that script.

Because round 2 was the last allowed reviewer round, the sub-orchestrator
(not a fresh implementor) fixed all three directly, rather than bouncing to
a third round:

- `docs/PRD.md` edited as described above (ratified-divergence 5, tuning
  knob, open-question-2 resolution).
- `tests/admin_info_system.rs` gained pinned exact-value assertions: a new
  test (`admin_info_system_pins_the_fields_the_differential_reason_string_claims_match`)
  plus an exact-literal assertion added to the existing `core.schema`
  shape test.
- The two erroneous `capture.sh` lines were removed and replaced with a
  comment explaining these fixtures are verbatim trace copies from issue
  #55's capture, not reproducible by that script against the current core
  set.

Re-ran the full gate suite after this remediation: 442 passed (20 suites, one
net new test over round 1's count), fmt clean, clippy clean.

**Per the pipeline's cap-out rule: this work went through the maximum two
review rounds and could use further review passes** — round 2 was the final
allowed round, and its fixes were applied directly rather than going through
a fresh implementor + round-3 review cycle.

## Deliberate no-op: `docs/solr-ref-findings.md`

The sub-orchestrator's file-ownership grant reserved findings 79–84 for this
issue, but no new finding was needed. Finding 78 (from issue #55) already
documents the version-detection mechanics this issue implements against, and
issue #59's own decisions — which version to report, and why `core.schema`'s
dash-part shape matters — are recorded in `docs/PRD.md` and in doc comments
in `src/lib.rs` instead. This is stated here explicitly as a deliberate
no-op, not an oversight.

## Gate results (verified in this worktree, `59-admin-info-system`, post round-2 remediation)

- `cargo test`: 442 passed (20 suites, 0 failed).
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean, no issues.

## Follow-ups (open, non-blocking — deferred by the reviewer, not fixed here)

1. The "this list is empty until the next entry needs it" comment near
   `EXPECTED_DIVERGENCES_MANIFEST_ERRORS` is now stale phrasing, given the
   `admin_info_system` entry sits right below it.
2. `solr_home` / `core_root` mimic Solr's real container paths
   (`/var/solr/data`) while `core.directory.*` invents `/var/wayfinder/{core}`
   — an inconsistent story; pick one.
3. `ADMIN_INFO_PARAMS` accepting `wt`/`json.nl` has no `ponytail:` comment
   naming the ceiling, unlike the `time_allowed` precedent in `src/config.rs`.
4. `check_core`'s 404 on an unknown core uses `Envelope::WithParams` (echoing
   `responseHeader.params`), though the success envelope has none —
   `Envelope::Bare` would be the closer match. No fixture pins this either
   way, since Solr answers HTML there, not JSON.
5. The JVM/OS placeholders assert a Java 17 runtime that doesn't exist.
   Harmless in the differential harness, but surfaces verbatim in Drupal's
   connector UI if an admin looks at it.

## Pointers

- Production code: `src/lib.rs` (route handlers, `CORE_ADMIN_SCHEMA`,
  `ADMIN_INFO_PARAMS`), `src/config.rs` (`[admin] reported_solr_version`).
- Tests: `tests/admin_info_system.rs`, `tests/differential.rs`
  (`EXPECTED_DIVERGENCES` / `EXPECTED_DIVERGENCES_MANIFEST_ERRORS`).
- Fixtures: `solr-ref/responses/admin_info_system.json`,
  `solr-ref/responses/admin_system.json`.
- Docs: `docs/PRD.md` §2 ratified-divergence 5, §6 tuning knob, §10 open
  question 2 (resolved); `docs/solr-ref-findings.md` finding 78 (unchanged,
  reused).
- Commit: `15a5a0f`.
</content>
