# Concepts and request architecture

Wayfinder is a **bounded compatibility contract**, not Solr and not a general
Solr replacement. It retains a JSON HTTP wire so existing clients can search a
single configured core. It has no Solr configuration files, SolrCloud,
ZooKeeper, distributed/sharded search, streaming expressions, SQL, or non-JSON
writers. Generic XML unsupported is a permanent boundary; XML, javabin, PHP, and other response
writers are not fallbacks.

## Definitions

A process has **one configured core per process**, schema, writable data
directory, and listening address. A **schema** maps input field names to static or dynamic
field definitions and analysis. A **document** is an entire JSON object written
through the update pipeline. A **pending** write is acknowledged but absent from
the searcher; a **searchable** write has committed and is visible to requests;
a **durable** write survives restart after the commit is persisted. A query is
read-only against that committed searcher view.

“Supported” means a documented, tested bounded behavior. “Constrained” means
accepted only for stated types/grammar. “Inert” means accepted but has no effect.
“Warning-only” means it is accepted while emitting a warning. “Unsupported”
means do not retry it as a parity request: choose a supported workflow instead.

## Request path

`client -> TLS/auth reverse proxy -> Wayfinder HTTP router -> core/schema ->
index or committed searcher -> JSON envelope`. The proxy owns public TLS and
network policy; Wayfinder receives HTTP. Authentication, if enabled, precedes
most routes; the configured core check prevents an arbitrary core name from
reaching the process's index. Parameter validation then applies the per-route
allowlist when `strict_params=true`.

Update and extraction requests enter one writer and become pending until a
commit mode makes a new searcher visible. Select and helper requests read that
searcher; administration/UI endpoints expose only their documented metadata.
A process boundary is therefore an isolation boundary: do not share a writable
data directory or pretend one process serves several cores.

## Choosing a next action

Before a state change, take a backup/rollback set and identify the schema,
core, and expected count. After the change, validate ping plus a representative
query/count; retry only a failed request whose outcome is known not to have
been applied. If a change is ambiguous, stop rather than duplicate it. Rollback
means returning traffic to the retained process/data/schema set, not editing
persisted Tantivy metadata.

For exact wire bounds and errors, use `docs/COMPATIBILITY.md`, the reference
inventories, and the hermetic integration tests—not an assumed Solr feature.
