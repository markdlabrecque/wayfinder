# #350 — select: parse form-encoded POST bodies (postbigrequest)

**Date:** 2026-08-11. Issue #350 (open). Branch
`markdlabrecque/issue-350-parse-form-encoded` off `main`. This report
documents the fix for Solarium's `postbigrequest` silent-zero-results bug.

## The defect

`search_api_solr` loads Solarium's `postbigrequest` plugin on `search()` and
`autocomplete()` whenever `http_method === 'AUTO'` (the default). When a GET's
query string exceeds `maxquerystringlength` (default **1024** bytes),
`PostBigRequest::preExecuteRequest()` switches the method to POST, sets
`Content-Type: application/x-www-form-urlencoded`, moves the query string into
the raw body, and calls `clearParams()`. Wayfinder read `RawQuery` only, so
such a request arrived with zero params and answered HTTP 200 / empty result
set — no error, no warning. A long `fq` list, several facet fields, or a wide
`fl` crosses 1024 bytes routinely; a server with `http_method: POST` sends
every query this way.

Invisible to the suite because every fixture was a short GET.

## Ground truth: Solr's merge model (finding 189)

Captured against `solr:9` before any implementation, to fix precedence rather
than invent it. Three probes on the 5-doc `content` core:

1. **Merge, not precedence.** `q=quick`(query) + `q=lazy`(body) over `df=body`
   answered **byte-identically** to a GET `q=quick&q=lazy` — `numFound` 2,
   `docs [doc3, doc1]`, `echo.q == ["quick","lazy"]` in both. There is no
   "body wins" / "query wins" rule; query string and body are unioned exactly
   like repeated query params.
2. **Repeatable params AND.** `fq=category:animals`(query) +
   `fq=category:garden`(body) → both kept as `["category:animals",
   "category:garden"]`, ANDed → 0 docs.
3. **Single-valued reads take the FIRST.** `rows=10`(query) + `rows=2`(body)
   → echo `["10","2"]`, 5 docs returned (first wins). Reversed → 2 docs.

Query params come first, body params appended. This is precisely Wayfinder's
existing `Params` model (`get` = first, `get_all` = all in order, `echo` =
folds repeats into arrays in order), so the implementation is a pure append.

Also observed (out of scope, recorded): a POST to `/select` with **no**
`Content-Type` 400s in Solr (`Bad contentType for search handler :null`); an
empty form body is fine.

## Implementation

- **`src/params.rs`**: factored the pair-parsing loop into a free
  `parse_pairs`, shared by `Params::parse` (query string) and the new
  `Params::merge_form_body` (body). The two paths cannot drift because they
  are one decoder — both are `application/x-www-form-urlencoded`.
- **`src/lib.rs`**: new `params_with_form_body(query, content_type, body)` is
  the shared intake for `/select`, `/mlt`, `/terms`. It appends the body's
  pairs only when `Content-Type` is `application/x-www-form-urlencoded`
  (matched before any `;` parameter, case-insensitively — RFC media-type
  rules). Every other content type (a JSON body on `/select`, an absent one)
  leaves the body out of params. `select`, `mlt`, `terms` gained
  `headers: HeaderMap` + `body: axum::body::Bytes` extractors; the admin-UI
  `query_ui` direct call passes an empty `HeaderMap` / empty bytes (GET, no
  body).
- `strict_params` validates body params identically: the merged `Params`
  flows through the same `check_params` allowlist, so a POSTed unknown param
  400s where the GET does.

## Fixtures (`solr-ref/`)

New capture block appended at the end of `capture.sh` (reuses the main
`content` core; `cap_form_post` writes a 7th `content-type` manifest column).
Run with `bash solr-ref/capture.sh --only '^form_post_'`; verified the diff
against a pre-run backup was append-only — no existing fixture churned.

- `form_post_big_get` (`manifest.tsv`) and `form_post_big_post`
  (`manifest-errors.tsv`, content-type `application/x-www-form-urlencoded`):
  the SAME params, a >1024-byte query string, as a GET and as the form POST
  Solarium would send. Identical envelopes (modulo `QTime`, normalised).
  **This is the bug's evidence pair** — the POST previously answered empty.
- `form_post_merge_rows`: `rows` in both query and body → echo
  `["2","10"]`, 2 docs (first wins). Pins finding 189's merge model.

The big query is built from real params only (a category facet + a run of
`fq=category:animals` filters that AND to the same set), with an in-script
length assertion that fails the capture if it ever drops below 1024.

## Harness (`tests/`)

- `tests/common/diff.rs`: `ManifestErrorEntry` gained an optional 7th
  `content_type` column (absent ⇒ `application/json`, the JSON-body default).
- `tests/common/mod.rs`: `request_full_with_content_type(app, method, path,
  body, content_type)`; `request_full` delegates with `application/json` so
  every existing JSON-body caller is unchanged.
- `tests/differential.rs`: the manifest-errors runner sends each row's
  declared content-type (`entry_content_type`); the three form-POST rows now
  ride this to Wayfinder. Live (`WAYFINDER_DIFF_SOLR=1`) mode already sends
  form-encoded via curl `-d`, so it is unchanged.
- `tests/form_post_body.rs` (new, 7 tests): the core bug, the merge/precedence
  model, the content-type gate (JSON body not parsed; `; charset=` suffix and
  case still match), `strict_params` rejection of an unknown body param, and
  `/mlt` + `/terms` intake. Mutation-tested: skipping the merge turns 6/7 red
  (the JSON-content-type test correctly stays green) and fails the
  differential manifest-errors row.

## Verification

```
cargo fmt --check                                         # clean
cargo clippy --all-targets -- -D warnings                 # clean
cargo test                                                # all binaries green
cargo test --test differential                            # 44 passed
cargo test --test form_post_body                          # 7 passed
```
