# Issue #261 — PDF extraction corpus and cancellation decision

Date: 2026-08-03
Branch: `markdlabrecque/issue-261-pdf-extraction-corpus`
Follow-up to: `docs/reports/2026-08-01-text-extraction-exploration.md` (#171
exploration), and the phase-0 budget substrate that shipped in #257
(`docs/reports/2026-08-02-extraction-phase-0.md`).

## Decision: GO

`pdf-extract` 0.12.0 (over `lopdf` 0.42) is the PDF extractor. It is good
enough on born-digital text and — the question this issue existed to answer —
**it exposes the per-page and per-character checkpoints the phase-0
cancellation contract requires**, which #257 explicitly named as the upgrade
path ("either a parser that exposes per-page checkpoints, or move extraction
into a separate OS process"). The residual risk is small, bounded, and
documented below; the in-process architecture decision from #171/#257 stands.

The blocker (#257, the budget substrate) is closed; `src/extract.rs` already
owns `Budget::count_pdf_page()` (cumulative, no decrement) and the dedicated
`ExtractionRuntime` worker pool whose burnt-slot containment is the whole
backstop. The follow-up **implementation issue** is specced at the end.

No production dependency was added by this exploration. `pdf-extract`/`lopdf`
were evaluated in a throwaway crate under `/tmp` (not in the repo); the only
repo changes are the corpus (`solr-ref/extract-inputs/pdf-*.pdf`), the Tika
ground-truth fixtures (`solr-ref/responses/extract_pdf_*.json`), and the
`capture.sh` block that reproduces them.

## How the three questions were answered

A born-digital corpus of 8 small redistributable PDFs exercises each
dimension the issue named (subset fonts + ToUnicode CMaps, ligatures,
multi-column layout, multi-page, the Info-vs-XMP metadata conflict, AES
encryption, malformed objects, and an image-only "scanned" page). For each, a
Tika/Solr fixture was captured as ground truth and the same bytes were fed to
`pdf-extract`. The side-by-side is the evidence; no claim rests on
documentation alone.

| Corpus file | Exercises | Tika (solr:9.10.1) | `pdf-extract` 0.12 | Verdict |
|---|---|---|---|---|
| `pdf-embedded-font.pdf` | subset font + ToUnicode CMap, accented Latin | `The café résumé shows naïve effrontery, 42 times.` | identical | **match** |
| `pdf-ligatures.pdf` | OpenType `ffi`/`fi`/`fl` → multi-char ToUnicode | `The efficient office staff affixed fluffy findings.` | identical | **match** |
| `pdf-multicolumn.pdf` | 2-column reading order | left column fully before right; `\n\n` between | same order; single `\n` between | **match (whitespace divergence only)** |
| `pdf-multipage.pdf` | 4 pages, per-page checkpoint | 4 pages, `\n\n\n\n` between | 4 pages, no separator between | **match (separator divergence only)** |
| `pdf-metadata-conflict.pdf` | Info dict ≠ XMP (title/author) | 200; `dc:title=title=Info Dict Title` | text match; `get_info` is Info-only | **aligned** (see Q3) |
| `pdf-encrypted.pdf` | AES-128 (V=4), no password (the wire shape) | 500 `InvalidPasswordException` | 500 `decryption error: incorrect password` | **match** on the wire path |
| `pdf-malformed-objects.pdf` | valid header/xref/trailer, corrupted content stream | 500 `DataFormatException` | **200 empty** (swallows the error) | **divergence** (see below) |
| `pdf-image-only.pdf` | raster page, no text layer | 200, empty body | 200, empty body | **match** (scanned PDF → empty is correct) |

### Q1 — Parser quality: good enough, with one real divergence

The hard cases — a subset font whose content stream is non-ASCII glyph IDs
recoverable **only** through the ToUnicode CMap, and OpenType ligatures whose
single glyph maps to a multi-character Unicode string — both round-trip
**exactly** to what Tika produced. Multi-column and multi-page preserve
reading/page order; the only differences are separator whitespace
(single-vs-double newline between columns, none-vs-triple between pages),
which is a normalisation detail for the renderer, not an extraction defect.
An image-only "scanned" page returns empty from both, which is the correct
no-OCR behaviour the issue requires.

The two divergences worth naming plainly:

1. **Malformed content streams are swallowed as empty output.** Where Tika
   faults with `java.util.zip.DataFormatException` and returns the 500
   envelope (same shape as `extract_corrupt_pdf.json`), `pdf-extract` returns
   `Ok("")` — it treats an unfilterable stream as "no text" rather than an
   error. For the Drupal search use case that means a corrupt PDF would be
   indexed as an empty document (invisible in search) instead of surfacing as
   a server error. This is a behavioural divergence to handle deliberately in
   the implementation: a `pdf-extract` PDF extractor cannot rely on the
   parser to distinguish "empty document" from "broken document." (The
   phase-0 mutation-testing rule applies: whatever guard covers this needs a
   deliberate-break test.)

2. **Unchecked `unwrap`/`expect` throughout the content-stream interpreter.**
   `pdf-extract` panics on structurally-unexpected input — e.g. a `Do`
   operator whose `/Resources/XObject` is absent hits
   `get(&doc, resources, b"XObject").expect("XObject")`. In isolation this is
   bad; against the Wayfinder architecture it is **contained**: `spawn_extraction`
   wraps the closure in `std::panic::catch_unwind`, so a parser panic becomes
   `ExtractError::Parse("extraction panicked")` → HTTP 500, never a process
   crash and never a shared-pool burn. So the practical shape is "malformed
   PDF → 500", which is *closer* to Tika than the silent-empty case above, not
   worse. The burnt-slot invariant from #257 holds: a panic unwinds and the
   worker returns its permit.

### Q2 — Cancellation: GO, proven, not just asserted

This is the question the issue was split out to answer, and it is where
`pdf-extract` clears the bar that an opaque parser would not.

`pdf-extract` exposes **`output_doc_page(doc, output, page_num)`**, which
interprets exactly one page's content stream (the `lopdf` `Document` — xref +
object table — is parsed once by `Document::load_mem`; the per-page call does
not re-decompress other pages). Its `OutputDev` trait is the within-page seam:
`begin_page`/`output_character`/`end_word`/`end_line`/`end_page` each take
`&mut self` and return `Result<(), OutputError>`, so a custom device can
charge output, count pages, and **check the deadline at character or line
granularity**, aborting by mid-page, by returning `Err` — which
`output_doc_page` propagates.

A Wayfinder `PdfExtractor` therefore maps onto the phase-0 contract directly:

```text
Document::load_mem(bytes)?              // one structural parse (opaque; see caveat)
for page in 1..=n {
    budget.check_deadline()?            // BETWEEN-PAGE checkpoint
    budget.count_pdf_page()?            // <- Budget::count_pdf_page (max_pdf_pages)
    output_doc_page(&doc, &mut sink, page)?   // sink checks deadline BETWEEN CHARACTERS
}
```

This was proven, not inspected. A throwaway proof
(`cancel_proof.rs`, run against the 4-page corpus file) demonstrated all
three abort modes:

- **between pages** — a 2-of-4 page cap stopped extraction before page 3;
- **within a page** — a 30-character budget aborted page 1 at exactly
  `This is page number 1 of the m` and returned the partial text;
- **deadline composition** — pages 1–2 completed, page 3 aborted on its first
  character when handed an already-expired deadline.

So this is the "parser that exposes per-page checkpoints" path #257 named,
and it does not need `tokio::time::timeout` to pretend the work stopped. The
`ExtractionRuntime` baked-in caller timeout + burnt-slot containment from #257
remains as the unconditional backstop; the cooperative path simply wins
whenever it can, so a well-behaved extraction returns its own
`DeadlineExceeded` and the pool slot comes back.

**One opaque phase remains, and it is bounded.** `lopdf`'s
`Document::load_mem` parses the xref and object table before any
`OutputDev` callback runs, so a pathological object structure could spend
wall-clock there without a checkpoint. This is acceptable: it is bounded by
`max_body_bytes` (32 MiB by default — the whole document is resident by then
anyway), it is a structural parse (xref walk + dictionary reads), not a
recursive interpreter, and it is not the "billion-laughs" shape. It is the
same residual shape every in-process parser has, and strictly smaller than
the "opaque end-to-end" risk the phase-0 docs hedged against. Revisit only if
a real corpus shows pathological load times.

### Q3 — Metadata conflict rules: Info dictionary wins (and `pdf-extract` is Info-only, so they align)

The conflict fixture carries a well-formed XMP packet
(`pdf:hasXMP=true`, `dc:title="XMP Title Wins?"`, `dc:creator="XMP Author"`)
that deliberately disagrees with the Info dictionary
(`/Title="Info Dict Title"`, `/Author="Info Dict Author"`). Captured Tika
resolved **every** title/author field to the Info-dict value —
`dc:title`, `title`, `pdf:docinfo:title`, `dc:creator`, `creator`, `Author`,
`meta:author`, `pdf:docinfo:creator` all came back `Info Dict Title` /
`Info Dict Author`. That is PDFBox's documented behaviour: the document
`Info` dictionary is authoritative for the core Dublin Core fields, and XMP
supplements rather than overrides it on conflict. The capture pins it; it is
not a universal Tika rule and could move with a PDFBox/Tika version bump, so
the implementation issue derives its expectation from this fixture rather
than from the PDFBox docs.

`pdf-extract` reads metadata through `get_info` → the trailer `/Info`
dictionary **only**; it has no XMP reader. For the narrow metadata promise
phase 0 made (`resourceName`, detected content type, and `title`/`author`
where explicit), that is exactly the source captured Tika reconciled to — so
a Wayfinder PDF extractor mapping Info `/Title`→`title`, `/Author`→`author`
is aligned with the ground truth for this case. The risk is bounded and
one-directional: if a future Tika/PDFBox change makes XMP override the Info
dict, Wayfinder (Info-only) would diverge; the differential harness added by
the implementation issue would catch that against this fixture.

Note one rendering divergence to handle: Tika prepends the document title to
the `extractFormat=text` body for PDFs
(`Info Dict Title\n\n\nBody text...`), whereas `pdf-extract` returns only the
body. The implementation decides whether to synthesise that title heading to
match Tika; it is an envelope concern, not an extraction one.

## Corpus and fixtures

### Provenance (all generated, all redistributable)

Every corpus file was generated for this exploration; none is a third-party
document. Fonts are **DejaVu Serif/Sans** (Bitstream-Vera family,
permissively licensed, freely redistributable), embedded as **subsets** by
**WeasyPrint 69** (HarfBuzz shaping), so the ToUnicode CMaps and OpenType
ligatures are the same shape Word/LibreOffice emit — a genuine born-digital
PDF, not a hand-rolled byte stream. `pikepdf` 10.11 post-processed the
encrypted, metadata-conflict, malformed-objects, and image-only variants.

The one-off generator is documented but **not committed** (it ran against
local font files under `/tmp`); reproducibility for the *fixtures* is the
`capture.sh` block, and the corpus files themselves are the artefact. Files
are intentionally small (≈0.8–5 KB) and single-feature so failures localise,
matching the existing `solr-ref/extract-inputs/` style (`sample.txt`,
`broken.pdf`, ...).

| File | How made |
|---|---|
| `pdf-embedded-font.pdf` | WeasyPrint, DejaVu Serif subset, accented Latin |
| `pdf-ligatures.pdf` | WeasyPrint, DejaVu Serif, `ffi`/`fi`/`fl` sentences |
| `pdf-multicolumn.pdf` | WeasyPrint, CSS `column-count:2` |
| `pdf-multipage.pdf` | WeasyPrint, 4× `break-before:page` (the per-page corpus dimension) |
| `pdf-metadata-conflict.pdf` | WeasyPrint body + pikepdf XMP (`dc:title`/`dc:creator`) set to disagree with the Info dict |
| `pdf-encrypted.pdf` | WeasyPrint body + pikepdf AES-128 (V=4, R=4) user-password encryption |
| `pdf-malformed-objects.pdf` | WeasyPrint body, then the first content-stream deflate header zeroed (valid xref/trailer; broken stream) |
| `pdf-image-only.pdf` | pikepdf-built page: a 64×32 FlateDecode greyscale `XObject` under `/Resources/XObject`, `…/Im0 Do`, no text operators |

`broken.pdf` from #171 is untouched — it is the "not a PDF at all" case;
`pdf-malformed-objects.pdf` is the distinct "structurally a PDF, broken
object inside" case the issue asked for.

### Captured Solr fixtures (ground truth)

Appended to `solr-ref/capture.sh` as the `# --- /update/extract PDF corpus
(issue #261) ---` block: a fresh container/core/port
(`wayfinder-solr-261` / `extract261` / `:9030`), the same Search-API-shaped
`ExtractingRequestHandler` as the #171/#258 blocks, and a `cap_extract261`
helper. One fixture per corpus file, all `extractOnly=true&extractFormat=text`
(the `search_api_solr` wire shape). Statuses observed: `200` for
embedded-font, ligatures, multicolumn, multipage, metadata-conflict,
image-only; `500` for encrypted and malformed-objects (both in the standard
Solr error envelope, `SolrException` root).

These fixtures live in `solr-ref/responses/` but are **deliberately not added
to `solr-ref/manifest-multipart.tsv`**: Wayfinder has no PDF extractor yet,
so adding them there would turn exploration evidence into permanent expected
divergences. The implementation issue extends the runner and adds the rows,
exactly as #171/#258 deferred their multipart rows until a route existed.

No existing fixture was re-captured. The block ran as a standalone snippet
against a throwaway container — never the whole `capture.sh`, which rewrites
every fixture and churns `QTime`/`_version_`/`rid` across every branch. The
committed `capture.sh` block is the reproducible record; the standalone run
only ever wrote the seven new `extract_pdf_*.json` files.

## Implementation issue spec (the #261 go-issue follow-up)

Title: **`feat(extract): PDF text extraction (pdf-extract)`**. Scope, in the
order the phase-0 contract already expects:

1. **Dependency.** Add `pdf-extract = "0.12"` (pulls `lopdf` 0.42). Keep it
   off the default feature set if a lean build wants it — but it is pure
   Rust, no native deps, consistent with the existing crate surface.
2. **`PdfExtractor` implementing `Extractor`.** Parse once with
   `Document::load_mem`, then loop `output_doc_page` per page against a
   budgeted custom `OutputDev`. Between pages:
   `budget.check_deadline()?` then `budget.count_pdf_page()?` (the
   `max_pdf_pages = 5000` guard from #257). Within the page, the device
   charges `budget.push_str` per run and `budget.check_deadline` every N
   characters (mirror `HTML_DEADLINE_CHECK_INTERVAL`). Register it in
   `dispatch()` for `ContentType::Pdf`.
3. **The two Q1 divergences, handled deliberately.**
   - *Malformed → silent empty.* A `pdf-extract` `Ok("")` is not the same as
     Tika's 500. The implementation needs a rule for when empty is legitimate
     (image-only scanned PDF) vs a swallowed error. The `extract_corrupt_pdf`
     / `extract_pdf_malformed_objects` fixtures are the differential evidence;
     `EXPECTED_DIVERGENCES` is the mechanism. At minimum, document which
     shape Wayfinder emits and pin it with a mutation test.
   - *Panics.* `catch_unwind` already maps these to 500. Keep it; add a test
     that feeds a panic-triggering malformed PDF (e.g. the `Do`-without-XObject
     shape) and asserts the 500 envelope, not a crash.
4. **Metadata.** Map Info-dict `/Title`→`title`, `/Author`→`author` via
   `get_info`. No XMP — Q3 shows Info is the aligned source. Derive the
   `file_metadata` expectation from `extract_pdf_metadata_conflict.json`.
5. **Envelope.** Decide the title-prepending divergence (Tika prepends the
   PDF title to `extractFormat=text`; pdf-extract does not). Render via the
   existing `ExtractRender`; the `detected_essence` already returns
   `application/pdf` for `ContentType::Pdf`.
6. **Differential rows.** Add the eight corpus rows to
   `manifest-multipart.tsv`; add/delete `EXPECTED_DIVERGENCES` entries so the
   harness is the evidence for the compatibility claim, per CLAUDE.md.

Acceptance mirrors every other extractor issue: `cargo test` hermetic (no
network, no Docker; live-Solr gated on `WAYFINDER_DIFF_SOLR=1`),
`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean,
tests red for the right reason first, and the mutation rule on the malformed-
stream guard.

## Verification performed

- `gh issue view 261` and the #171/#257/#258 reports read for context and
  the budget substrate the verdict inherits.
- Corpus generated with WeasyPrint 69 + pikepdf 10.11 (DejaVu Serif/Sans);
  each file verified with pikepdf for the property it claims (subset `+`
  BaseFont, present ToUnicode, `/Count`, encryption, Info≠XMP, no text ops).
- `pdf-extract` 0.12.0 / `lopdf` 0.42 evaluated in a throwaway crate under
  `/tmp` (not in the repo): `extract_text_from_mem[_encrypted]` on every
  corpus file, plus an `OutputDev` + `output_doc_page` proof
  (`cancel_proof.rs`) demonstrating all three cooperative abort modes.
- Tika ground truth captured against `solr:9.10.1` (`SOLR_MODULES=extraction`,
  Search-API-shaped `ExtractingRequestHandler`) — eight `extract_pdf_*.json`
  fixtures, statuses and metadata read back for the comparison table.
- `capture.sh` block appended and `bash -n`-checked; run as a standalone
  snippet so no existing fixture was re-captured (`git status` shows only the
  seven new corpus files, eight new fixtures, and the `capture.sh` edit).

## Residual risks

- **One opaque phase** (`Document::load_mem`) is not cooperatively
  checkpointed; bounded by `max_body_bytes`. Acceptable per #257; revisit if a
  real corpus shows pathological parse times.
- **Malformed-stream silent-empty** is a behavioural divergence from Tika's
  500. The implementation issue owns the resolution; it cannot be hidden by
  widening a normaliser.
- **Encrypted PDFs.** Off the realistic wire (the client sends no password);
  both tools 500 there. `pdf-extract`'s password path is additionally broken
  on this AES-128 fixture (accepts the password, returns empty), but that
  path is not on the wire; `lopdf` has the `aes` crate wired, so the bug is
  in the decrypt plumbing, not missing AES. RC4/legacy and pre-decrypted
  extraction are unaffected. Document, do not block on.
- **PDFBox-version drift** could change the Info-vs-XMP precedence the Q3
  fixture pins. The differential harness is the guard.

Review verdict: exploration acceptance satisfied — corpus committed, fixtures
captured, cancellation decision made with empirical proof, metadata rule
pinned by capture, implementation issue specced. Ship the implementation
issue.
