# Report: update pipeline (issue #9)

- Branch: `9-update-pipeline`
- Scope: implement `/update` (add/delete/commit commands, overwrite
  semantics, `commitWithin`/`softCommit` visibility, copy-field and
  single-valued-field validation) plus two round-1-review fixes: a load-time
  check that `core.unique_key` is a declared, static, Text-kind field, and a
  fix so a mid-batch validation error still arms autocommit's
  time/doc-count follow-through for docs already written before the error.

## History

The implementation, capture, and round-1 review fixes were done by an
earlier fable orchestrator (commits `b505155`..`d89d764`, 2026-07-28) working
on its own container (`wayfinder-solr-9`, port 8989). That orchestrator went
idle after the round-1 fix commit without opening a PR — no further activity
on the branch or worktree since. I (the coordinator) picked up the relay
directly per the stall protocol rather than resuming a dead session: verified
the branch, rebased it onto current `origin/main`, fixed what the rebase
broke, re-ran the gates, and opened the PR.

## Rebase

`origin/main` had moved on since this branch's base (#31/#33/#38/#41 landed).
Rebasing onto `origin/main` surfaced two things:

1. A textual conflict in `docs/solr-ref-findings.md` — both sides had
   appended new sections after the same point (this branch's finding 46-49
   for the update-pipeline capture; main's already-superseded draft of
   finding 42, later corrected by issue #34's `fl=score` landing). Resolved
   by keeping `origin/main`'s corrected finding 42 and appending this
   branch's findings 46-49 section after it, unmodified.
2. A non-textual break: `CoreIndex::add_documents` gained a second
   `overwrite: bool` parameter on `origin/main` (issue #8), and one test
   helper in `src/core_index.rs` (`indexed_scored_hit`) still called the
   old one-argument form. Fixed by passing `true` (commit `e2c1ac5`) —
   the helper always wants overwrite semantics; no other call site was
   affected.

## Findings

`docs/solr-ref-findings.md` gained findings 46-49 (34 new
`manifest-errors.tsv` fixtures against a self-contained `update9` core):
the bare-`responseHeader`-only success envelope for every `/update` command
shape, `GET /update`'s content-stream-vs-method-error distinction, pinned
overwrite/delete/commit semantics, and visibility timing for
`commitWithin`/`softCommit`/uncommitted adds.

## Verification

I did not re-review the original implementation's logic line by line (that
was this branch's own round-1 review, already recorded in its commit
messages); I verified the state that matters for merging:

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean, zero warnings.
- `cargo test` — all 10 suites green post-rebase, including
  `tests/differential.rs` (27 passed) and the new `tests/update_pipeline.rs`
  (23 passed).
- Confirmed no existing fixtures churned: only new files appear under
  `solr-ref/responses/` relative to `origin/main`.

## Follow-ups (not actioned here, out of scope for landing this branch)

- Issue #40 (opened from this branch's round-2 review) extends the
  unique_key load-time check to reject `multi_valued` and analyzed
  (non-`Str`) unique keys — deliberately left for its own PR, stacked on
  this one.

## Pointers

- Production code: `src/lib.rs` (`/update` handler), `src/core_index.rs`
  (`add_documents`, delete/overwrite, autocommit arming), `src/schema.rs`
  (unique_key load-time check)
- Tests: `tests/update_pipeline.rs` (23 tests)
- Fixtures: `solr-ref/responses/update_*.json`,
  `solr-ref/manifest-errors.tsv`, `solr-ref/capture.sh` (update9 block)
- Solr facts learned: `docs/solr-ref-findings.md` findings 46-49
- Commits (post-rebase hashes): `9aeca4a`..`c0d7a42` (original
  implementation/capture/round-1 fixes, findings-doc conflict resolved
  within `9aeca4a` during the rebase), `e2c1ac5` (post-rebase
  `add_documents` call-site fix)
</content>
