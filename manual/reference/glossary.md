# Glossary

- **Core** — one configured schema and data directory served by one process.
- **Data directory** — Tantivy index, persisted schema/analyzer contract, and
  durable `synonyms.txt` state.
- **Fast field** — Tantivy columnar field required by supported sorting and
  faceting operations.
- **Stored field** — field retrievable in response documents through `fl`.
- **Analyzer** — tokenizer plus ordered filters used at index time or query
  time.
- **Commit** — makes pending writes searchable and durable.
- **Online snapshot** — committed index/schema/analyzer-only copy; not a
  complete backup because it omits `synonyms.txt`.
- **Complete backup** — graceful stopped whole-directory copy plus matching
  schema and server configuration.
- **Retained wire** — the bounded Solr-shaped JSON routes, distinct from the
  Wayfinder-owned `/ui` routes.
- **Prefix family** — an allowlisted parameter prefix such as `literal.` which
  admits keys below that prefix.
