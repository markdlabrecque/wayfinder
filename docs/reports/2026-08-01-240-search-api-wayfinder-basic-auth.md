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

## Exported-config tradeoff

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

The supplied red unit tests were preserved. Focused red evidence was 44 tests
with 9 failures before implementation; focused verification passed all 44
assertions after the implementation. The Docker integration harness passed,
including `AUTH: PASS` and `ROUNDTRIP: PASS`.

Full hermetic gate (with `PI_SUBAGENT_*` and `PI_WORKFLOW_*` unset only in
that spawned process) passed: `cargo fmt --check`; `cargo clippy --all-targets
-- -D warnings`; `cargo test`; the matching three `bench` commands; and full
PHPUnit (`134` tests, `215` assertions). No supplied test was changed.
