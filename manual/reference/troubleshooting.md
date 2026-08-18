# Troubleshooting index

| Symptom | Check | Safe response |
|---|---|---|
| Authentication unexpectedly disabled | `WAYFINDER_CONFIG` was absent or its path did not exist, so defaults were selected. | Use the native, systemd, or Compose readability preflight in [Deployment](../../docs/DEPLOYMENT.md) before startup. |
| A request is 400 only with `strict_params=true` | The name is not allowlisted, or is accepted but has a bounded value form. | Check [route parameter allowlists](parameters.md); do not add Solr parameters speculatively. |
| Sort or facet fails | The field lacks a compatible fast field. | Add `fast = true`, create a fresh data directory if required, and replay the authoritative corpus. |
| New schema does not start against old data | Persisted schema or analyzer contract is incompatible. | Follow the startup error; use a fresh directory and reindex rather than editing index metadata. |
| Extraction is 413, 415, 500, or 503 | A resource ceiling, unsupported format, parser failure, or exhausted/deadline budget occurred. | Check [extraction boundary](extraction.md) and server limits; generic XML is unsupported. |
| A container extraction fails under the Compose user | The data mount or scratch-image `/tmp` is not writable by UID/GID 65532. | Create/chown the data directory for 65532 and use the repository image with its `01777` `/tmp`. |
| Synonym change is absent after restore | An online snapshot omitted `synonyms.txt`. | Restore a graceful stopped whole-directory backup for complete durable state. |
| Drupal autocomplete fails | Stock Search API Solr calls unsupported `/autocomplete`. | Use the supported bounded terms/suggest path; see [Drupal boundaries](drupal.md). |

For errors returned by the wire, see [response and errors](response-errors.md).
