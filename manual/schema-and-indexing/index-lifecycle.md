# Index updates, visibility, and reindexing

The update route handles whole-document adds, delete by ID or query, and commit;
it does not implement atomic field modifiers, optimistic concurrency, versions,
or stale-write conflicts. See [Compatibility](../../docs/COMPATIBILITY.md) and
the [route parameter inventory](../reference/parameters.md) for the bounded
update grammar.

## Replace and delete documents

An add with the unique key replaces the prior whole document when `overwrite`
is enabled; send the complete replacement, not a patch. Delete by ID is the
narrowest destructive operation; delete-by-query is supported but can remove
more documents than intended. There is no partial-update recovery or conflict
detector.

**Prerequisites:** know the unique key, capture a complete backup or source
record, and test the selector on a non-production-like copy. **Visibility and
durability:** an update is pending until explicit `commit=true`, a commit request,
or an autocommit threshold. `softCommit` makes pending work searchable but is
not the durability promise; `commitWithin` is bounded scheduling, not a
transaction acknowledgement. **Failure/retry:** transport failure leaves the
outcome unknown; retrying a whole replacement is generally safe only when the
same ID/body is intended, while retrying a delete-by-query can compound later
changes. **Validation:** query by ID and compare counts after a durable commit.
**Rollback:** re-add the saved complete document or restore the prior index;
there is no undo endpoint.

## Commit and partial valid prefixes

Updates are processed as a sequence. A malformed later document can yield a
failure after an earlier valid prefix has been accepted; do not assume batch
atomicity. A commit exposes and durably persists accepted pending writes. On a
failed upload, inspect the JSON error envelope, query/commit to establish the
accepted prefix, then retry only the remainder after correction. Validate using
a deterministic ID/count query; restore saved source documents if the accepted
prefix was unintended. The [response/error inventory](../reference/response-errors.md)
defines the HTTP and JSON failure shape.

Autocommit document/time thresholds are supported server settings; unset means
that trigger is disabled. Their lifecycle and the deliberately **inert**
`query.time_allowed` and `resources.searcher_pool_size` are in the
[configuration inventory](../reference/configuration.md). Do not treat accepted
parameters as a promise of full Solr semantics.

## Blue-green reindex for schema changes

Static field additions, removals, type changes, first/last dynamic rules, and
changes to/from spatial, date-range, or payload types require a fresh data
directory and reindex. Copy/index-analyzer changes affect only new content and
may also require it; the startup analyzer-contract check is authoritative.

**Prerequisites:** preserve an old binary/schema/configuration/data set, provision
an empty owned directory, and have a complete source reindex feed. **Workflow:**
start a candidate process on a different port and data directory, index all
source data, commit, and validate health, representative queries, and counts.
**Visibility/durability:** the old endpoint remains authoritative until callers
are switched; the candidate is durable only after commit. **Failure/retry:**
keep the old process/data untouched, fix the candidate schema/feed, discard the
candidate directory, and rerun. **Rollback:** repoint callers to the old
endpoint and retain it until validation passes. Never force a changed schema
onto existing persisted metadata. The canonical migration rules are in
[Configuration](../../docs/CONFIGURATION.md) and the operator procedure in
[Deployment](../../docs/DEPLOYMENT.md).
