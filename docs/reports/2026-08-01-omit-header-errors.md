# Issue #179 — `omitHeader` on errors and boolean spellings

- Branch: `markdlabrecque/unsettled-does-omitheader-true-suppress-response`
- Issue: #179
- Reference: Solr 9.10.1 (`solr:9.10.1`)

## Captured contract

Fresh Solr captures establish that `omitHeader` suppresses `responseHeader` on both success and error responses. Accepted values are case-insensitive `true`/`yes`/`on` and `false`/`no`/`off`. Numeric and single-letter forms such as `1` and `t` are invalid; Solr rejects them as Jetty HTML 400 responses before its JSON response writer runs. `/update` errors also suppress their normally header-bearing `NoParams` envelope.

The capture block, fixtures, error-manifest rows, and finding 109 record that evidence. Existing fixtures were not recaptured.

## Changed behavior

- Centralized the accepted `omitHeader` vocabulary in `Params`.
- Enabled the envelope policy only for endpoints that support the parameter: `/select`, `/mlt`, `/terms`, and `/update`.
- Applied suppression to `WithParams` and `/update`'s `NoParams` error envelopes as well as success responses.
- Kept unsupported admin endpoint behavior inert: `omitHeader` cannot alter strict unknown-param or unknown-core errors there.
- Kept invalid values inert until endpoint parameter validation runs, so they cannot suppress an earlier unknown-core response.
- Once validated as invalid on a supported endpoint, returned a headerless JSON 400 through an explicit error policy.

PRD divergence 8 deliberately retains Wayfinder's JSON-only error contract for invalid values instead of reproducing Solr's container-level Jetty HTML response.

## Tests and review

`tests/omit_header.rs` covers accepted true/false spellings, success and error envelopes, select/MLT/terms/update registration, update `NoParams` errors, unsupported admin scope, invalid JSON errors, and invalid values before unknown-core validation.

Review used the default two rounds. Round 1 found that suppression inferred from raw params leaked into unsupported admin errors; endpoint authorization was made explicit and guarded. Round 2 found that invalid values acted as true before validation; suppression was separated into an explicit validation-error policy, with unknown-core select/update guards. The second fix was handled as a foreground escalation after the review cap, then verified by the full local release gates. No third independent review was claimed.

Mutation evidence: temporarily accepting invalid values made `select_invalid_omit_header_values_return_headerless_json_400` fail (expected 400, received 200); the mutation was reverted before the final gates.

## Evidence

On the final tree:

- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test` — passed, including all 22 `omit_header` tests and the hermetic differential suite.
- `git diff --check` — passed.

CI evidence remains pending until the PR runs.
