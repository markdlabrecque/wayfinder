# Security and observability

Wayfinder is HTTP-only. Terminate TLS at a trusted reverse proxy and keep the
backend hop on loopback, a trusted private network, or an encrypted tunnel.
With `[auth]`, HTTP Basic protects the process except the deliberately public
core admin ping and `/ui/ping`; credentials are plaintext-equivalent without
TLS. Keep even public ping on a trusted network and treat it as shallow
synthetic health, not readiness for data, authorization, storage, or upstream
dependencies. Protect configuration files because they can contain credentials.

The UI is read-only except synonym replacement; serve it same-origin behind the
proxy and do not treat browser origin rules as authorization. Bind backend
ports privately and make proxy access policy explicit. Authentication failure
is a JSON 401 with `Basic realm="wayfinder"`; unknown cores must not reveal the
configured core.

## Logs and metrics boundaries

`RUST_LOG` controls structured logs to stderr. At detailed levels logs include
the full URI, so query strings, literals, and even secrets mistakenly placed in
a URI can be sensitive: scrub/redact at the proxy, restrict log readers, and
never put credentials in query parameters. UI stats and selected admin/system
metadata are diagnostic aids. MBeans gives selected runtime/index counters; mbeans reset with process restart
and are not a durable audit store.

There is no Prometheus endpoint, no OpenTelemetry exporter/collector, no
built-in TLS certificate lifecycle, no distributed health, and no alerting
system. Build metrics scraping, log shipping, SLOs, certificates, alert routes,
and dependency probes externally; do not claim the mbeans/UI payload is a
monitoring API.

**State-change lifecycle.** Prerequisites: test proxy/TLS/auth policy in a
private candidate and retain the prior config. Visibility: auth and log level
apply at startup; public ping remains intentionally visible. Durability:
configuration is durable only when retained with its deployment set; mbeans
state is not. Failure/retry: reject malformed credentials/config, correct them,
and retry startup; do not weaken access controls to diagnose. Validation: test
unauthenticated ping, rejected protected request, authenticated read, TLS at
the proxy, and URI redaction in the proxy or log-retention pipeline. Never put
credentials in request URIs: Wayfinder stderr can contain the full URI.
Rollback: restore the prior proxy/server configuration and restart or reroute.

Hermetic evidence: `tests/basic_auth.rs`, `tests/admin_mbeans.rs`,
`tests/admin_ui_index_stats.rs`, and `tests/ops_shutdown.rs`.
