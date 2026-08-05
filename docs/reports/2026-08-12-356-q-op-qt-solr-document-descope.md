# #356 — `q.op` / `qt` on the `solr_document` datasource stay out of `SELECT_PARAMS`

**Date:** 2026-08-12. **Branch:** `markdlabrecque/issue-356-q.op-qt-solr`.
**Status:** **descoped with evidence** — `q.op` and `qt` stay absent from
`SELECT_PARAMS`; the descope is recorded in PRD §5 and guarded.

Found by the 2026-08-04 full-source sweep of `search_api_solr` 4.4.0. Lowest
priority of the sweep's findings, listed so the parity picture is in one place.

## The evidence

Both params are emitted on the **`solr_document` datasource path only** — the
datasource for indexing/searching documents in a foreign Solr core that Drupal
does not own (`SearchApiSolrBackend.php:1808-1830`). The query builder branches
on `Utility::hasIndexJustSolrDocumentDatasource($index)`:

- A normal Drupal-owned datasource gets the index/site filter (line 1809).
- Only the `else` — the index is *just* `solr_document` — emits
  `addParam('qt', $config['request_handler'])` when a handler is configured
  (line 1814) and `addParam('q.op', 'OR')` unless already set (line 1828), the
  latter with the comment that the query builder assumes OR "but a foreign
  schema could have a non-default config for q.op".

Two things that keep this small (both pinned by the guard):

- The module's only other `q.op` occurrence (`:2085-2093`) is dead code — a
  `/* We keep this as an example. ... */` block inside
  `applySearchWorkarounds()`, never executed — not a second live path.
- `modules/search_api_solr_legacy/.../Solr36Connector.php:76-77` also adds
  `q.op`, but that is the Solr 3.6 legacy connector, out of scope for a Solr
  9.x backend.

## Decision: out of Wayfinder's world (no admission)

The `solr_document` / `SolrMultisiteDocument` datasources are not in Wayfinder's
world. A Wayfinder core is Drupal-owned by construction — #301 settled **one
core per site** as the supported topology (PR #323; the server serves a single
core per process, PRD open question 1) — so the datasources that target a
*foreign* core have no Wayfinder to point at, and the two params only that path
emits never reach a request Wayfinder serves. They therefore stay absent from
`SELECT_PARAMS` and 400 under `strict_params = true`, rather than being admitted
inertly.

Admitting them would be the wrong half-measure either way:

- `qt` is meaningless for a server with one select handler.
- `q.op` is **not inert** — it carries real OR/AND default-operator semantics —
  so admitting it without implementing the operator would be a silently wrong
  answer, and there is no served client to implement it for.

If the premise ever changes (a served client starts sending either param), the
right move is to admit `q.op` with real operator semantics and decide what `qt`
means for a single-handler server — not to have pre-admitted them inertly.

## Changes

- **`docs/PRD.md` §5** — new subsection "`solr_document` datasource — out of
  Wayfinder's world (`q.op`, `qt`)" placed before the Solr 9.x parity roadmap
  (a distinct descope category: client-evidenced but on an out-of-scope
  datasource, not "zero client evidence"). Records the evidence, the #301 link,
  and the two narrowness facts.
- **`docs/solr-ref-findings.md`** — finding 190, a source-sweep finding citing
  the exact lines.
- **`tests/q_op_qt_descope_guard.rs`** — new expiring guard, four channels:
  1. **Source** — both params stay confined to the `solr_document` branch
     (windowed from the gate to the next statement); `qt` emitted exactly once,
     `q.op` exactly twice (live + dead example); the second `q.op` stays inside
     the `/* We keep this as an example. */` comment.
  2. **Trace** — no captured request across the 28 traces sends `q.op` or `qt`,
     with a positive control that the corpus carries `q=` select traffic.
  3. **PRD** — §5 records the descope and references #356, #301, `SELECT_PARAMS`.
  4. **`strict_params`** — `q.op` and `qt` still 400 (executable assertion they
     have not been silently added to `SELECT_PARAMS`).

## Mutation check

The two `strict_params` 400 tests are code whose whole value is failing
correctly, so they were mutation-tested: temporarily adding `"q.op"` and `"qt"`
to `SELECT_PARAMS` flipped both to FAILED, then the change was reverted. The
suite is green again afterwards.

## Verification

```
cargo fmt --check                         # clean
cargo clippy --all-targets -- -D warnings # clean
cargo test                                # all green, incl. finding_citations
cargo test --test q_op_qt_descope_guard   # 11 passed
```

No `src/` production code touched — this is a docs + guard change. The params
were already absent from `SELECT_PARAMS` and parsed nowhere; the work is to
record *why* they stay absent and to make that "why" executable.
