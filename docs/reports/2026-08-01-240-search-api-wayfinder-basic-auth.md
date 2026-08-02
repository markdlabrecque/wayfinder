# Issue #240 — Search API Wayfinder Basic authentication

## Delivered behavior

`search_api_wayfinder` now has optional top-level `username` and `password`
backend configuration and matching schema keys. The password form element has
no default value; its stored value is retained in `FormState` and a blank
submission preserves it only when the username is unchanged. Validation matches
Wayfinder/RFC 7617: both values must be non-empty or both empty, usernames may
not contain `:`, and neither value may contain ASCII controls. Unicode and
other permitted characters are accepted.

`WayfinderClient` accepts optional credentials without breaking existing
callers. It supplies Guzzle's `auth => [$username, $password]` only for a
complete pair, on shared request paths and `ping`; incomplete or absent pairs
remain unauthenticated. The existing 401 JSON-envelope extraction continues to
raise `SearchApiException('authentication required')`.

## Accepted exported-config tradeoff

This deliberately follows `search_api_solr`'s `BasicAuthTrait`: credentials
are ordinary exported backend plugin configuration. That keeps the connector
compatible and dependency-free, but the password is present in Drupal config
exports and must be protected with the site's normal configuration-access
controls. No Drupal Key integration or dependency was added by design.

## Integration evidence

The manual, Docker-gated harness mounts a harness-local `wayfinder.toml` with
`operator` / `secret`, sets `WAYFINDER_CONFIG`, and configures those backend
credentials. It proves an unauthenticated client can ping the intentional
public endpoint but that select raises exactly `authentication required`; then
it completes the authenticated Drupal index/search round trip. The fixture is
isolated to this harness and uses no production credential.

## Tests

The focused PHP suite has 44 tests. Its assertions were preserved; the exact
401 Wayfinder authentication-envelope fixture was corrected. Focused
verification passed all 44 tests (82 assertions). The Docker integration
harness passed, including `AUTH: PASS` and `ROUNDTRIP: PASS`.

### Credential-forwarding mutation evidence

For a mutation check, a trap-backed temporary copy of
`WayfinderBackend.php` was made and `WayfinderBackend::getClient()` was
mutated to omit its final `$config['username'] ?? ''` and
`$config['password'] ?? ''` arguments to `new WayfinderClient(...)`.
`WAYFINDER_INTEGRATION=1 bash drupal/search_api_wayfinder/tests/integration/run.sh`
then exited `1` as expected during indexing with
`Drupal\search_api\SearchApiException ... authentication required`; the
harness consequently reported `FAIL: expected indexed documents for
index_id=wf80_index, found 0`. The EXIT trap restored the original file
exactly and removed its temporary backup. Re-running the same integration
command after restoration passed, with `AUTH: PASS - public ping and exact
unauthenticated select failure verified` and `ROUNDTRIP: PASS - real
index+search round trip through WayfinderBackend::search() succeeded`.

## Review and verification

Round 1 requested credential-forwarding mutation evidence, translated form
errors, and stale-comment fixes; all were addressed. Round 2 found no
implementation defect, but its gate process did not clear every workflow
identity variable. The foreground resolved that procedural escalation by
unsetting every `PI_SUBAGENT_*` and `PI_WORKFLOW_*` variable, verifying none
remained, and running one full gate successfully: `cargo fmt --check`; `cargo
clippy --all-targets -- -D warnings`; `cargo test`; the matching three `bench`
commands; and full PHPUnit (`134` tests, `215` assertions).

No implementation findings or follow-ups remain. The accepted exported-config
credential tradeoff above is the only residual risk.
