# First-class document text extraction exploration (#171)

Date: 2026-08-01
Scope: PRD decision, client evidence, Solr wire capture, dependency survey, and implementation recommendation. No Wayfinder handler or production dependency was added.

## Decision

Reverse the old Tika/`/extract` non-goal, with a narrower meaning than “embed Tika”:

- **First-class means no runtime service dependency.** Extraction runs in the Wayfinder process using permissively licensed Rust crates. There is no JVM, Tika server, sidecar, or separately operated daemon.
- **It does not mean no third-party code.** Hand-writing PDF, Office, ODF, and RTF parsers would be a separate multi-year product and is inconsistent with Wayfinder's existing use of Tantivy, Axum, and Serde.
- **OCR is out of scope.** A scanned document with no text layer may return empty text.
- **Resource controls precede parsers.** PDF is split out because an HTTP timeout cannot cancel opaque blocking parser work in-process.

`docs/PRD.md` records this reasoning and the staged roadmap.

## Client evidence

The 28 committed Search API traces contain no `/update/extract` request. The capture site did not have an attachment extractor configured, so trace absence is not evidence that the client cannot emit it.

The vendored `search_api_solr` 4.4.0 source has a direct path:

- `SearchApiSolrBackend::extractContentFromFile()` (`coverage/search_api_solr_4.4.0_source/src/Plugin/search_api/backend/SearchApiSolrBackend.php:4677`) obtains an Extract query, sets `extractOnly=true`, selects XML or text, attaches the file, and executes it.
- `SolrConnectorPluginBase::extract()` (`.../src/SolrConnector/SolrConnectorPluginBase.php:1350`) sends that query with the indexing timeout.
- `getContentFromExtractResult()` consumes the resource-name entry and ignores metadata. Solarium's Extract `Result::getData()` supplies that entry: since Solr 8.6 returns the generic raw key `file`, it aliases `file` to the query's `resource.name` (and does the same for `file_metadata`) for cross-version compatibility. Thus the captured `file` key is the real client wire response, not a mismatch with the connector's basename lookup.
- The captured Search API configset already defines `/update/extract` with `lowernames=true`, `uprefix=ignored_`, `captureAttr=true`, `fmap.a=links`, and `fmap.div=ignored_` (`solr-ref/search-api/configset/solrconfig_extra.xml:83`).

The client-evidenced first slice is therefore **multipart upload + `extractOnly=true`**, not server-side indexing. `literal.*`/`fmap.*` indexing remains useful Solr compatibility, but it should follow the evidenced response path rather than lead it.

## Captured Solr 9.10.1 wire contract

Capture environment: a freshly recreated, version-pinned `solr:9.10.1` container with `SOLR_MODULES=extraction`, a Search-API-shaped `ExtractingRequestHandler`, and a separate `extract171` core. Reproduction is appended to `solr-ref/capture.sh`; it fails closed on readiness/setup errors and unexpected HTTP statuses. Tiny payloads live in `solr-ref/extract-inputs/`.

| Fixture | Request/result |
|---|---|
| `extract_plain_text_xml.json` | Multipart `POST`, `extractOnly=true`; HTTP 200. Default extraction format returns an XHTML string under `file`. |
| `extract_plain_text_text.json` | Adds `extractFormat=text`; HTTP 200. The same key contains Tika's plain-text rendering. |
| `extract_html_index.json` | Adds `literal.id`, `fmap.content=body`, and `commit=true`; HTTP 200 with only `responseHeader`. |
| `extract_html_select.json` | Proves the indexed document received the literal ID, mapped extracted content in `body`, and captured link attributes in `links`. |
| `extract_corrupt_pdf.json` | Malformed PDF; HTTP 500 in Solr's normal error envelope. |

Observed contract:

1. A successful extract-only JSON body is `{responseHeader, file, file_metadata}`. The multipart part is named `file`, and that part name—not `resource.name`—is the result key.
2. `file_metadata` is a flat Solr NamedList represented as an alternating JSON array. Each metadata value is itself an array because keys may repeat. `resource.name` appears as metadata but does not rename the top-level `file` key.
3. The default format is XHTML/XML. `extractFormat=text` is not a trimmed body: leading/trailing newlines are part of the captured value.
4. The indexing path accepts `literal.<field>` and `fmap.<from>` in the query string and returns only `responseHeader`; indexed output is verified by a separate select.
5. Handler defaults matter. With `captureAttr=true` and `fmap.a=links`, an HTML anchor contributes captured attribute values to `links`; `fmap.content=body` maps extracted body text.
6. A corrupt PDF returns HTTP 500 with `responseHeader.status=500`, `error.code=500`, metadata naming the Solr/root exception classes, and a free-text parser message/trace. Code and envelope are contractual; Java object IDs and stack text are not.

The multipart captures intentionally stay out of `manifest-errors.tsv`: its runner only models JSON request bodies, and Wayfinder has no extraction route to compare yet. `capture.sh` contains the exact commands and an explanation. The implementation issue should extend the runner and add self-expiring expected divergences before changing that status.

## Dependency evaluation

Versions and repository activity were checked on 2026-08-01 through crates.io and GitHub. All recommended candidates are pure Rust and permissively licensed.

| Area | Candidate | Version checked / license | Maintenance evidence | Recommendation |
|---|---|---|---|---|
| Charset detection | `chardetng` + `encoding_rs` | 1.0.0, Apache-2.0 OR MIT; 0.8.35, `(Apache-2.0 OR MIT) AND BSD-3-Clause` | `chardetng` updated 2026-03-30; `encoding_rs` is mature and widely depended on | Use together. `encoding_rs` decodes but does not detect. Prefer declared BOM/charset before detection. |
| HTML | `html5ever` tokenizer | 0.39.0, MIT OR Apache-2.0 | Updated 2026-03-13; Servo project | Pilot its incremental tokenizer with a budgeted sink that retains only text plus narrow head metadata. Token/input/output counters can stop before building an unbounded DOM. Exclude script/style/template. |
| HTML alternatives | `scraper`; `html2text` | 0.27.0, ISC; 0.17.1, MIT | Both active in 2026 | Do not lead with either: `scraper` builds an opaque DOM before Wayfinder can budget its walk, while `html2text` adds presentation-oriented wrapping and does not expose the metadata model needed here. |
| ZIP container | `zip` | 8.6.0 stable, MIT | Active `zip-rs/zip2`; 9.0 prereleases exist | Use with default features disabled and only required compression methods. Enforce entry count, per-entry/cumulative output, and compression-ratio budgets before reading entries. |
| XML | `quick-xml` | 0.41.0, MIT | Updated 2026-06-29; high adoption | Use streaming events and explicit package-part allowlists. Do not deserialize whole untrusted documents. |
| Spreadsheets | `calamine` | 0.36.1, MIT | Updated 2026-07-27; supports XLS/XLSX/XLSB/ODS | Use for XLSX and ODS because shared strings, typed cells, formulas, and sheet semantics are more complex than generic XML text walking. Its README's “simple enough” caveat requires a representative fixture corpus. |
| RTF | `rtf-parser` | 0.4.3, MIT | Updated 2026-06-11; implements RTF 1.9 and exposes `to_text()` | Pilot with default features disabled (avoid unused WASM bindings). Add malformed-input and deeply nested group tests before accepting it. |
| PDF | `pdf-extract` over `lopdf` | 0.12.0 / 0.44.0, MIT | Both updated in June/July 2026; `pdf-extract` includes CMap/font helpers | Best initial PDF candidate, not yet approved. Evaluate against born-digital PDFs with embedded/subset fonts, ToUnicode CMaps, ligatures, columns, encrypted files, and malformed objects. `lopdf` alone is a PDF object parser, not a text extractor. |
| DOCX alternative | `docx-rs` | 0.4.22, MIT | Active, but describes itself as a DOCX writer | Reject for extraction. A bounded `zip` + `quick-xml` reader over known parts is smaller and more auditable. |
| Generic office alternative | `dotext` | 0.1.1, MIT | Last release 2017 | Reject as unmaintained and too broad to trust on untrusted uploads. |

### Format matrix and order

| Phase | Formats | Extraction strategy | Metadata promise |
|---|---|---|---|
| 0 | All | Extractor interface, MIME/signature dispatch, byte/concurrency/output budgets, bounded blocking execution, error taxonomy | `resourceName`, detected content type |
| 1 | Plain text, HTML | Charset decode; incremental HTML5 token sink with input/token/output budgets | title/author where explicit |
| 2a | DOCX, PPTX | Bounded ZIP + streaming XML over known document/slide/core-properties parts | title, author, created when present |
| 2b | XLSX, ODS | `calamine`, sheet/row/cell separators specified by fixtures | workbook properties where reliable |
| 2c | ODT, ODP, RTF | Bounded ZIP + XML; `rtf-parser` pilot | same narrow normalized set |
| 3 | PDF | Separate issue and corpus; `pdf-extract` candidate | PDF info dictionary/XMP only after conflict rules are specified |

Legacy binary DOC/PPT are not in the issue's requested matrix and have no credible pure-Rust candidate from this survey. XLS is covered by `calamine`; unsupported legacy formats should return a captured error rather than be mistaken for OOXML.

## Resource-limit decision

Minimum controls for phase 0:

- maximum HTTP body bytes while streaming upload to a temporary file;
- extraction concurrency semaphore and dedicated bounded blocking pool;
- maximum extracted Unicode scalar/byte count;
- wall-clock deadline reported to cooperative extractors;
- ZIP entry count, path validation, per-entry bytes, cumulative uncompressed bytes, and compression ratio;
- format-specific structural limits (XML depth/events, spreadsheet sheets/cells, RTF groups/tokens, PDF pages/objects where exposed).

`tokio::time::timeout(spawn_blocking(...))` is insufficient: dropping the join handle does not stop the parser thread. Plain-text decoding is cooperatively chunked. HTML must use an incremental tokenizer and a Wayfinder-owned sink whose input/token/output counters can abort before constructing a DOM; using `scraper` would violate this requirement because its parse precedes the budgeted walk. PDF must not ship until the selected library exposes enough checkpoints, is accepted with a documented bounded-concurrency residual risk, or the architecture decision is revisited. A separate daemon remains out of scope.

## Recommended follow-up issues

1. **Extraction budgets and internal extractor contract** — phase 0, including tests that deliberately break each limit and prove the guard catches it.
2. **`/update/extract` tracer: Search API extract-only plain text + HTML** — multipart parser, client-shaped XML/text envelopes, stable minimal metadata, and Solarium-compatible fixture tests.
3. **Solr Cell indexing semantics** — `literal.*`, `fmap.*`, `uprefix`, `captureAttr`, and update-pipeline integration; only after the evidenced client slice.
4. **Office/ODF/RTF extraction** — split further if the retained tracer shows shared ZIP/XML infrastructure is stable.
5. **PDF extraction corpus and cancellation decision** — independent acceptance gate; no OCR.

## Verification performed

- `gh issue view 171 --json ...` — acceptance and open questions inspected.
- Source/config grep and targeted reads — direct client emission path confirmed; no request in committed traces.
- One-off Docker capture against `solr:9.10.1` with `SOLR_MODULES=extraction` — four 200 responses and one corrupt-PDF 500 captured.
- crates.io API, `cargo info`, and GitHub repository metadata — versions, licenses, update dates, and project status checked.
- No production code or `Cargo.toml` change was made.

Review verdict: exploration acceptance criteria are satisfied. Remaining work is deliberately split into implementation issues; the principal unresolved risk is hard cancellation of opaque in-process parsers.
