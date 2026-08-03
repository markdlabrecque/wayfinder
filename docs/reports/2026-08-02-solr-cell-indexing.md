# Issue #259 — Solr Cell indexing semantics (Option B)

Date: 2026-08-02
Branch: `markdlabrecque/issue-259-solr-cell-indexing`
Follow-up to: `docs/reports/2026-08-02-extract-only-tracer.md` (#258, the
extractOnly tracer). PRD divergence 10 is the relevant compatibility record.

## The decision this issue turned on

The issue asked for `/update/extract` server-side indexing matching the
captured `extract_html_index`/`extract_html_select` pair. Investigation
against real `solr:9.10.1` (with `capture.sh`'s exact handler/schema config)
turned up a **blocking spec contradiction** before any code was written:

- **`links` requires fabricating `shape="rect"`.** The select fixture's
  `links: ["rect","https://example.test/doc"]` needs Tika's *injected*
  `shape="rect"` attribute, captured via `fmap.a=links` + `captureAttr=true`.
  PRD divergence 10 explicitly forbids that injection ("reproducing it would
  mean fabricating markup that was never in the document"). You cannot match
  the fixture and honour the PRD.
- **`body` is Tika's indexing-path content serializer output**, not
  extractOnly's text form — a structure-dependent whitespace layout Wayfinder
  does not replicate (and would need a second text serializer to).

Per CLAUDE.md ("Don't paper over a wrong ticket premise") this was flagged
with evidence rather than built to a forced spec. The owner chose **Option B**:
index via Wayfinder's own extractors and ratify the resulting `body`/`links`
divergence, keeping the no-fabrication stance consistent with divergence 10.

## What shipped

`/update/extract` now has two modes, selected by the resolved `extractOnly`
boolean:

- **`extractOnly=true`** (#258, unchanged): extract and return
  `{responseHeader, file, file_metadata}`.
- **`extractOnly` absent/false** (#259): apply the extracted content to the
  index through the same commit path `/update` uses, answering the bare
  `responseHeader` (`extract_html_index.json`).

The Solr-Cell field pipeline (`solr_cell_fields` in `src/lib.rs`):

1. **Source fields** from `ExtractedDocument::solr_cell_source_fields`:
   `content` (the extracted `body_text`), `title`/`author`, plus captured
   element attribute values grouped by element name when `captureAttr` is on.
2. **`lowernames`** (default `true`): lowercase field names.
3. **`fmap.<from>=<to>`**: rename (Search-API defaults `fmap.a=links`,
   `fmap.div=ignored_`, merged with request params).
4. **`uprefix`** (default `ignored_`): fields not resolving against the
   schema (declared or a dynamic rule) are dropped when `uprefix` is set —
   the `ignored_*` net effect — and pass through otherwise, so `add_documents`
   errors on a genuinely unknown field as strict Solr does.
5. **`literal.*`** overlay: explicit field values (`lowernames` applies,
   `fmap` does not — a literal is already the caller's chosen destination).

The HTML extractor (`src/extract.rs`) now collects every attribute value of
every element into `Extracted::captured_attrs` (budget-bounded via a new
`Budget::charge_output` that charges without appending to the text output),
propagated to `ExtractedDocument`. `captureAttr` gates whether the indexing
path uses them; the extractOnly path ignores them and pays only the build
cost.

Handler defaults are **hardcoded Search-API-shaped** (`lowernames=true`,
`uprefix=ignored_`, `captureAttr=true`, `fmap.a=links`, `fmap.div=ignored_`),
overridable by request params. Rationale: the only evidenced config, and the
captured pair was taken against it; "wire format only, never Solr's config
format" means matching that configset's wire behaviour, not exposing its
`solrconfig.xml`.

`check_params` gained a trailing-dot prefix family: an allowlist entry ending
in `.` accepts any key with that prefix (route-scoped, so `literal.id`/`fmap.*`
are accepted on `/update/extract` but still 400 on `/select`). `EXTRACT_PARAMS`
now carries `commit`/`commitWithin`/`softCommit`/`overwrite`/`uprefix`/
`lowernames`/`captureAttr` plus `literal.`/`fmap.` sentinels.

## The documented divergence

The indexed document's `body` and `links` come from Wayfinder's own extractors
and so differ from `extract_html_select.json`:

- `body` = Wayfinder's `body_text` (its HTML text form), not Tika's
  content-field serialization.
- `links` = the real `<a>` `href` only — no fabricated `shape="rect"`.

Recorded in **PRD divergence 10** (the "extractOnly required" bullet is
retired by #259; a new indexing-path bullet documents the `body`/`links`
divergence). The divergence is asserted in `tests/extract_index.rs`, which
also proves the captured fixture still genuinely differs (`assert_ne`), so it
cannot silently start matching. There is no Drupal/Search-API consumer for
server-side indexing (#259's own survey), so the divergence has no client
behind it.

## Tests

`tests/extract_index.rs` (11 tests, all green): the captured index response
matches `extract_html_index.json` exactly; index→select returns the extracted
doc with `id` (literal)/`body`/`links` and proves the fixture divergence;
`literal.*`, `captureAttr=false`, `uprefix` drop-vs-error mutation guards;
commit/softCommit visibility gating; invalid-boolean validation; the
`literal.*`/`fmap.*` allowlist under `strict_params`; and an extractOnly
regression guard. The two #258 tests that asserted `extractOnly` was required
were removed (their premise is retired) and noted in `extract_route.rs`.

`body` expected values are **derived from the `extract_html_only_text`
fixture** (its `file` is `"\n"×13 + title + "\n\n" + body_text`), not typed
from implementation output, per CLAUDE.md.

## Verification

```
cargo fmt --check                         # clean
cargo clippy --all-targets -- -D warnings # clean (CI's exact command)
cargo test                                # all green, hermetic
```

`tests/differential.rs` (36 tests) green hermetically; the live Solr
differential is unchanged in shape (the select pair is a two-step flow the
single-request manifest runner cannot express, so it lives in
`extract_index.rs` rather than `manifest-multipart.tsv`).

## Scope notes / ponytails

- Wayfinder does not emit Tika's long metadata list (`resourceName`,
  `Content-Type`, `stream_*`, `X-Parsed-By`, …) as indexable source fields.
  They would all be `uprefix`-dropped and the captured select returns only
  `id`/`body`/`links`, so this changes nothing observable. Trigger for
  revisiting: a captured index whose `fmap.<tika-meta-key>` lands a value in a
  real schema field.
- `commitWithin` schedules a hard commit + reload (same divergence as
  `/update`); tested as accepted, not for its timer.
- A future config surface for non-Search-API handler defaults is deliberately
  not added (no evidenced need; hardcoding matches the captured configset).
