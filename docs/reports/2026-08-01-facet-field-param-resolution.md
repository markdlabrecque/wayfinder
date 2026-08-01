# Issue #150 — facet-field local-param resolution

## Goal and acceptance

Pin Solr-compatible resolution of `facet.field` local parameters: a response `key` must not affect schema resolution of the faceted field, and duplicate `key` values must follow captured Solr behavior. Acceptance required a real Solr fixture, regression coverage (including the numeric shared-schema case), mutation proof, and green compatibility/full gates.

## Result and implementation

The capture contradicts the ticket's source-based guess: `{!key=a key=b}category` is **first-wins**. Solr returns the category counts under `a`, with no `b` member (finding 103).

Production behavior was already correct: ordered `LocalParams` lookup takes the first `key`, and schema resolution keys off the faceted field rather than its response label. There is no production behavior change. The only `src/` edits in `e839b3d` document the verified behavior.

Commits and files:

- `3e70afa test(facet): capture duplicate local-param keys` — `solr-ref/responses/facet_local_params_duplicate_key.json`, `solr-ref/manifest.tsv`, `solr-ref/capture.sh`, `docs/solr-ref-findings.md`.
- `9cabde4 test(facet): pin field-based schema resolution` — `tests/facet_local_params_key.rs`, `tests/common/mod.rs`; adds numeric `views` to the shared test schema and proves `{!key=views}category` remains a text facet (no Points warning).
- `e839b3d docs(facet): record duplicate key precedence` — documentation comments only in `src/facet.rs` and `src/local_params.rs`.

Mutation proof: deliberately resolving the facet value kind from the local-param response label rather than the actual field makes the numeric-label/text-field regression fail; the regression therefore guards the field-based resolution contract.

## Evidence

- `cargo test --test differential` — 27 passed.
- Targeted facet suite — 12 passed.
- Implementer full gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed.
- Reviewer full gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed.

## Review and follow-up

Reviewer verdict: **approved**, no findings.

Accepted correction: first-wins replaces the ticket's last-wins guess, based on captured Solr evidence. No unresolved risks, deferred follow-ups, skips, or outstanding work.
