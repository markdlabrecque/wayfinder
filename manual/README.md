# Wayfinder manual

Wayfinder runs **one process, one schema, one core, one data directory, and one
listener**. Its HTTP API uses bounded Solr-shaped JSON for the documented
routes; it is not Solr parity or a general Solr replacement.

Start with the executable [twenty-minute quickstart](getting-started/quickstart.md).
It uses the adjacent canonical [`schema.toml`](getting-started/schema.toml) and
deterministic [`corpus.json`](getting-started/corpus.json).

## Manual map

1. **Start and succeed:** [orientation](getting-started/orientation.md),
   [core concepts](getting-started/concepts.md), and the executable
   [quickstart](getting-started/quickstart.md).
2. **Build and populate a core:** [schema design](schema-and-indexing/schema-design.md),
   [field/analyzer details](schema-and-indexing/field-and-analyzer-reference.md),
   [updates and commits](schema-and-indexing/updates-and-commits.md),
   [file extraction](schema-and-indexing/file-extraction-reference.md), and
   [reindexing](schema-and-indexing/index-lifecycle.md).
3. **Search:** [query and relevance](search/query-cookbook.md),
   [search components](search/search-components.md), and
   [response contracts](search/response-contract.md).
4. **Integrate and operate:** [Drupal Search API](integrations/drupal-reference.md),
   [server/deployment](operations/server-and-deployment.md),
   [security/observability](operations/security-and-observability.md), and
   [backup/migrations](operations/backup-and-migrations.md).
5. **Reference:** [mechanically checked inventories](reference/README.md) for
   routes, parameters, configuration, schema, extraction, CLI, envelopes,
   troubleshooting, glossary, boundaries, and evidence provenance.

The four files in `docs/` remain normative. The manual curates them into user
journeys and links back rather than replacing their authority.
