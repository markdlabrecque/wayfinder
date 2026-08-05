# Issue #399 — Solr compatibility contract

## Spec

Reconcile the governing compatibility policy so Solr captures remain immutable factual evidence without automatically creating product scope. Require exact Solr fidelity on supported, client-exercised paths unless a deliberate supported-path departure is ratified. Keep unsupported and out-of-scope behavior in PRD §5, factual findings in `docs/solr-ref-findings.md`, and treat differential expected-divergence lists as regression/inventory evidence rather than an implementation queue.

## Changed behavior

- `CLAUDE.md` and PRD §2 now distinguish immutable evidence, supported client contracts, and product scope.
- Every deliberate departure on a supported path belongs in PRD §2 with evidence, rationale, and an issue/report; unsupported behavior belongs in PRD §5.
- Unsupported local-params parser types moved from ratified divergence 6 to PRD §5 without changing runtime behavior.
- `facet.method=enum` moved from `ACCEPTED_DIVERGENCES` to the self-expiring manifest-errors inventory because that method is unsupported; its status and response body still pass through differential comparison.
- Findings and differential-harness prose no longer describe expected divergences as owning issues, to-do items, or product commitments.
- `tests/documentation_contract.rs` guards the policy and ledger boundaries; local-params documentation tests now point to PRD §5.

## Evidence

- Initial documentation contract was confirmed red before the policy edits.
- Mutation check: replacing the supported/unsupported ledger rule caused `tests/documentation_contract.rs` to fail for the expected reason; the original was restored.
- `cargo test --test documentation_contract` — pass (3 tests).
- `cargo test --test differential manifest_errors_every_row_runs_against_the_matching_hermetic_app` — pass.
- `cargo test --test local_params` — pass (12 tests).
- `cargo test --test edismax_descope_guard` — pass (9 tests).
- `cargo test --no-fail-fast` — pass before the final narrow review fixes.
- `cargo fmt --check` — pass.
- `cargo clippy --all-targets -- -D warnings` — pass.
- Independent reviewer final gate, `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — pass.

## Review

Independent review approved after two policy corrections:

1. PRD §2 ratification applies to every supported-path departure, not only client-exercised paths.
2. The unsupported `facet.method=enum` fixture is inventory evidence, not an accepted supported-path divergence.

Final verdict: **APPROVED**, with no unresolved findings.
