# Backup, migrations, and disaster recovery

A complete recovery set is the data directory, matching schema, server
configuration, and binary version or pinned image digest. Include durable
`<data-dir>/synonyms.txt`. Encrypt backups, control access, enforce a documented retention schedule, and
copy completed verified sets off-host. Encryption and
off-host storage are operator responsibilities, not Wayfinder features.

`wayfinder snapshot` is an online point-in-time copy of one committed Tantivy
generation, persisted schema, and analyzer contract to a fresh destination. It
allows live reads/writes and rejects an existing destination. Its **snapshot
omission** is deliberate: it omits `synonyms.txt`; uncommitted writes are also
absent. It is useful for index recovery validation, but is not a complete
backup. Never use `cp`, `rsync`, or `tar` against a running writable directory;
merges can make the copy unrestorable. An atomic storage snapshot is acceptable
only when it covers the whole directory and is restore-tested.

## Complete backup and restore

For a complete backup, gracefully stop the service, copy the entire data
folder plus matching configuration/schema, calculate and verify checksums, then
restart and validate ping and a representative query/count. A failed or
interrupted copy is not a backup: leave the source untouched, fix storage or
permissions, and retry. Test restoration regularly on isolated infrastructure;
a successful backup command is not a DR drill.

For restore, verify checksums before change; stop the live service; move the
current complete set aside as rollback; restore into a fresh directory with
correct ownership; restore its matching configuration/schema/version; start;
and validate ping, representative results, and document counts. Never merge
into an existing data directory. Startup schema/analyzer refusal is
authoritative. Rollback is routing or restoring the retained current set before
deleting it.

## Upgrade, reindex, and DR lifecycle

Prerequisites for an upgrade are release-note review, a tested backup, old
binary/image/schema/config/data held as one rollback set, capacity for a fresh
index when required, and a rehearsed off-host restore. Visibility: a reindex is
searchable only after its commits and traffic switch; old data remains visible
until that switch. Durability: preserve committed data and all side files,
including synonyms. Failure/retry: if a schema/analyzer migration fails, create
a fresh directory and reindex rather than bypassing the guard; retry failed
indexing after reconciliation. Validation: perform restore drills, checksum
checks, health, representative queries, per-index counts, and failover tests.
Rollback: switch traffic back to the retained old complete set.

`tests/online_snapshot.rs` verifies online committed-generation behavior;
`tests/ops_shutdown.rs` verifies clean shutdown durability. The normative
procedures remain `docs/DEPLOYMENT.md`.
