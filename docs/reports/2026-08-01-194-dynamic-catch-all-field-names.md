# Issue #194: dynamic catch-all field-name guard

- Issue context: #194 (intended PR body context: `Closes #194`; no PR is claimed to exist yet).
- Branch: `markdlabrecque/issue-194-fields-entry-named`

## Approved spec

When any `[[dynamic_fields]]` rule exists, an explicit `[[fields]]` entry named `_dynamic` or
`_dynamic_text` must fail during schema loading with an ordinary `anyhow` error. The error names
the field and contains `reserved`; it must not reach Tantivy and panic. Without dynamic rules,
both names remain valid static fields.

## Implementation and changed files

- `src/schema.rs`: added the reservation guard before Tantivy builder calls.
- `tests/schema_layer.rs`: added a positive loop covering both reserved names with dynamic rules,
  plus the regression proving both names remain valid when no dynamic rules exist.
- `docs/schema.md`: documented the conditional behavior.

This report is the additional changed file created for the implementation record.

## TDD and mutation evidence

- The initial new test failed for the intended missing behavior: Tantivy panicked with
  `Field already exists in schema _dynamic`.
- After implementation, the targeted schema tests passed.
- Mutation check: temporarily removing `_dynamic_text` from the reservation made the loop test
  fail with Tantivy panic `Field already exists in schema _dynamic_text`; the mutation was then
  reverted.

## Gate evidence

- `cargo test --test schema_layer` — passed.
- An earlier full-gate attempt encountered an unrelated timing failure in
  `autocommit_max_time_arms_even_when_a_later_doc_in_the_batch_is_invalid`.
- An immediate targeted rerun passed.
- Final `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed,
  including a fresh run after rebasing onto the latest `origin/main`.

The implementation gates ran in the foreground. The reviewer independently ran the same full gate
with child workflow identity unset only for that spawned test process.

## Review outcome

- Round 1 requested narrowing the reservation to schemas that actually have dynamic rules.
  This was fixed, with negative tests and documentation added.
- Round 2 approved with no findings; the reviewer full gate passed.

## Outstanding work, deviations, and risks

No accepted deviations. No unresolved risks. No deferred follow-ups. No failed or deliberately
skipped steps beyond the earlier unrelated timing failure, which was resolved by the immediate
passing targeted rerun and the passing final full gate.
