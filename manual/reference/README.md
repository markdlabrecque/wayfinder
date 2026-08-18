# Reference inventories

These mechanically checked inventories describe the shipped surface, not a promise
of general Solr compatibility. They are generated-maintained reference tables:
source declarations are the authority, and
`tests/manual_reference_contract.rs` rejects a source addition until its one
designated row is documented here.

- [Wire routes](wire-routes.md)
- [Route parameter allowlists](parameters.md)
- [Server configuration](configuration.md) and [CLI/environment](cli-and-environment.md)
- [Schema](schema.md) and [custom analyzers](analyzers.md)
- [Response and error envelopes](response-errors.md)
- [Extraction](extraction.md)
- [Drupal integration boundaries](drupal.md)
- [Compatibility boundaries](boundaries.md)
- [Troubleshooting](troubleshooting.md) and [glossary](glossary.md)
- [Evidence and provenance](provenance.md)

Canonical operational rules remain in [Compatibility](../../docs/COMPATIBILITY.md),
[Configuration](../../docs/CONFIGURATION.md), and
[Deployment](../../docs/DEPLOYMENT.md). The retained Solr response fixtures under
`solr-ref/responses/` are frozen provenance, not a source of additional scope.
