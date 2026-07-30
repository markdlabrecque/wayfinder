# Issue #80 — carry over the search_api_wayfinder Docker integration harness

Part of #57 (search_api_wayfinder backend), follow-up from #75 (M1) round-2 review.

## What was built

M1 (#75) shipped verified only by a hermetic PHPUnit suite mocking Drupal's
`FieldInterface`/`IndexInterface`. Two of the round-1 review's must-fix bugs on that
ticket were wire-contract facts the mocks encoded *wrong*, and a green unit suite
could not catch either: the mocks agreed with the wrong model. The same pattern
repeated twice more after M1 landed — #81 (`isMultiValued()` misreading cardinality
from `isList()`) and #83 (`formatValue()` serializing real `TextValue` objects to
`{}`) were both real bugs that a green unit suite had already passed.

This ticket carries over and adapts a Docker-based integration harness under
`drupal/search_api_wayfinder/tests/integration/`:

- `docker-compose.yml` — a real Drupal site container plus a real Wayfinder
  instance (via `presets/search-api.toml`), no mocks on either side.
- `create_content.php` — creates real Drupal content through the entity API
  (real nested property paths, real `TextValue` objects), not hand-built arrays.
- `setup_server_index.php` — configures a Search API server/index using the
  standalone `backend: 'wayfinder'` plugin from #75 (the old worktree's harness
  wired `backend: 'search_api_solr'` + `connector: 'wayfinder'`; all of that
  connector-specific wiring was replaced).
- `run_queries.php` — runs a real `WayfinderBackend::search()` query through
  Search API's normal query API and asserts on the result.
- `run.sh` — orchestrates the whole run: brings up Docker, waits for both
  containers to be ready, installs the site, indexes content, asserts a
  post-index `numFound`, runs the query script, tears down.

Gated behind `WAYFINDER_INTEGRATION=1`, the same pattern as `WAYFINDER_DIFF_SOLR=1`
for the differential harness — it needs real Docker and is not part of default
`cargo test`/`vendor/bin/phpunit` CI.

Commits on this branch (`git log --oneline 0252568..HEAD`, oldest to newest):

- `6107b67` — carry over the harness from the old worktree, adapted to the
  standalone backend.
- `70dd736` — force a sync commit before querying (Solr/Wayfinder's async commit
  window was racing the query step) and document the #84 blocker discovered
  while making the harness actually run.
- `1d761ec` — round-1 must-fix hardening (see Pipeline below).

## Pipeline

1. **test-writer / implementor (carry-over)**: ported the old worktree's harness
   files, replacing all `search_api_solr`/Solarium-connector wiring with the
   standalone `backend: 'wayfinder'` plugin. Added the sync-commit-before-query
   fix once the harness first ran against real Docker and exposed a race between
   Wayfinder's commit and the query step.
2. **reviewer round 1: BOUNCED**, four must-fix items:
   - `run.sh`'s search_api_solr/solarium absence check used `composer why`, which
     exits 1 both for "package absent" and for "package present but not required
     by root" — collapsing two different states the check needed to distinguish.
   - The harness's Solr port (`18983`) collided with `bench/run.sh`'s
     `SOLR_HOST_PORT` default, so the two harnesses could not run concurrently
     on one machine.
   - The post-index assertion was a comment claiming indexing worked, not an
     actual `numFound` check.
   - The wayfinder-ping readiness wait loop fell through silently on exhaustion
     instead of failing loudly, so a genuinely-unready server would proceed into
     the rest of the run instead of stopping the harness with a clear error.
3. **implementor round 1 fixes** (`1d761ec`): switched the absence check to
   `composer show` (a clean binary present/absent signal); moved the port from
   `18983` to `18990` to stop colliding with `bench/run.sh`; added a real
   `numFound` assertion via curl after indexing; changed the ping-wait-loop
   exhaustion path to a loud `exit 1`.
4. **reviewer round 2: APPROVED**, and independently re-ran the full Docker
   harness themselves end-to-end rather than trusting the implementor's
   transcript — this is what surfaced issue #84 (below): the reviewer's own run
   showed indexing succeeding but the search step failing, and traced it to a
   genuine Wayfinder core bug rather than anything in this harness or the
   `search_api_wayfinder` module.

## Test evidence

```
$ cargo test
... 488 passed; 0 failed
$ cargo fmt --check
(clean)
$ cargo clippy --all-targets -- -D warnings
(clean)
$ cd drupal/search_api_wayfinder && vendor/bin/phpunit
OK (56 tests, 79 assertions)
```

All four gates green. These are the pre-existing hermetic suites (Rust core +
PHPUnit); they are unaffected by this ticket, which adds a fifth, deliberately
non-hermetic gate (`WAYFINDER_INTEGRATION=1`) that is not part of the above.

### The integration harness itself: deliberately red

Running `WAYFINDER_INTEGRATION=1 tests/integration/run.sh` end-to-end (confirmed
independently by the round-2 reviewer, not just claimed by the implementor):

- Docker containers come up, Drupal installs, the site configures a Search API
  index against the standalone `wayfinder` backend.
- `create_content.php` creates real nodes through the entity API.
- Indexing succeeds and the new `numFound` assertion passes — this is a real,
  positive proof that #81's cardinality fix and #83's `TextValue` fix both hold
  under real Drupal data, not just under unit-test mocks.
- The final query round trip fails: `run_queries.php` exits 1 on a genuine
  Wayfinder core bug, filed as **issue #84** (see below). This is documented in
  `run_queries.php`'s own header comment as the harness's current expected/red
  state, analogous to an `EXPECTED_DIVERGENCES` entry in `tests/differential.rs`
  — a red that is expected to flip green on its own once #84 lands elsewhere,
  not something to paper over in this branch.

## The #84 discovery

The harness's whole reason for existing — catching a real bug a green unit suite
would agree with anyway — worked exactly as designed, on the very first real
Docker run. Indexing worked (proving #81 and #83 hold under real data), but the
search step 400s with:

```
edismax `qf` names no field this core has: `ts_body ts_title`
```

Root cause (filed as
[issue #84](https://github.com/markdlabrecque/wayfinder/issues/84), **Wayfinder
core, `src/core_index.rs`, explicitly out of scope for #80 and the
`search_api_wayfinder` module**): `CoreIndex::resolve_field_weights` resolves
every name in `qf`/`pf` via a literal Tantivy `Field` lookup only, and never
falls back to `match_dynamic()` the way the plain `q` text path's
`rewrite_dynamic_fields()` does. Any dynamic-field name in `qf` — which is the
*normal* case for `search_api_solr`-convention traffic (`ts_title`, `tm_body`,
etc.), i.e. essentially all real Drupal traffic — silently resolves to nothing,
and `defType=edismax` hard-errors.

The round-2 reviewer independently reproduced this **outside Docker**: built the
real `wayfinder` binary, ran it directly against `presets/search-api.toml`, and
confirmed the asymmetry with a byte-identical error message plus a working
control case (the same field name succeeds via the plain lucene `q=ts_title:...`
path, which does go through `rewrite_dynamic_fields()`). This independent,
out-of-container reproduction is why #84 is filed with confidence as a core bug
rather than a Docker/harness artifact.

The round-2 reviewer also flagged, and it was added as a follow-up comment on
#84 itself, that a naive fix to `resolve_field_weights` alone is not sufficient:
`_dynamic_text` is a JSON catch-all field, `tokenize_for_field` returns
unanalyzed raw text for non-`Str` field types, and `build_field_disjunction`
builds `Term::from_field_text` with no JSON path — so a shallow fix would
silently degrade the failure from a loud 400 into an ambiguous "0 results", and
would also collapse every dynamic `qf` name onto one `Field` handle, destroying
per-field boost weighting.

## Review outcome

Two rounds were used. Round 1 found four real must-fix defects in the harness
itself (composer-check semantics, a port collision with `bench/run.sh`, an
assertion that only existed as a comment, and a silent wait-loop fallthrough) —
all four fixed in `1d761ec`. Round 2 approved outright, and the reviewer went
beyond re-reading the diff: they independently re-ran the entire Docker harness
themselves, which is what surfaced #84. Per the pipeline's own convention, a
work item that bounces once and is approved on round 2 has had a real, substantive
second pass, not a rubber stamp — the round-2 reviewer's independent
reproduction of #84 outside Docker is direct evidence of that.

## Update: #84 landed, full round trip now green

#84 (Wayfinder core `resolve_field_weights`/edismax dynamic-field fallback,
PR #88) merged to `main` on 2026-07-30. This branch was rebased onto the new
`main` and the full `WAYFINDER_INTEGRATION=1` harness was re-run end-to-end,
twice consecutively:

```
$ WAYFINDER_INTEGRATION=1 bash drupal/search_api_wayfinder/tests/integration/run.sh
...
confirmed: 3 document(s) indexed for wf80_index
--- real index+search round trip ---
fulltext_wayfinderroundtrip: 1 results
  result item id: entity:node/1:en
ROUNDTRIP: PASS - real index+search round trip through WayfinderBackend::search() succeeded
--- tearing down wf80-* containers ---
$ echo $?
0
```

The previously-red final step (acceptance criterion 2) is now green: a real
index+search round trip through `WayfinderBackend::search()` passes,
end-to-end, with no mocks on either side.

One new, real defect surfaced by the re-run (not present in the original
round-1/round-2 review, since it only manifests on a *second* run against a
stale `drupal-site/` directory): Drupal's own post-install hardening leaves
`sites/default` read-only (`chmod 555`) and `settings.php` read-only
(`chmod 444`). The script's `rm -rf drupal-site` at the top of the run
therefore fails with `Permission denied` / `Directory not empty` on any
re-run, contradicting follow-up 5's original claim below that this only
"bites re-runs on native Linux" — it bites on Docker Desktop/macOS too, just
via permission bits rather than uid. Fixed with a one-line
`chmod -R u+w drupal-site 2>/dev/null || true` immediately before the `rm -rf`
(`run.sh`, right above the existing `rm -rf drupal-site` / `mkdir -p
drupal-site` pair). Re-run confirmed clean on a fresh directory and on a
directory left over from a prior run.

## Follow-ups (deferred, not fixed in this branch, all flagged by round-2 review unless noted)

1. **`run.sh:142`'s new `numFound` curl doesn't handle its own curl failure
   gracefully under `set -euo pipefail`** — a curl failure aborts the script
   before it can print a diagnostic, the same latent issue as the pre-existing
   sync-commit curl at line 137.
2. **`run.sh:93-104`'s search_api_solr/solarium absence check block lacks
   `set -euo pipefail` and has no positive control** — a broken `composer` or a
   wrong `cd` would silently false-pass the check rather than fail it. Per this
   repo's convention ("code whose whole value is failing correctly gets
   mutation-tested"), this check needs a positive control (e.g.
   `composer show drupal/search_api` must independently succeed) to prove it
   can actually fail.
3. **`jq` is now a required host-side prerequisite for `run.sh`**, undocumented
   in the script's header comment.
4. **Cosmetic**: `1d761ec`'s commit message slightly misstates the round-1
   `composer why` finding — it says `composer why` "exits 1 both when absent and
   when root-required," when the actual nuance is that `why` exits 1 for
   absent-or-not-required-by-root. Full detail is in the round-1 review record;
   not critical, callout only.
5. **Pre-existing follow-ups carried over from round 1, still deferred**:
   - `run.sh`'s drush command redundancy (`search-api:index || sapi-i` — both
     invoke the same underlying command, so the fallback cannot help if the
     first form fails).
   - No `timeout-minutes` set on the new CI `workflow_dispatch` job.
   - ~~Root-owned `drupal-site/` is not cleaned up after a run — bites re-runs
     on native Linux, harmless on Docker Desktop (bind-mount ownership
     differs).~~ **Fixed above** — was actually a permission (not ownership)
     issue that bites Docker Desktop too; resolved with a `chmod -R u+w`
     before the teardown `rm -rf`.
   - A fixed `sleep 3` stands in for a real readiness check on the Drupal
     container, rather than polling until it actually responds.

## Acceptance criteria (from #80) — status

- [x] Harness runs against a real Wayfinder + Drupal (docker-compose), gated
      behind `WAYFINDER_INTEGRATION=1`, not part of default CI
- [x] At least one real index+search round trip passes end-to-end — indexing
      passes (proven with a real `numFound` assertion, not a comment), and the
      search half now passes too, post-#84 (PR #88): `ROUNDTRIP: PASS` above.
- [x] Old worktree's connector-specific harness wiring (`search_api_solr` +
      Solarium connector) fully replaced with the standalone `backend:
      'wayfinder'` plugin — no reference to `search_api_solr`/Solarium remains,
      and the harness itself now actively checks for and rejects their presence
      (`composer show`)
- [x] docs/reports entry (this document)

All acceptance criteria are now met.

Gates re-run directly against the working tree on
`80-search-api-wayfinder-harness`, rebased onto `main` post-#84: `cargo test`
(490 passed), `cargo fmt --check` (clean), `cargo clippy --all-targets -- -D
warnings` (clean), `vendor/bin/phpunit` (56/56 tests, 79 assertions), and the
Docker-gated `WAYFINDER_INTEGRATION=1` harness itself, run twice consecutively
end-to-end with a real `ROUNDTRIP: PASS`.
