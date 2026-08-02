# Report: online snapshot (issue #239)

- Branch: `239-online-snapshot`
- **Durable goal / approved scope:** add `wayfinder snapshot <live-data-dir> <fresh-destination-dir>`: produce a consistent, durable copy of a live Tantivy index without stopping commits or merges. PRD v4 read-replica scope is unchanged.

## Implementation

- Added `src/snapshot.rs` and CLI wiring in `src/main.rs`. A short Tantivy `META_LOCK` read selects one committed generation and opens its component handles; copying is then from those handles while writers continue committing/merging.
- The snapshot is staged, required files, an Index reader, and checksums are validated before publication. Every copied file, staging directory, and parent directory is synced.
- Publication is atomic no-clobber (`NOREPLACE`) on Linux, Apple, and Redox; unsupported targets fail closed. The destination receives truthful `.managed.json`, persisted schema, and analyzer marker.
- Updated `docs/operations.md`.
- Added `tests/online_snapshot.rs`: original snapshot reopens, an existing destination is not clobbered, and 20 snapshots during 30 committed 80-document batches each contain only whole batches and observe a merge.

## Evidence

- Initial red: `cargo test --test online_snapshot -- --nocapture` — failed for the expected reason: `snapshot` was parsed as a schema path.
- Final targeted: `cargo test --test online_snapshot -- --nocapture` — 2 passed, about 5.28s.
- `cargo clippy --all-targets -- -D warnings` — green.
- `git diff --check` — green.
- Final `cargo test` after the durability and merge-evidence fixes — green. Earlier full gates were also green after initial implementation, in reviewer round 1, after the first review bounce, and in the round-2 replacement.

## Review and deviations

- **Round 1 verdict: changes requested.** Must-fix findings were a long-held lock that stalled commit/merge, weak concurrency evidence, possible publication with missing required files, TOCTOU no-clobber, and stale operations docs. All were fixed.
- **Round 2 reached the review cap; it was not approval.** Must-fix findings were crash-durability syncing and a concurrency test that did not prove whole batches or an observed merge. The foreground fixed both under recoverable escalation because repeated Implementer child sessions failed from harness file-lock/tool-call infrastructure. This is an accepted workflow deviation: foreground implementation after the cap; reason: recover tooling failure; evidence: targeted test, clippy, and diff-check above; risk: it has not received an additional independent review round.
- Residual platform risk: `NOREPLACE` was executed on the current Apple host. Linux and Redox implementations were verified statically, not executed; unsupported targets fail closed.

## Outstanding follow-up / recovery

- No deferred functional follow-ups remain. An additional post-cap independent review is optional; the lack of one and the cross-platform execution boundary are recorded risks above.
