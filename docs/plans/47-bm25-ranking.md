# Issue #47: category-field ranking divergence

## Goal

Resolve #47 by proving the captured Solr order for `q=category:animals`, identifying the root cause, and making the smallest change still needed on current `main`.

## Plan

1. Verify the Solr fixture expects `doc1, doc4` and reproduce current Wayfinder behavior in the dedicated worktree.
2. Trace schema/query scoring to identify whether string-field norms cause the reported `doc4, doc1` order.
3. Check whether merged work already fixed the root cause and whether a fixture-derived regression test protects it.
4. If behavior is still wrong, first add a targeted failing test, then implement the minimal root-cause fix. If it is already correct, do not duplicate production changes; mutation-test the existing guard by temporarily restoring field norms and proving the test fails.
5. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
6. Record the issue history, root cause, mutation evidence, and review verdict in `docs/reports/2026-07-29-47-bm25-ranking.md`; open a PR with `Closes #47`.

## Acceptance criteria

- `q=category:animals` returns `doc1` before `doc4`, matching `solr-ref/responses/select_q_field_term.json`.
- A fixture-derived regression test fails if field norms are re-enabled for `string`/`keyword` fields.
- No duplicate or broader scoring implementation is introduced if current `main` already contains the root-cause fix.
- All repository gates pass.
- The report and PR clearly explain that exact BM25 score magnitudes remain governed by PRD ratified divergence 4, while ranked document order must match.
