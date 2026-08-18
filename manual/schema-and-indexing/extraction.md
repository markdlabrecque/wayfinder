# Extract and index uploaded documents

`POST /wayfinder/{core}/update/extract` accepts bounded multipart extraction.
Use `extractOnly=true` to inspect extracted content before indexing, then map
fields with the documented `literal.<field>` and `fmap.<from>` prefix families.
They are mapping controls, not XPath or generic XML support. Exact request
shapes and expected statuses are covered hermetically in
[`tests/extract_route.rs`](../../tests/extract_route.rs); use the
canonical quickstart corpus for routine JSON indexing.

## Detection, mapping, and limits

Supported detection is plain text, HTML, recognized OOXML/ODF containers, RTF,
and PDF. ZIP is refined only when it is a recognized office package. Generic
XML dispatch is unsupported even if an upload is named or declared XML;
legacy OLE, arbitrary ZIP, OCR, and external extraction services are also
unsupported. Consult the [extraction inventory](../reference/extraction.md) and
[configuration inventory](../reference/configuration.md) for transport,
concurrency, size, scalar, output-byte, and deadline limits.

Use `literal.<field>` for known metadata/value assignment and `fmap.<from>` for
an extraction output mapping only after the destination field is valid for the
schema. Unknown fields are rejected. A successful `extractOnly` response proves
only extraction, not index visibility.

## Safe upload lifecycle

**Prerequisites:** choose a supported format, validate multipart field mapping
against a staging core, size the upload and concurrency below configured limits,
and retain the source file. **Visibility/durability:** indexing follows the same
pending-write and commit rules as ordinary updates; request `commit=true` or
perform a later durable commit before relying on search or restart survival.

**Failure/retries:** 413 means a body/content limit, 415 means an unsupported
format, 500 is an extractor/parser failure, and 503 signals exhausted budgets or
a deadline. No status authorizes blindly retrying an unknown indexing result:
query by the unique ID, establish the accepted state, then retry an idempotent
whole replacement or corrected upload. **Validation:** inspect `extractOnly`
content, then verify stored fields and a search after commit. **Rollback:**
delete by the known ID and commit, or restore/reindex from the preserved source;
there is no OCR fallback or partial-document undo. Error envelopes are defined
in [response errors](../reference/response-errors.md).
