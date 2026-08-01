# Report: operations runbook (issue #233)

- Branch: `233-deployment-backup-restore-reindex-runbook`
- Durable goal / approved spec: provide a docs-only operator runbook covering deployment layouts, systemd, Docker Compose, security boundaries, safe backup and restore, Drupal Search API reindexing, and upgrades.

## Delivered behavior and files

- `README.md` links operators to the new runbook.
- `docs/operations.md` documents one Wayfinder core per process, directory layout and permissions, systemd and Compose deployment examples, TLS/Basic-auth and public-health-route boundaries, stop-copy-start backups, restore with checksum validation and rollback, Drupal reindexing, and upgrade/rollback guidance.
- The backup guidance explicitly prohibits ordinary recursive copies of a live writable data directory and requires a restore test.

## Accepted correction

The issue premise said to run one Wayfinder process per Drupal Search API index. Captured/current behavior requires **one process per Wayfinder core** instead: multiple Search API indexes may share one core and remain separate through `index_id`. Separate processes are recommended only where failure, resource, schema, or backup boundaries must be separate. This correction is accepted because it matches the implemented backend behavior; the risk is that operators who require isolation must deliberately provision separate cores/processes.

## Backup evidence

Using `target/debug/wayfinder` under 30 committed batches of 80 documents with 4 KiB bodies:

- 20 live ordinary recursive copies produced 13 restorable copies and 7 traversal failures.
- A controlled metadata-last copy still failed reopening because `meta.json` referenced a missing `.term` file.
- After graceful `SIGTERM`, an offline copy restored all 2,560 documents.

This establishes that a live directory traversal is not a safe backup protocol. Issue [#239] tracks safe online snapshots.

## Verification

- Backup/restore experiment above: results as recorded above.
- Shell syntax checks on both runbook shell snippets: passed.
- ShellCheck on both runbook shell snippets: passed.
- `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`: exit 0; 929 tests passed, 0 failed.

The cited final cargo gate was run on the current tree. No child-run gate is cited, so no sanitized `PI_SUBAGENT_*`/`PI_WORKFLOW_*` child-process evidence applies.

## Review history and verdict

- **Round 1 — must-fix findings:** checksum manifest included itself and had no verification step; the backup procedure could leave the service stopped on failure and restore did not roll configuration back; authenticated-query verification guidance was missing. All were fixed.
- **Round 2 — review cap reached, fixes requested:** retain restore rollback through select verification including the authenticated branch; state the checksum exclusion exactly; arm backup restart handling before stopping the service. The foreground fixed all three and independently reran shell syntax and ShellCheck on both snippets.
- The cap is recoverable rather than approval. The final foreground verdict is **mergeable** based on the fixes and final current-tree gate; the round-two reviewer did not issue a separate post-cap approval.

## Follow-ups and unresolved risks

- **#239 — safe online snapshots:** intentionally separate future feature, not unresolved scope in #233. Until it exists, backups require a graceful stop and offline copy, or a storage snapshot with an independently validated whole-directory point-in-time guarantee.
- No other follow-ups or unresolved findings remain. The inherent operational risk of a maintenance window for portable backups is documented rather than eliminated.
