# File extraction reference

`/update/extract` accepts multipart file uploads for extract-only output or
indexing. Shipped extractors are plain text (including Markdown/CSV by MIME or
extension), HTML/XHTML, DOCX, PPTX, XLSX, ODT, ODP, ODS, RTF, and PDF.
Byte signatures win over declared MIME, which wins over `resource.name`/filename
extension; ZIP signatures are refined only into recognized office packages.
Legacy OLE (`.doc`, `.ppt`, `.xls`) and arbitrary ZIP are unsupported.
**Generic XML dispatch is unsupported**: an XML type/name can be detected but
has no production extractor.

Charset resolution is BOM, then a recognized declared charset, then bounded
detection; a resolved BOM is consumed. An unknown declaration falls through to
detection rather than failing. Invalid decoded sequences use replacement
characters. Empty output can be a successful 200 result, including image-only
PDFs and the known malformed-content-stream case that the PDF library swallows;
Wayfinder has no OCR to distinguish them. An encrypted PDF that cannot open
with the empty wire password is a parser failure (500). The HTML extractor
captures attributes but does not fetch linked resources; no extractor calls an
external service.

For indexing, source fields include extracted content/metadata and, by default,
captured HTML attributes. Processing order is `lowernames` (default `true`),
a merged `fmap` rename (request mappings override defaults `a→links` and
`div→ignored_`), `uprefix` filtering (default `ignored_` drops unresolved
fields), then a `literal.*` overlay. `captureAttr=false` removes captured
attributes before that pipeline. Literals are lowercased when enabled but are
not passed through `fmap`. With empty `uprefix`, an unknown field reaches normal
schema validation and returns 400. HTML links contain real attribute values
only—no fabricated attributes or recursively fetched content.

## Limits, statuses, and safe lifecycle

`max_body_bytes` is request-wide across multipart content; `max_inflight_uploads`
bounds intake separately from `max_concurrency` parsing; output has scalar and
byte budgets; deadline bounds extraction. ZIP/container and structural budgets
also reject unsafe expansion. Typical statuses: 400 malformed multipart,
missing file/mapping/charset/output violation; 413 body budget; 415 unsupported
format; 500 parser failure; 503 saturated intake/parser or deadline. Release a
saturated slot and retry a new upload; do not retry corrupt input unchanged.

**Prerequisites:** choose a supported local file, intended mapping, and a schema
that accepts its destination. **Visibility:** extraction-only changes nothing;
indexed output is pending until its commit mode. **Durability:** only a completed
commit makes indexed output restart-safe. **Failure/retry:** a rejected mapping
or budget does not make a complete indexed document; correct it and retry, but
reconcile any timeout by literal id. **Validation:** check extracted output or
query committed mapped fields/HTML attributes. **Rollback:** delete the indexed
whole document or restore a retained data set; discard temporary upload data.

Hermetic advanced shapes and safety budgets live in `tests/extract_route.rs`,
`tests/extract_index.rs`, `tests/extraction.rs`, `tests/html_extractor.rs`,
`tests/office_extractor.rs`, and `tests/pdf_extractor.rs`.
