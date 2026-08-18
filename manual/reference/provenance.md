# Evidence and provenance

The four canonical documents — [Compatibility](../../docs/COMPATIBILITY.md),
[Configuration](../../docs/CONFIGURATION.md),
[Deployment](../../docs/DEPLOYMENT.md), and
[Development](../../docs/DEVELOPMENT.md) — are normative. This manual curates
those rules; it does not replace them.

The route, parameter, configuration, built-in-type, and analyzer inventories
are checked against source declarations by
[`tests/manual_reference_contract.rs`](../../tests/manual_reference_contract.rs).
They intentionally fail when source declarations gain an undocumented row.

Frozen JSON in [`solr-ref/responses/`](../../solr-ref/responses/admin_info_system.json)
is regression evidence for the shipped wire. Expected values come from those fixtures, never
from implementation output; fixtures do not create new product scope.
Historical captures and client observations in
[`solr-ref/FINDINGS.md`](../../solr-ref/FINDINGS.md) remain supporting evidence.
Current scope and deliberate differences belong in the canonical Compatibility
document.
