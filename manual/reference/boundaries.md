# Compatibility boundaries

Wayfinder retains a bounded Solr-shaped JSON wire for existing clients; it is
not a general Solr replacement or an ongoing parity roadmap. It runs one core
per process and uses TOML schema and server configuration, never Solr
configuration files.

Permanent unsupported boundaries include SolrCloud, ZooKeeper, distributed or
sharded search, streaming expressions, SQL, core-admin/configset lifecycle,
atomic updates, optimistic concurrency, XML/javabin/PHP response writers, OCR,
and external extraction services. Generic XML extraction dispatch is
unsupported even when a client labels an upload as XML.

The source allowlists accept only documented route parameters when
`strict_params=true`. Acceptance is not proof of complete Solr semantics:
consult [route parameter allowlists](parameters.md) for implemented,
constrained, inert, warning-only, and prefix-family status. The full normative
list of deliberate differences and unsupported boundaries is
[Compatibility](../../docs/COMPATIBILITY.md).
