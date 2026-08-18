# Operate, deploy, and recover a core

This guide curates operational decisions; [Deployment](../../docs/DEPLOYMENT.md)
and [Configuration](../../docs/CONFIGURATION.md) are normative. A core is one
process, one port, one schema, and one data directory. Multiple cores require
separate processes/ports/schemas/directories; never share a writable directory.

## Server tuning and lifecycle

All server settings are startup configuration. `writer_heap`, writer threads,
merge policy, row/facet clamps, body/extraction budgets, autocommit, auth, and
reported version have the effects in the [configuration inventory](../reference/configuration.md).
Document-store compression/block size apply only when an index is created;
changing them later is **inert** until reindex. `query.time_allowed` and
`resources.searcher_pool_size` are accepted but **inert**. Unknown config keys
are errors; a missing `WAYFINDER_CONFIG` path currently selects defaults, so an
operator intending auth must preflight readability outside Wayfinder.

For any tuning change, preserve a matching rollback set, apply it to a candidate
process, and validate ping, indexing, latency/resource behavior, and committed
query counts. Validation must complete before retiring the prior set. Its visibility begins only after the candidate starts (and index
creation-only settings only in a fresh directory). If startup fails, correct the
candidate and retry; roll back by restoring the prior process/configuration/data
set. Do not edit live persisted schema metadata.

## UI, authentication, and observability

`GET /ui`, `/ui/query`, `/ui/schema`, `/ui/stats`, and `/ui/ping` are read-only.
`GET /ui/synonyms` reads state; `POST /ui/synonyms` is the only UI mutation and
atomically replaces durable query synonyms. Serve the UI same-origin behind a
trusted reverse proxy; do not expose it cross-origin or rely on it as a CSRF/
authorization boundary. Basic auth is process-wide when configured, but `/ui/ping`
and core `/admin/ping` remain unauthenticated health checks. Keep both on a
trusted network and do not use health endpoints as a readiness proof for data.

Wayfinder is HTTP-only: terminate TLS upstream and keep the proxy hop on
loopback, private network, or an encrypted tunnel. Basic credentials are
plaintext-equivalent without TLS. Logs are structured through `RUST_LOG` to
stderr. Health is limited to ping routes; selected admin metadata/metrics are
listed in [wire routes](../reference/wire-routes.md). There is no built-in TLS
certificate lifecycle, metrics exporter, tracing collector, distributed health,
or alerting system; provide these externally.

## Native, systemd, and container deployment

Use the supported native binary invocation and hardened systemd ownership/
SIGTERM model in [Deployment](../../docs/DEPLOYMENT.md#install-and-run) and
[its systemd unit](../../docs/DEPLOYMENT.md#systemd). Run as a dedicated
unprivileged user with exclusive data-dir access. SIGTERM drains and commits;
SIGKILL is recovery-only and can leave recent pending writes unavailable.

The published GHCR image is multi-architecture only for the tags/digests
advertised by its registry manifest. Pin an immutable manifest-list digest in
production, record it with the schema/configuration and test it on each target
architecture; do not treat a mutable tag as rollback evidence. The repository
image is static `scratch`: it has no shell, package manager, CA bundle, or
interactive repair tooling, and UID/GID 65532 needs the documented sticky `/tmp`
and writable mounted data directory. Build/proxy/Compose details remain
normative in [Deployment](../../docs/DEPLOYMENT.md#docker-compose).

## Backup, restore, upgrade, rollback, and disaster recovery

An online `wayfinder snapshot` selects one committed index generation and copies
index, persisted schema, and analyzer contract to a **fresh** destination. It is
atomic as published, permits concurrent reads/writes, and deliberately omits
`<data-dir>/synonyms.txt`; uncommitted writes are absent. It is therefore not a
complete backup. A successful snapshot can be validated by opening/restoring it,
but cannot restore durable query-synonym state alone.

For complete backup, prerequisites are a maintenance window, a verified restore
destination, matching schema/server config/binary or pinned image digest, and
storage for the whole directory. Gracefully stop, copy the entire data directory
including `synonyms.txt` plus matching files, restart, and validate ping,
representative queries, and counts. Do not `cp`, `rsync`, or `tar` a live
writable directory; concurrent merge/metadata changes can create an
unrestorable copy. If stop/copy fails, keep the original untouched, correct the
failure, and retry; an interrupted backup is not valid.

For restore/DR, verify checksums first, gracefully stop, move the current
complete set aside as rollback, restore into a fresh directory with correct
ownership, then start and validate health/query/counts before deleting rollback.
Never merge a backup into an existing data directory. A schema mismatch refusal
is authoritative. During upgrade, retain old binary/image, schema, config, and
data as one set; use a fresh blue-green reindex whenever required, validate, and
switch traffic only then. On failure, route traffic back to the retained set.
The detailed canonical sequences are [Backup](../../docs/DEPLOYMENT.md#backup),
[Restore](../../docs/DEPLOYMENT.md#restore), [Reindex](../../docs/DEPLOYMENT.md#reindex),
and [Upgrade checklist](../../docs/DEPLOYMENT.md#upgrade-checklist).
