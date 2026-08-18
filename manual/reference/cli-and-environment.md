# CLI and environment reference

The CLI operates one local Wayfinder installation. It does not administer Solr
cores, configsets, clusters, or remote services. For layout, shutdown, and
backup safety, use [Deployment](../../docs/DEPLOYMENT.md); for TOML settings,
use [Configuration](../../docs/CONFIGURATION.md).

## `wayfinder`

```text
wayfinder <schema.toml> <data-dir> [bind-addr]
```

**Prerequisites:** `schema.toml` is required and defines one core. `data-dir` is
required, must be owned exclusively by this process, and persists the index,
schema contract, and query synonyms. `bind-addr` defaults to `127.0.0.1:8983`. Its visibility begins when the listener binds. A successful start
only means the listener bound; validation requires `/wayfinder/{core}/admin/ping`, then a
representative committed query. Stop with SIGTERM to drain and flush pending
writes. If startup fails, do not repair by editing persisted state: correct the
candidate schema/configuration or restore the matching previous set.

## `wayfinder snapshot`

```text
wayfinder snapshot <live-data-dir> <fresh-destination-dir>
```

The source must be a live compatible data directory. The destination must not
exist and must support the required atomic no-replace publish behavior. The
command copies one committed Tantivy generation with persisted schema and
analyzer contract while writes continue; it excludes uncommitted writes and
`<data-dir>/synonyms.txt`. Thus it is an online index snapshot, **not a complete
backup**. Validate by restore-testing a representative query. On failure, do
not reuse or merge a partial destination; remove/inspect it under your backup
policy and retry with a fresh destination. Roll back a restore by retaining the
old full directory set. See [backup and recovery](../operations/deploy-and-recover.md#backup-restore-upgrade-rollback-and-disaster-recovery).

## `WAYFINDER_CONFIG`

`WAYFINDER_CONFIG` names the process-level `wayfinder.toml`. It is independent
from the schema argument. If unset, defaults apply; a nonexistent path currently
also selects defaults, while a present unreadable/invalid configuration fails.
Unknown keys fail. This behavior is constrained compatibility, not a safe auth
default: when authentication is intended, preflight that the named file is
readable before launching. Changes take effect at process startup, so preserve
the old config/data set, restart a candidate, validate ping/auth/index/query
behavior, and revert to the old set on failure. Every setting's lifecycle,
including **inert** accepted keys, is in the [configuration inventory](configuration.md).

## `RUST_LOG`

`RUST_LOG` configures the tracing filter; absent or invalid values fall back to
`info`. Logs go to stderr without ANSI escapes, so the supervisor/container
should collect and retain stderr. Logging changes are visible at startup; retry
a malformed filter by restarting with a valid value. It does not enable a metrics
exporter, remote collector, audit log, TLS, or request authorization. Validate
by observing startup/request/shutdown records and retain external observability
and access controls for those unsupported concerns.
