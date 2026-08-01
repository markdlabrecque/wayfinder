# JSON NamedList flat rendering

## Approved spec
`select` honors `json.nl=flat` with alternating NamedList output. Repeated `json.nl=map&json.nl=flat` is first-wins (`map`). Captured coverage probes for both semantics are satisfied.

## Implementation and evidence
- Fixtures captured against a one-off `solr:9`: `select_json_nl_flat.json` and `select_json_nl_repeated_map_flat.json`.
- `solr-ref/capture.sh` has a reproduction block and the manifest has corresponding rows; the differential suite includes both fixtures.
- Root cause: `JsonNl::from_params` already rendered `flat` and parameter parsing was already first-wins. Production change only adds `json.nl` to `UPDATE_PARAMS`.
- Tests were added/updated. Coverage improved from 67/75 to 68/75; both semantics are true.

## TDD and verification
- Red: new strict-update test returned `400 unknown parameter` before the allowlist fix.
- Green: the same test passed after the fix.
- Current-state commands passed: `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test`; coverage command. Differential tests include the captured fixtures.

## Review and workflow record
Both review rounds passed their full gates and reported only low-severity stale-comment findings. After the default two-round cap, the foreground Orchestrator corrected the remaining comment-only provenance and reran all current-state gates successfully.

This is an accepted, recoverable workflow escalation/process deviation caused by a retained subagent file lock plus the review cap. It carries no behavior risk. There are no review findings, unresolved risks, or follow-ups remaining.
