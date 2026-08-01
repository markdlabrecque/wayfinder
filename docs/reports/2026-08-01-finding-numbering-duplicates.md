# Findings 16/17/18 were two different sets of findings

Follow-up to #198 / PR #202, which swept drifted citations repo-wide and added
`tests/finding_citations.rs`. That sweep deliberately left one thing: `docs/solr-ref-findings.md`
numbered **16, 17 and 18 twice** — once in issue #3's faceting capture, once in issue #2's `sort`
capture. Its `findings_are_numbered_uniquely` test pinned the duplicate set at `{16, 17, 18}` with
a message naming itself for deletion when they were fixed. This is that fix.

## What changed

- Issue #3's faceting findings renumbered **16/17/18 -> 105/106/107** (next free numbers). Issue
  #2's `sort` 16-18 keep theirs, being contiguous with 19/20, which are uniquely cited.
- Nine external citation sites moved: `src/facet.rs:32,739`, `tests/differential.rs:654,659,663,1892`,
  `docs/PRD.md:168`, `docs/reports/2026-07-31-138-facet-local-params-key.md:64`,
  `docs/reports/2026-07-28-faceting-aggregation.md:332`. Four in-doc cross-references too.
- New **Numbering** section in the findings doc: a number is assigned once and never reused, and
  32, 33, 43, 44, 45, 85, 86 are vacated-and-never-reused.
- `findings_are_numbered_uniquely` now asserts `duplicated.is_empty()`, per its own instruction.

## Evidence

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` clean; `cargo test` 851 passed.
- Mutation-tested: renaming `107.` back to `18.` in the doc fails both guards with the right
  message (`numbers {18} more than once`, plus `finding 107 does not exist`). Reverted.

## Ceiling

The guard checks that a cited number *exists*, not that the finding supports the sentence citing
it. A citation of faceting-16 left stale by this renumber would now silently resolve to the `sort`
finding instead of dangling — the reason the duplicate had to go, and the reason the uniqueness
assertion is the load-bearing half.
