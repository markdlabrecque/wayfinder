# Issue #196 — partial `fl` patterns

## Approved spec and captured decision

Implement partial field-list patterns for Search API requests: `*` expands within an `fl`
member, while `?` remains literal. Real Solr capture is the decision source. The new fixture,
`solr-ref/responses/select_fl_ss_wildcard.json`, records `fl=ss_*` returning only the five
matching `ss_*` fields; the capture recipe was appended to `solr-ref/capture.sh`.

## Changed behavior

`CoreIndex::render_doc` now uses dependency-free, Unicode-safe `*` glob matching when selecting
stored fields. It supports prefix, suffix, and multi-star patterns, including the required
backtracking case; `?` has no wildcard meaning. Existing stored-only selection, schema/render
order, deduplication, and `score` behavior are preserved.

## Implementation and evidence

- Test commit `fe2bc4c` captured the fixture and proved RED: the actual response document was
  `{}` where the fixture expected `ss_field_sku`.
- Implementation commit `f7c2301` adds the matcher in `src/core_index.rs` and uses it from
  `render_doc`; it also adds the repeated-suffix unit test.
- Mutation evidence: a greedy matcher was killed by the repeated-suffix unit test; an overmatch
  mutant was killed by the fixture integration test.

## Gates and review

Implementer and Reviewer each used:

```text
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --no-fail-fast
```

Both runs passed. Round 1 requested a fix for a greedy-suffix matching bug. That fix was made;
Reviewer round 2 ran the same gate and observed **866 passed, 0 failed**, then approved. After
rebasing onto `origin/main` at `c590c4f`, the foreground Orchestrator reran the same full gate:
formatting, clippy, and all **866 tests passed with 0 failures**. After `main` advanced again,
the branch was rebased onto `b00dc4d` and the full gate was rerun: formatting, clippy, and all
**881 tests passed with 0 failures**.

## Follow-up / residual risk

Nonblocking advisory only: the appended capture block lacks failure-path trap cleanup. On normal
runs it recreates and removes its named container, but a failed run can leave that container
behind. No other findings, deviations, or unresolved risks were reported.
