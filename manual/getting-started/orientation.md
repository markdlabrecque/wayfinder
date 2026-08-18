# Orientation and request path

Wayfinder is a single-core search service: one process owns one schema, one data
directory, and one listener. It preserves a **bounded** Solr-shaped JSON wire for
existing clients; it is not Solr, a general replacement, or a Solr-parity
roadmap. The normative boundary is [Compatibility](../../docs/COMPATIBILITY.md);
the complete route and parameter inventories are [wire routes](../reference/wire-routes.md)
and [parameters](../reference/parameters.md).

## Concepts

A **schema** defines one core's fields and analyzers. An **index** is the
on-disk Tantivy data built under that schema. A document update is pending until
a commit makes it searchable and durable. A request names that core in its
`/wayfinder/{core}/...` path; a core name is not a switchable Solr core.

```text
client -> reverse proxy/TLS (operator supplied) -> HTTP listener
       -> route + parameter validation -> query/update/extraction handler
       -> schema/analyzers -> Tantivy data directory -> JSON response
```

The operator UI is Wayfinder-owned and outside that wire. Its route mutability
and synonym-write boundary are documented in [operations](../operations/deploy-and-recover.md#ui-authentication-and-observability).

## Choose a safe first path

Use the executable [quickstart](quickstart.md) and its canonical corpus for
routine start, index, commit, query, and restart examples. It is exercised by
[`tests/manual_examples.rs`](../../tests/manual_examples.rs). Before changing a
schema or data directory, read [Configuration](../../docs/CONFIGURATION.md) and
[Deployment](../../docs/DEPLOYMENT.md): a schema is persistent state, not a
per-request option.

Supported means an implemented bounded behavior. **Constrained** means only the
documented subset works. **Inert** means accepted but has no effect.
**Warning-only** means it is accepted only to report its limit. **Unsupported**
means do not retry with Solr configuration or another response writer; select a
documented alternative or change the integration. These labels are assigned per
parameter in the [parameter inventory](../reference/parameters.md).
