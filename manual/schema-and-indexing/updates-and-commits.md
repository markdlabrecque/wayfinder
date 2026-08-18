# Updates and commits

An update accepts a JSON document array or command object. Command objects
support ordered `add`, `delete` by id or query, and `commit`; repeated `add`
keys execute in body order. A whole-document add is replacement by unique key
when `overwrite=true` (the default); `overwrite=false` permits duplicate live
keys. Delete by id removes every live duplicate; deleting a missing id is a
successful no-op. Delete-by-query uses query analysis. Atomic field modifiers,
optimistic concurrency, `versions=true`, and stale-write conflicts are
unsupported—do not model partial document patches as updates.

Invalid JSON and malformed unknown or doc-less commands reject the complete body
before the execution loop, so they leave no partial prefix. Once document
indexing begins, a schema-invalid later document can fail after an earlier valid
prefix reached the writer: this is a **partial valid prefix**, not a transaction.
Autocommit may make that prefix visible and durable even when the request returns
400. Reconcile IDs/counts first, then retry only the corrected remainder; use
idempotent whole documents because an ambiguous transport failure can still
duplicate or replace data.

## Commit modes

No commit parameter leaves writes **pending**: acknowledged but not searchable
or guaranteed across a crash. `commit=true`, a body `commit`, and
`softCommit=true` synchronously make preceding writes **searchable and durable**;
in this implementation soft commit is the same hard commit/reload. `commitWithin` schedules
visibility after its window. `autocommit_max_docs` and `autocommit_max_time`
commit pending writes at their thresholds. A successful completed commit is
**durable**; SIGTERM also flushes acknowledged pending updates before clean exit.
An add after an in-body commit remains pending until another commit.

## Safe mutation lifecycle

**Prerequisites:** confirm the core, unique key, schema constraints, and an
idempotency/rollback plan; preserve the old document or complete backup.
**Visibility:** check the specific id after the chosen commit mode, never just
the 200 acknowledgement. **Durability:** restart or use the known commit path
before claiming persistence. **Failure/retry:** a validation 400 can retain a
partial valid prefix; query it before retrying. A timeout after submission is
ambiguous, so reconcile by id/count before another write. **Validation:** check
stored fields, expected count, and delete effects with an explicit query.
**Rollback:** restore prior whole documents or return to a retained data set;
there is no atomic partial-field rollback.

The detailed request forms and edge cases are hermetically exercised in
`tests/update_pipeline.rs`; shutdown durability is exercised in
`tests/ops_shutdown.rs`.
