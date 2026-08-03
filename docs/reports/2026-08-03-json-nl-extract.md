# Issue #274 — `json.nl` honoured on `/update/extract?extractOnly=true`

Date: 2026-08-03
Branch: `markdlabrecque/fix-issue-274`
Follow-up 3 from #258 (`docs/reports/2026-08-02-extract-only-tracer.md`).

## The premise, checked against real Solr first

The issue offered two exits — implement `json.nl` on the extract response, or
drop it from `EXTRACT_PARAMS` — and asked, per the compatibility contract, to
let a captured fixture decide which rather than guess. The deciding capture
(against `solr:9.10.1`, container `wayfinder-solr-274`, port 9040, core
`extract274`, removed after capture) is unambiguous: **Solr honours `json.nl`
here, so the right answer is to implement it.**

`file_metadata` is a plain `NamedList` in Solr, and the JSONWriter reshapes it
per the param exactly as it does a facet bucket list:

- `flat` (default) — `["key", [values], ...]` (the existing baseline,
  `extract_plain_text_xml.json`)
- `map` — `{"key": [values], ...}` (key order preserved)
- `arrarr` — `[["key", [values]], ...]`
- `arrmap` — `[{"key": [values]}, ...]`

`responseHeader` is a `SimpleOrderedMap` and stays an object under every value,
and `file` is a String value (not a nested NamedList), so neither moves —
verified byte-identical across all four values. Recorded as finding 128 in
`docs/solr-ref-findings.md`.

Side note (not captured as a fixture): an **invalid** value such as
`json.nl=garbage` makes Solr's JSONWriter emit truncated, invalid JSON
(`"file_metadata"` with no value) while still answering HTTP 200 — actively-
worse behaviour Wayfinder deliberately does not reproduce. Unknown values fall
back to `flat` via `facet::JsonNl::from_params`, consistent with the facet
routes; that is a defensible PRD-section-2 divergence, not a to-do, and is not
captured because the malformed body is unparseable by the harness.

## What shipped

`file_metadata` is now rendered through the same `JsonNl` model the facet
routes already use, so the allowlist (`json.nl` is on `EXTRACT_PARAMS`) and the
handler agree for all four values rather than only by accident for `flat`.

- **`src/facet.rs`** — `JsonNl` and `from_params` are now `pub(crate)`, and a
  generic `render_named_list(&[(String, Value)], JsonNl) -> Value` renders any
  ordered name→value sequence in the four `json.nl` shapes. The extract
  `file_metadata` keys are always strings (no facet `facet.missing`-style null
  key to negotiate), so this is a sibling of `render_buckets`, not a rewrite of
  it.
- **`src/lib.rs`** — the extract handler builds `file_metadata` as
  `Vec<(String, Value)>` and hands it to `facet::render_named_list` with
  `JsonNl::from_params(&params)`. The `flat` path is byte-identical to before
  (the baseline fixture still matches), so this is purely additive for the
  non-default values.
- **`tests/common/diff.rs`** — `normalize_extract`'s `X-Parsed-By` strip is now
  shape-agnostic. The prior `strip_x_parsed_by_metadata_key` assumed the flat
  array; once the handler honours `json.nl`, `file_metadata` is map/arrarr/
  arrmap as often as flat, so the strip has to follow. `strip_x_parsed_by_metadata`
  branches on the rendered shape (object → map; array whose first element is a
  String/Array/Object → flat/arrarr/arrmap) and removes the entry in place,
  preserving shape so a genuine shape difference still surfaces in the diff.

## New fixtures and manifest rows

Three captures, all on the plain-text `extractOnly` baseline (the same input as
`extract_plain_text_xml`) so the only varying factor is `json.nl`:

- `solr-ref/responses/extract_plain_text_json_nl_map.json`
- `solr-ref/responses/extract_plain_text_json_nl_arrarr.json`
- `solr-ref/responses/extract_plain_text_json_nl_arrmap.json`

Plus three rows in `solr-ref/manifest-multipart.tsv`, three
`ACCEPTED_DIVERGENCES_MULTIPART` entries (each carries `X-Parsed-By` in both
the XHTML `file` meta element and `file_metadata`, same waiver as the #258
plain-text rows), and a `#274` block appended to `solr-ref/capture.sh`
(append-at-the-end, per the concurrent-merge rule). The `flat` baseline is the
existing `extract_plain_text_xml.json` — no new flat fixture was needed.

Captured directly into `solr-ref/responses/`, not by re-running `capture.sh`
(that would have churned every fixture's `QTime`/`_version_`/`rid` across all
branches' diffs). The full `capture.sh` re-run was deliberately avoided per
CLAUDE.md; only the three new fixtures were written.

## The self-expiring trip-wire fired, as designed

`budget_violation_statuses_have_no_captured_fixture_yet` (`src/extract.rs`)
asserts the set of `extract_*` fixtures matches `CAPTURED_EXTRACT_FIXTURES`
exactly. The three new fixtures tripped it — exactly its purpose. They are 200
successes with `json.nl` variations, the same shape of response as the existing
plain-text baseline, so they cover none of the uncaptured budget-violation
statuses (413/503/415/400) and the status mapping needed no re-verification;
the array was just extended (39 → 42) and re-sorted.

## Tests

- **Differential, end-to-end** (`extract_multipart_manifest_matches_captured_
  fixtures`): the three new manifest rows run against Wayfinder, normalise both
  sides, and match the captured fixtures — the compatibility evidence.
- **Shape pin** (`extract_file_metadata_shape_follows_json_nl`): asserts the
  handler renders `file_metadata` as object for `map` and array for
  `flat`/`arrarr`/`arrmap`, and that `responseHeader`/`file` are untouched,
  independently of `normalize_extract`.
- **Explicit-flat guard** (`extract_explicit_json_nl_flat_matches_the_flat_
  baseline_fixture`): `json.nl=flat` still matches the existing flat baseline,
  so honouring the param does not regress the default.
- **Normaliser guard** (`normalize_extract_strips_x_parsed_by_from_every_
  json_nl_shape`): hand-built `Value`s in each of the four shapes prove
  `X-Parsed-By` is stripped and unrelated keys survive in their original shape.
- **Loader** (`load_manifest_multipart_parses_every_line...`) stays green with
  the three new rows.

Confirmed red for the right reason before the implementation: the four
behavioural tests failed because the handler rendered `file_metadata` flat
regardless of `json.nl`, and the explicit-flat baseline was already green.

## Green evidence

- `cargo test --no-fail-fast` — **1075 passed**, 0 failed, hermetic (no
  network, no Docker).
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact invocation).

## Out of scope / not attempted

- `json.nl` is honoured only for `file_metadata`. That is the whole of what the
  param affects on this endpoint (`responseHeader` and `file` are unaffected,
  per finding 128) — there is nothing else to implement here.
- The invalid-`json.nl` fallback to `flat` is documented but not fixture-pinned
  (Solr's malformed-JSON response cannot be parsed by the harness); it reuses
  the established `JsonNl::from_params` behaviour rather than introducing a new
  one.
