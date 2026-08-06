# #393 — static text length bound

**Status:** complete; independently reviewed and approved.

## Approved spec and decision

Static `text_en` and `text_general` accept terms through an inclusive **32,766 Unicode-scalar** bound and discard a term at 32,767 scalars. This script-independent resource-protection ceiling is far above plausible search terms. Tantivy 0.26.1 exposes no discovered hard term maximum; its `RemoveLongFilter` is optional protection for unconstrained content, not a compatibility limit. Frozen `solr-ref/responses/` fixtures were unchanged; no capture was made.

## Implementation / changed behavior

- Replaced the former static byte cutoff: a 45-byte English term survives in `text_en`, and a 14-scalar/42-byte Korean term survives in `text_general`.
- Static chains retain synonym behavior. Dynamic `_dynamic_text` and other language chains retain their existing length/filter behavior.
- Static tokenizer identities changed under analyzer contract **v7**. Existing **v6** index markers require reindexing/adoption rather than silently using incompatible indexed terms.

Files changed: `src/schema.rs`, `src/core_index.rs`, `tests/schema_layer.rs`, `tests/synonyms_ui.rs`, `tests/phase4_review_regressions.rs`, and this report. (The final Reviewer quick-fix added the direct `text_en` inclusive/over-bound assertion in `tests/schema_layer.rs`.)

## Evidence

- Targeted `cargo test` runs: red before implementation—45-byte English and 42-byte Korean terms were dropped by the old chain; the v6 contract test was red; saving a synonym containing the 45-byte term was red. Targeted tests passed after implementation.
- Mutation evidence: temporarily setting the static maximum to 40 made both static-preset tests fail; the mutation was reverted.
- The first full gate, `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`, found two obsolete tests sequentially. They were corrected/restored without weakening the intended behavior.
- Final foreground full gate passed:

  ```text
  cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
  ```

  All formatting, lint, and test checks were green.

## Review

Independent review round 1 requested a quick fix: add a direct `text_en` upper-bound assertion. Round 2 approved the change after a full green gate. No review cap was reached.

## Outstanding items

No unresolved risks, accepted deviations, or follow-ups.
