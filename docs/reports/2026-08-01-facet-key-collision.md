# Issue #149 — reject colliding `facet.field` response labels

- Issue: #149
- PR/CI: not opened or claimed by this report.

## Problem and ratified decision

Solr emits duplicate outer JSON members when distinct `facet.field` requests use the same
`{!key=...}` response label. Normal JSON object models cannot represent those duplicates without
silently retaining one value. The approved, ratified PRD divergence is therefore deliberately
narrow: Wayfinder returns an informative normal error-envelope 400 only for colliding
`facet.field` response labels, rather than emitting duplicate members or silently choosing a
facet.

The four new Solr fixtures establish finding 102: field-label collisions produce duplicate outer
members in both flat and `json.nl=map` forms; identical `facet.query` values instead coalesce.
Wayfinder matches the latter behavior.

## Implementation

Changed files:

- `src/facet.rs` — detects colliding `facet.field` response labels and returns the 400.
- `tests/facet_key_collision.rs` — fixture-derived collision and coalescing coverage.
- `solr-ref/responses/facet_collision_field_flat.json`
- `solr-ref/responses/facet_collision_field_map.json`
- `solr-ref/responses/facet_collision_query_flat.json`
- `solr-ref/responses/facet_collision_query_map.json`
- `solr-ref/capture.sh` — capture definitions.
- `docs/solr-ref-findings.md` — finding 102.
- `docs/PRD.md` — ratified divergence 7.

The collision fixtures are deliberately excluded from `solr-ref/manifest.tsv`: a differential
JSON-object comparison cannot faithfully preserve the duplicate members that are the evidence.
An executable guard protects that exclusion, so it fails if the rationale stops holding rather
than allowing an invalid manifest entry to become routine.

## TDD and review evidence

- Initial `cargo test --test facet_key_collision`: 3 passed, 2 failed. The failures proved the
  prior 200 last-write-wins behavior rather than the requested rejection.
- After implementation: `cargo test --test facet_key_collision`: 5 passed.
- Review round 1 found formatting and test-guard weaknesses.
- Review round 2 ran its full gate green, then requested one final raw-test bounding quick fix.
  The foreground applied the exact bounded `facet_fields` assertion because the two-round review
  cap had been reached.

The capped review is not post-fix Reviewer approval. The residual process risk is that the final
foreground-applied bounding assertion did not receive another review round; no unresolved product
risk is recorded beyond the documented PRD divergence.

## Final verification

- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test` — passed; the full suite includes `facet_key_collision` 5/5.

No CI or PR result is claimed. No follow-ups were deferred.
