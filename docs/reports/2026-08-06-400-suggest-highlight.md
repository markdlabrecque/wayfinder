# #400 — honor `suggest.highlight` at the public `/suggest` seam

**Date:** 2026-08-06. **Issue:** #400 (claimed before implementation and
announced publicly). **Status:** complete, independently reviewed and
approved.

## Approved spec / settled behavior

At the public `/suggest` request seam, `suggest.highlight` is a boolean
switch: it defaults to `true`; an explicit `false` returns plain suggestion
terms; an explicit `true` returns highlighted terms unless `suggest.cfq`
engages a context filter. An engaged context filter remains unhighlighted,
regardless of `suggest.highlight`. An absent, empty, or otherwise
non-engaging `cfq` follows the normal highlighted path when highlighting is
enabled.

This is an accepted product divergence from the frozen historical Solr wire:
`suggest_q_hl_off_en.json` records a highlighted term despite
`suggest.highlight=false`. Drupal's public client sets this parameter false
and applies its own highlighting, so Wayfinder must honor the caller's
explicit request instead.

## Implementation summary

- `tests/suggest.rs` replaces the fixture-equality assertion for the
  highlight-off case with a public `/suggest` request assertion: false returns
  `quick brown fox`, while the fixture is explicitly retained as evidence of
  the historical `quick brown <b>fox</b>` behavior. The context-filtered,
  explicit-highlight-on coverage remains plain.
- `src/lib.rs` derives highlighting from the shared expression
  `suggest.highlight` (default true) and `!cfq_engages_filter(cfq)`, retaining
  the established empty/non-engaged `cfq` behavior.
- `docs/PRD.md` records divergence 12 and its rationale.
- Frozen fixtures, including `solr-ref/responses/suggest_q_hl_off_en.json`,
  were unchanged.

## Verification evidence

Initial red command:

```text
cargo test --test suggest suggest_q_hl_off_en_returns_plain_terms
```

It failed for the intended missing behavior: the actual response term was
`quick brown <b>fox</b>`, while the new expectation was the plain
`quick brown fox`.

Targeted green tests passed after implementation, including the
highlight-off public `/suggest` test and the explicit-highlight-on,
context-filtered test.

The Implementer full gate and the independently repeated Orchestrator full
gate both passed:

```text
cargo fmt --check                                  # clean
cargo clippy --all-targets -- -D warnings          # clean
cargo test --no-fail-fast                          # 1473 passed, 0 failed, 1 ignored (#362)
```

The ignored test is the pre-existing #362 measurement test. Where these full
gates were launched as child-run gates, their spawned test processes had
`PI_SUBAGENT_*` and `PI_WORKFLOW_*` unset; runtime identity was not changed.

## Review outcome

An independent Reviewer returned **APPROVED** with no findings. No review cap
was reached. There are no unresolved follow-ups.

## Residual risk

There is no dedicated four-way truth-table test combining
`suggest.highlight=false` with empty and engaged `suggest.cfq` values. This
is mitigated by the single shared boolean expression in `src/lib.rs` and the
existing branch tests for false-on-plain and true-with-engaged-`cfq` behavior.
This is a residual test-combination risk only; it is not an unresolved
follow-up.
