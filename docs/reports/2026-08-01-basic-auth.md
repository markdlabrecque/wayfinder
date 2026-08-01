# Issue #229 — optional HTTP Basic authentication

## Approved spec and route decision

Issue #229 adds optional HTTP Basic authentication for a Wayfinder deployment. `[auth]` is
optional: absent configuration preserves the existing open server; a present section requires
both a non-empty username and password. The approved route decision protects all application
routes when auth is configured, including Solr wire endpoints, `/update`, and the admin UI. Only
`/solr/<configured-core>/admin/ping` and `/ui/ping` remain public, so health infrastructure can
distinguish an unavailable process from unavailable credentials. The configured-core restriction
is exact; another core's ping and a look-alike longer path require authentication.

## Changed behavior and files

Implementation checkpoint: `01447db` (`fix(auth): harden credentials and health exemptions`),
following `4d1e19d` (`feat(auth): add basic authentication`).

- `src/config.rs`: adds optional `[auth]`; retains only a SHA-256 digest of
  `username:password` and compares a presented digest with `subtle::ConstantTimeEq::ct_eq`.
  It rejects missing, non-string, empty, colon-containing username, and ASCII-control-character
  credentials; passwords may contain colons. It extracts `[auth]` from `toml::Value` before generic
  deserialization so syntax and semantic errors cannot echo or coerce credential values.
  `AuthConfig`'s debug output does not expose values.
- `src/lib.rs`: applies auth middleware to the application surface, accepts case-insensitive
  `Basic` scheme syntax with one valid base64 payload, returns 401 with
  `WWW-Authenticate: Basic realm="solr"` and Wayfinder's JSON `WfError` envelope on failure, and
  exempts only the two approved health routes.
- `tests/basic_auth.rs` and `src/config.rs` tests: cover accepted credentials, malformed/wrong
  authorization, protected select/UI/update paths, exact health exemptions, invalid credential
  config, colon-in-password acceptance, and no syntax-error secret leak.
- `README.md`: documents `[auth]`, its validation/open-default semantics, protected/public
  surfaces, the plaintext-equivalent Basic warning, and the current live-knobs table.
- `docs/solr-ref-findings.md`: appends finding 118.
- `docs/PRD.md`: ratifies divergence 9.
- `docs/reports/2026-08-01-basic-auth.md`: this record.

## Credential and timing design

The config does not retain plaintext credentials after parsing: it hashes the exact UTF-8
`username:password` bytes with SHA-256. Each presented Basic payload is hashed to the same fixed
length, then compared with `subtle` constant-time equality. This avoids a variable-length,
early-exit string comparison at the credential boundary. Invalid config fails startup instead of
opening the server, and parse errors do not include the raw TOML/credential values. These measures
limit accidental retention/leakage but do not make Basic an encrypted transport protocol.

## Solr capture and corrected premise

Reported live capture, 2026-08-01: a cloud-mode `solr:9` container enabled BasicAuthPlugin with
`auth enable operator:secret/blockUnknown`. The request sequence was:

1. `GET /solr/admin/info/system` without `Authorization` -> 401 Jetty HTML,
   `Authentication failed, Response code: 401`.
2. The same GET with a wrong Basic credential -> 401 Jetty HTML, `Bad credentials`.
3. The same GET with correct `operator:secret` Basic credentials -> 200.

Both 401 responses carried `WWW-Authenticate: Basic realm="solr"`. This corrects the ticket's
premise that Solr returned a JSON auth-failure envelope: its auth filter produced Jetty HTML before
the JSON writer. Finding 118 records the evidence. The capture used `docker run -d --name wayfinder-auth-capture-229 -p 18983:8983 solr:9 solr-fg -c`, followed by `docker exec wayfinder-auth-capture-229 bin/solr auth enable --type basicAuth --credentials operator:secret --block-unknown true`. The three responses were requested with `curl` against `http://127.0.0.1:18983/solr/admin/info/system`, first without credentials, then with `-u operator:wrong`, and finally with `-u operator:secret`.

## Deliberate divergence

PRD ratified divergence 9 records that Wayfinder returns its JSON `WfError` envelope for auth
401s while the captured Solr auth filter returned Jetty HTML. Decision: match the 401 and
`Basic realm="solr"` challenge, but retain JSON because Wayfinder's response surface is JSON-only
and its clients parse JSON. This is analogous to ratified divergences 1 and 8. Risk: a consumer
expecting Jetty HTML will differ; the project does not support that response format.

## Tests, verification, and review

- Initial red evidence: `cargo test --test basic_auth` initially failed because `auth` was an
  unknown server-config field.
- Mutation evidence: `AuthConfig::matches` was deliberately changed to accept every credential;
  `cargo test --test basic_auth` failed in `basic_auth_requires_well_formed_matching_credentials`
  (exit 101), proving wrong credentials cannot bypass unnoticed. The mutation was reverted.
- Implementer verification after each review bounce remained green:
  - `cargo fmt --check` — exit 0.
  - `cargo test --test basic_auth` — exit 0.
  - `cargo clippy --all-targets -- -D warnings` — exit 0.
  - `cargo test` — exit 0.
- Full gates ran with all `PI_SUBAGENT_*` and `PI_WORKFLOW_*` variables unset only in the spawned test process, preserving runtime identity while keeping foreground-harness tests hermetic.
- Documentation validation: `git diff --check` passed; `cargo test --test finding_citations` passed (2 tests).

Review round 1 raised these must-fix findings, resolved in `01447db`:

1. The initial health exemption accepted any core's `/admin/ping`; resolution: bind the exemption
   to the configured core name and add negative tests for another core and a suffix path.
2. Basic credential config needed RFC-7617-safe validation; resolution: reject empty values,
   username colons, and ASCII controls while allowing password colons, with config tests.
3. TOML syntax errors could expose source excerpts containing credentials; resolution: parse to
   `toml::Value` first and replace syntax errors with a generic message, guarded by a sentinel
   no-leak test.

Review round 2 verified the route, middleware, validation, constant-time comparison, dependency,
and test contracts, then found one remaining semantic TOML leak: serde could echo an integer
credential value or coerce a TOML datetime after syntax parsing. The foreground chose the
recoverable **fix** path at the two-round review cap. The final implementation extracts and
validates `[auth]` before generic deserialization, with integer/datetime/no-leak regressions; its
full gate passed. Round 2 also identified stale PRD dashboard text and missing README divergence
wording, both corrected. No reviewer round beyond the configured cap was claimed.

## Unresolved risks and follow-ups

- **Basic has no transport encryption.** It is plaintext-equivalent and must be used only on
  loopback or a private trusted network, or behind TLS termination.
- **Public health checks.** `/solr/<configured-core>/admin/ping` and `/ui/ping` intentionally
  remain unauthenticated and disclose service health.
- **JSON auth-401 divergence.** Wayfinder intentionally does not reproduce Solr's Jetty HTML
  auth-failure body (PRD divergence 9/finding 118).

No other deferred follow-ups, accepted deviations, failed gates, or posting failure are known.
