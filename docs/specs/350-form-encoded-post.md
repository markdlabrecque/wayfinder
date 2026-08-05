> **Historical implementation record.** This completed spec does not define current requirements or future work.

# #350 — parse form-encoded POST bodies (`postbigrequest` silently yields zero results)

Branch: `350-form-encoded-post`. **Group B — rebase onto #353**, which also edits
`SELECT_PARAMS`.

**This is a live silent-failure bug at default configuration.** Treat it as the
highest-value item in the batch.

## The defect

`SolrConnectorPluginBase.php:999-1005` loads Solarium's `postbigrequest` plugin
on both `search()` and `autocomplete()` whenever `http_method === 'AUTO'` — the
**default** (`:147`).

Solarium's `PostBigRequest::preExecuteRequest()`: when a GET's query string
exceeds `maxquerystringlength` (default **1024**), it

1. switches the method to POST,
2. sets `Content-Type: application/x-www-form-urlencoded`,
3. moves the query string into the raw body, and
4. calls **`clearParams()`** — so the URL carries no query string at all.

Wayfinder's `select` reads `RawQuery` only (`src/lib.rs`, the `select` handler);
nothing parses a form-encoded body. The request arrives with zero params, and an
absent `q` is treated as "matches nothing" — deliberately, per
`err_missing_q.json`.

**Result: HTTP 200 with an empty result set.** No error, no warning, no log. A
site with a long `fq` list, several facet fields, or a wide `fl` crosses 1024
bytes as a matter of course and simply sees no results. A server configured with
`http_method: POST` sends *every* query this way.

Invisible to the current suite because every fixture is a short GET.

## Verify before implementing

1. **Confirm the Solarium behaviour against real source**, not the issue text —
   particularly the `clearParams()` call and the 1024 default. If Solarium is not
   vendored, note that and cite the upstream version you read.
2. **Establish Solr's precedence rule** for a param present in *both* the query
   string and the body. Solr permits both. **Do not invent a rule** — capture it
   while the container is up. This is the single most important thing to get
   right, because guessing produces a divergence that no test will catch until a
   real site hits it.
3. Confirm which routes the client can post to this way. The issue names
   `/select`, `/mlt`, and `/terms`. Check `any_method` routing in `src/lib.rs`
   and the module source for others.

## Scope

Accept `application/x-www-form-urlencoded` request bodies, merging them into the
same `Params` the query string produces, on every route the client can post to:
`select`, `mlt`, `terms`, and — once #351 lands — `autocomplete`. Apply the
precedence rule established in step 2.

**`strict_params` must validate body params identically.** Otherwise a POSTed
unknown param becomes a silent pass where the GET 400s, which reintroduces the
same class of bug this issue is about.

Watch the body size limit. The routes are wrapped by a body-limit layer
(`src/lib.rs:512`, `tests/body_limit.rs`) — a request large enough to trigger
`postbigrequest` must not then be rejected by the limit. Check the interaction
explicitly and add a test for it; a 413 in place of results is a better failure
than silence but still a bug.

## Fixtures

Needs a capture block: one `select` whose query string exceeds 1024 bytes,
issued **both** as a GET and as the form-encoded POST Solarium would send,
asserting identical envelopes. Plus the precedence case from step 2.

POST bodies are not core-relative GETs, so per the compatibility contract they
belong in **`solr-ref/manifest-errors.tsv`**, not `manifest.tsv`. The
differential harness GETs every row in `manifest.tsv` verbatim — putting a POST
there breaks it.

Append the capture block at the **end** of `solr-ref/capture.sh`, run with
`capture.sh --only <prefix>`, and commit the fixtures before anything else.

## Testing

Tests first, red. The defining test: **a form-encoded POST with no query string
returns the same results as the equivalent GET.** That test fails today with an
empty result set, which is the bug — confirm it fails for that reason and not
because the route 404s or 405s.

Also cover:

- a param in both query string and body resolves per the captured precedence
- an unknown param in the body 400s under `strict_params`, identically to the GET
- a body at the size that triggers `postbigrequest` is not rejected by the body
  limit
- `mlt` and `terms` get the same treatment

Mutation-test the `strict_params` body validation: remove it, confirm a test goes
red, revert.

## Files

**You own:** `src/lib.rs` (`select`, `mlt`, `terms`, `Params`, `SELECT_PARAMS` —
**add only, never reorder**), `solr-ref/capture.sh` (append at end),
`solr-ref/manifest-errors.tsv`, `tests/differential.rs`.

**Sibling:** #353 lands first in `SELECT_PARAMS`; rebase onto it and re-run the
gates after. A green branch plus a green `main` does not imply a green merge.

**Coverage denominator:** if this changes the endpoint count, say so in the PR
and leave the number alone — #354 owns it.

## Definition of done

- Form-encoded POST bodies parsed and merged on all affected routes, with
  captured precedence.
- `strict_params` validates body params identically, mutation-tested.
- Body-limit interaction tested.
- GET/POST envelope equivalence proven against fixtures.
- If an `EXPECTED_DIVERGENCES` entry starts matching, delete it.
- `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
  clean.
