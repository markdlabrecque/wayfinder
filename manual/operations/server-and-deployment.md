# Server and deployment

One process owns one core, one port, one schema, and one data directory. Never
share a writable data directory. Run additional cores as isolated processes
with separate ports and service identities. A reverse proxy can expose multiple
such backends, but preserve reverse proxy isolation: route each host/path to
one backend, keep each backend private, and do not make one data directory a
multi-core mount.

## Capacity and tuning

All server settings are startup settings. `writer_heap` is the writer arena
budget (Tantivy needs roughly 15 MB per writer thread); default one thread keeps
stable score tie-breaking, while bulk loads may raise it. `merge_policy=no_merge`
is for controlled bulk work; log parameters are ignored there. `rows_limit` and
`facet_limit_max` clamp. Document-store compression/block size apply only at
index creation, so later edits are inert until reindex. `time_allowed` and
`searcher_pool_size` are accepted but inert. Extraction has separate body,
intake, parse, output, and deadline budgets. Size resident upload memory from
`max_inflight_uploads × max_body_bytes`; plan parse concurrency, output limits,
and deadlines independently rather than treating them as one multiplier. Tantivy is mmap/page-cache based,
not a process heap-size knob.

## Native, systemd, and image lifecycle

Use the documented native invocation and dedicated unprivileged account. A
systemd service should preflight an intended `WAYFINDER_CONFIG`, set
`KillSignal=SIGTERM`, give a stop timeout, restrict filesystem writes to the
data directory, and preserve a private writable temporary directory. SIGTERM
stops intake, drains work, and commits acknowledged pending writes; SIGKILL is
recovery-only and can lose recent pending visibility.

The static `scratch` image has no shell, package manager, CA bundle, or repair
tools. Its runtime UID/GID needs mounted writable data and sticky `/tmp` for
multipart staging. For GHCR, pin and record an immutable multi-architecture
manifest-list digest; mutable tags are neither rollout nor rollback evidence.
Test the chosen digest on each target architecture.

**State-change lifecycle.** Prerequisites: capacity estimate, matching
schema/config/data rollback set, and private health path. Visibility: a config
or image change starts only with its new process; creation-only settings need a
fresh index. Durability: completed commits and the mounted data directory
survive process restart. Failure/retry: stop a failed candidate, inspect stderr,
fix it, and retry without touching live metadata. Validation: ping, an indexed
write, representative query/count, and resource behavior. Rollback: route
traffic to the retained process/image/schema/data set.

Deployment and shutdown behavior are exercised by `tests/ops_shutdown.rs` and
`tests/publish_public_multi_arch_contract.rs`; use `docs/DEPLOYMENT.md` for the
normative service and Compose material.
