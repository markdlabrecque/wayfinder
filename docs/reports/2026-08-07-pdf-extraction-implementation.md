# Issue #294 — PDF text extraction (`pdf-extract`)

Date: 2026-08-07
Branch: `294-pdf-extraction`
Implements: the #261 GO decision (`docs/reports/2026-08-03-pdf-extraction-corpus.md`).
Closes #294.

## What landed

`PdfExtractor` implementing the in-tree `Extractor` trait, registered in
`dispatch()` for `ContentType::Pdf`, running on the phase-0 budget substrate
(#257) under the same four budgets as every other extractor (request bytes,
concurrency slots, extracted-character count, wall-clock deadline) plus the
PDF-specific `max_pdf_pages` counter that phase 0 already owned.

`pdf-extract` 0.12 (over `lopdf` 0.42) is the parser — pure Rust, no native
deps, consistent with the office stack. `euclid` 0.20 is named directly even
though it is already transitive via `pdf-extract`: the cooperative-cancellation
device builds `pdf_extract::Transform` values (`Transform2D<f64, Space, Space>`)
that must be the *same* euclid type the `OutputDev` trait uses, so the version
is pinned rather than left to `pdf-extract`'s selection.

## The cancellation contract, implemented

The #261 report's whole reason for existing was proving a PDF parser exposes
enough checkpoints for cooperative cancellation, and that is what the
`PdfSink` (a custom `pdf_extract::OutputDev`) implements at the two seams the
report named:

- **Between pages** — `begin_page` checks `budget.check_deadline()` then
  `budget.count_pdf_page()` (`max_pdf_pages = 5000`) before any of that page's
  content-stream work runs.
- **Within a page** — `output_character` checks `check_deadline()` every
  `PDF_DEADLINE_CHECK_INTERVAL` (= 256) glyphs, and charges every emitted
  character against the output budget (`Budget::push_str`) so the scalar/byte
  limits abort part-way through a page, not only at a page boundary.

`Document::load_mem` is the one opaque, un-checkpointed phase — bounded by
`max_body_bytes` (the whole document is resident by then) and a structural
parse, not a recursive interpreter. This is the residual risk the #261 report
accepted; no real corpus has shown it to bite.

One deliberate deviation from the report's literal pseudocode: the report's
`cancel_proof` looped `output_doc_page` per page. The implementation drives
the device with `output_doc` instead, which walks the page tree *once* — the
naive per-page loop re-walks it on every call (O(pages²)). The `begin_page`
callback is the between-page checkpoint the report's explicit loop performed;
the cancellation contract is identical, the complexity is linear.

## The two Q1 divergences, handled

1. **Malformed content stream → silent empty (200, where Tika throws 500).**
   `pdf-extract` swallows an unfilterable stream as "no text" rather than
   erroring, and it cannot tell that apart from a legitimately empty
   image-only PDF. Wayfinder emits the same 200-empty shape for both and
   records the status divergence: `extract_pdf_malformed_objects` is in
   `DIVERGENT_STATUS_MULTIPART` (200 vs the captured 500), and
   `tests/pdf_extractor.rs::malformed_pdf_extracts_to_empty_not_an_error`
   pins it at the extractor level. Mutation-tested: adding a guard that
   turns empty output into `Err` fails three unit tests and the differential
   runner (reverted).

2. **Panics → catch_unwind → 500.** `pdf-extract` has unchecked
   `unwrap`/`expect` throughout its content-stream interpreter. These are
   caught by the existing `spawn_extraction` `catch_unwind`
   (`ExtractError::Parse("extraction panicked")` → 500), which is
   mutation-tested in `tests/extraction.rs` already. The born-digital corpus
   does not trigger any panic (verified empirically against all eight files
   plus `broken.pdf`). A *synthetic* panic-triggering fixture was attempted
   (a hand-built page lacking `MediaBox`, then a `Do`-without-`XObject`
   shape) but neither reached the panic site — crafting a valid-enough-yet-
   panicking PDF is brittle and version-coupled to `pdf-extract`'s internal
   `expect` strings, so the durable guard is the generic `catch_unwind`, not
   a bespoke fixture. Revisit if a real corpus shows a panic, or if
   `pdf-extract` exposes a fallible API.

## Metadata

Info-dictionary only (Q3): `/Title` → `title`, `/Author` → `author`, read
via the public `doc.trailer` `/Info` reference and decoded (UTF-16BE BOM →
UTF-16, else PDFDocEncoding ≈ Latin-1 lossy). No XMP — `pdf-extract` has no
XMP reader, which is exactly the source captured Tika reconciled to on
conflict (the metadata-conflict fixture). The #261 report assumed
`pdf-extract`'s `get_info` was usable; it is **private** in 0.12, so this
reads the trailer `/Info` dictionary via `lopdf`'s public API instead — the
same source, different access path. Noted as a finding rather than a silent
re-decision.

## Envelope / `extractFormat=text`

All eight #261 fixtures are `extractFormat=text` (the `search_api_solr` wire
shape), so there is no captured PDF XHTML body to pin and no `_xml` manifest
rows. The device still renders a sensible `body_xhtml` (one `<p>` per
non-empty page) for the default XHTML path.

The leading-newline structure of Tika's PDF text bodies is *not* the 13-newline
constant the plain-text/HTML extracts use (PDFBox emits 30+ leading newlines,
a function of its paragraph structure). Rather than fabricate that count, the
differential harness's `normalize_extract` gains a PDF branch that collapses
whitespace runs to single spaces (applied symmetrically), so the body is
compared by its non-whitespace token sequence. This is the #261 report's
"normalisation detail for the renderer, not an extraction defect", made
explicit and self-checking: the runner still proves the raw envelopes differ
before normalising, and a dropped word / mojibake glyph / reordered column
still fails.

## Differential wiring

- `solr-ref/manifest-multipart.tsv`: eight #294 rows (six 200s +
  `extract_pdf_encrypted` 500 + `extract_pdf_malformed_objects` 500), all
  declaring `application/pdf`.
- `tests/differential.rs`:
  - `ACCEPTED_DIVERGENCES_MULTIPART` += six PDF success rows (body whitespace
    + rich Tika metadata, reconciled by `normalize_extract`'s PDF branch).
  - `DIVERGENT_STATUS_MULTIPART`: **deleted** `extract_corrupt_pdf` (415) —
    `broken.pdf` now reaches a parse failure (`load_mem` faults on the missing
    xref) and answers 500, matching the capture. This is PRD divergence 10's
    corrupt-PDF bullet retiring, exactly as the PRD predicted.
  - `DIVERGENT_STATUS_MULTIPART` += `extract_pdf_malformed_objects` (200 vs
    500). The runner's status-divergence path was extended to handle a
    *success* where the capture errored (check the body is a success body,
    not an error envelope), since the existing path assumed every divergence
    was an error code.
- `tests/common/diff.rs`: `normalize_extract` PDF branch (whitespace-run
  collapse + six-envelope-key metadata filter via `keep_envelope_metadata_keys`)
  and the `is_pdf_content_type` detector.
- `tests/extract_route.rs`: the obsolete
  `extract_corrupt_pdf_is_a_recorded_status_divergence` (which pinned the old
  415) is rewritten as `broken_pdf_is_a_500_parse_failure_matching_the_capture`,
  pinning the retirement.

`docs/PRD.md` divergence 10 is updated: the corrupt-PDF 415 bullet is retired
with a forward reference, and the malformed-content-stream 200 divergence and
the PDF body-whitespace normalised divergence are recorded.

## Drupal side

Out of scope here (this is the server). PDF becomes an allowed type in
`ExtractFileValidator` / the extraction settings form once the server serves
it, per the #294 issue body — that is the Drupal side's own issue to land.

## Verification performed

- `pdf-extract` 0.12.0 / `lopdf` 0.42 evaluated in a throwaway crate under
  `/tmp` (not in the repo) with a custom `OutputDev` + `output_doc_page`
  mirroring the production device: all eight corpus files, all three
  cooperative abort modes (between-page page cap, within-page char cap,
  already-expired deadline), metadata via the trailer Info dict, and the
  malformed/encrypted/broken outcomes — confirming the #261 report's
  predictions before any production code was written.
- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings`
  clean (CI's exact command); `cargo test` hermetic, all green (8 new
  `tests/pdf_extractor.rs` tests + the differential + the rewritten route
  test). No network, no Docker; live-Solr gated on `WAYFINDER_DIFF_SOLR=1`.
- Mutation test on the malformed→empty guard (break, confirm three unit
  tests + the differential runner fail, revert).

## Residual risks

- **One opaque phase** (`Document::load_mem`) is not cooperatively
  checkpointed; bounded by `max_body_bytes`. Accepted per #257/#261; revisit
  if a real corpus shows pathological parse times.
- **Malformed-stream silent-empty** is a behavioural divergence from Tika's
  500, recorded in `DIVERGENT_STATUS_MULTIPART` and pinned by a unit test +
  mutation test.
- **`pdf-extract` panics** are caught by the existing generic `catch_unwind`,
  not a PDF-specific fixture (see above). The corpus does not trigger any.
- **PDFBox-version drift** could change the Info-vs-XMP precedence the Q3
  fixture pins; the differential harness is the guard.
- **Encrypted PDFs** are off the realistic wire; both tools 500 there.
  `pdf-extract`'s password path is additionally broken on the AES-128 fixture
  but that path is not on the wire (documented in the #261 report, not
  blocking).
