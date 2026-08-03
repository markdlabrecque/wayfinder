# Issue #262 — file extraction tracer: end-to-end through search_api_wayfinder

Date: 2026-08-04
Branch: `markdlabrecque/issue-262-tracer-file-extraction`
Follow-up to: `docs/reports/2026-08-02-extract-only-tracer.md` (#258, the
`/update/extract` endpoint this slice drives) and
`docs/reports/2026-08-01-text-extraction-exploration.md` (#171, the epic).

## What shipped

The retained tracer bullet for #262: a thin vertical slice that ports
`search_api_attachments`' core behaviour into `search_api_wayfinder`, end to
end, on one file field — *file field → processor computed field →
`WayfinderClient` multipart POST → `/update/extract?extractOnly=true` →
extracted text indexed → found by a fulltext search*. Nothing more.

- **`WayfinderClient::extract(string $filepath): array`** — a multipart POST
  to `/update/extract`, next to `update()`/`select()`, built on the existing
  `request()` helper. Mirrors the wire shape capture.sh's `cap_extract` and
  Solarium's ExtractQuery produce: one part named `file` carrying the stream,
  with `extractOnly=true`, `extractFormat=text`, `resource.name=<basename>`,
  `wt=json`. Returns the full decoded envelope; the text lives under the `file`
  key (the part name, per #258 — not `resource.name`).
- **`WayfinderBackend::extractContentFromFile(string $filepath): string`** —
  mirrors `search_api_solr`'s same-named method and contract; pulls the text
  out of the response's `file` key. The processor reaches it through the
  index's server backend.
- **`FileExtraction` processor** (`src/Plugin/search_api/processor/`):
  `supportsIndex()` gates to Wayfinder-backend indexes; `getPropertyDefinitions()`
  declares one `saw_<field>` computed property per discovered file-typed field;
  `addFieldValues()` loads the files and populates the field via the backend,
  **catching per-file extraction failures and logging them so one bad
  attachment never fails the index batch** (the hard requirement from #262).
- **Integration harness** extended: a file field on the article bundle, an
  attachment whose text exists nowhere else in the corpus, the processor +
  fulltext index field wired up, and a query for the attachment-only token.

## Decisions this slice made (they are hard to change later)

1. **Field naming — `saw_` prefix** (search_api_wayfinder). Distinct from
   `search_api_attachments`' `saa_`, so both modules' properties coexist
   without colliding machine names. Running both against the same file fields
   would double-index attachment text, so a site should pick one module — the
   prefix makes that a choice, not a silent clash. The processor's own admin
   description says so explicitly.
2. **Extracted text lands in its own fulltext field with independent boost**,
   not appended to the body. Shared documents would otherwise dominate
   relevance; an own field lets each site boost attachments independently. The
   processor declares the property as `string` (matching prior art) and the
   site adds it to the index as `text`.

## Two cross-layer gaps the tracer surfaced (both fixed at the root)

A tracer bullet's job is to fire exactly these.

### 1. `DocumentBuilder` emitted `null`/`[]` for empty fields

Optional/computed fields with no value were put into the doc as `null`
(single-valued) or `[]` (multi-valued). Solr silently omits absent fields;
Wayfinder rejects `null` for a typed field
(`field ts_X expects a string value, got null`). This was latent because every
existing index always populated title/body. The extraction field is empty on
every item that has no attachment, so it surfaced here. Fix: omit any field
whose value set is empty (`src/DocumentBuilder.php`, +1 unit test). This is a
general correctness fix, not extraction-specific.

### 2. The release image (`FROM scratch`) had no `/tmp`

#257 streams the uploaded file to a `NamedTempFile` under `/tmp` before
parsing. The `scratch` final stage has no filesystem at all, so
`/update/extract` failed in the release image with
`No such file or directory ... /tmp/...`. The #258 differential harness never
caught this because it runs against a separate Solr container. Fix: the
Dockerfile provides an empty `/tmp` in the release stage (`RUN mkdir
/scratch-tmp` in the builder, `COPY --from=builder /scratch-tmp /tmp` in
scratch — ~0 bytes, image stays minimal). This makes the shipped image actually
support its own extraction feature.

## Scope deliberately deferred (documented follow-ups, not gaps)

Each is called out in the processor's class doc, not left implicit:
- **Wiring `ExtractFileValidator` (#264, landed during this PR's review).** #264
  shipped `isFileIndexable()` / `limitToAllowedNumber()` / `limitBytes()` as a
  standalone, config-decoupled class whose report explicitly says "#262's
  processor wires it." The immediate follow-up is to call it from
  `addFieldValues()` (owning the `file_exists()` / `isPermanent()` preconditions
  #264 left to the processor) once #266 lands the settings form that feeds it.
  Until then this tracer extracts every referenced file — including private
  ones — so production use should wait for that wiring, which carries #264's
  default exclude-private safety decision.
- Media (`entity_reference` → media → file) and the `entity:file` datasource
  case. Only plain `file`-typed fields are discovered in this slice.
- Extraction-result caching. Every reindex re-extracts; the Wayfinder server's
  own budgets bound the work, but a persistent cache is the obvious next slice.
- A fallback queue for transient extraction failures.

## Test coverage

- **Unit** (`vendor/bin/phpunit`, hermetic, the CI gate): 154 green (was 134).
  - `WayfinderClient::extract()`: success (fixture-derived), the exact multipart
    wire shape, non-200 error envelope, connect failure, unreadable file,
    auth headers.
  - `WayfinderBackend::extractContentFromFile()`: returns the `file`-key text,
    empty string on a key-less response, propagates `SearchApiException`.
  - `FileExtraction` processor: `supportsIndex` true/false/no-server; property
    declaration with the `saw_` prefix (asserted it is *not* `saa_`); empty for
    a specific datasource; `addFieldValues` populates, logs-and-skips on
    failure, no-ops when no index field references the property or the entity
    can't be loaded.
  - `DocumentBuilder`: omits empty-valued fields (the gap above).
  - **Mutation-tested** the two guards whose whole value is failing correctly:
    the batch-safety `catch` (rethrow → test errors) and the `supportsIndex`
    backend gate (swap the id → both gate tests fail).
- **Integration** (`WAYFINDER_INTEGRATION=1`, Docker, manual): **green,
  end-to-end**. 4 docs indexed; the attachment-only token
  `wayfinderattachment262` is found via the `file_content` fulltext field —
    `EXTRACT: PASS - file attachment text was extracted via /update/extract and
    found by fulltext search`. The existing `wayfinderroundtrip` round trip
    still passes.

## Prior art

`search_api_attachments` 10.0.x `FilesExtractor.php` (GPL-2.0-or-later, same
licence as this module) was read in full and ported deliberately, not copied:
the per-file-field computed-property + `addFieldValues()` shape and the
file-field discovery loop are adapted from it. The extraction itself delegates
to `WayfinderBackend::extractContentFromFile()` rather than an external
text-extractor plugin, because Wayfinder extracts in-process on the server.
