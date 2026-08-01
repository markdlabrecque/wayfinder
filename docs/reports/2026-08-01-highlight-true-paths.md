# Report: issue #181 highlighting true paths

## Approved spec and corrected premise

Implement Solr-compatible highlighting for `hl.requireFieldMatch` and
`hl.mergeContiguous`, using captured Solr fixtures as ground truth.

The issue premise was corrected during capture: the pre-existing field-scoped
behavior was already `hl.requireFieldMatch=true`. The missing behavior was the
false/default cross-field path. With `false` or no parameter, terms from query
clauses for other fields may highlight in the requested field; with `true`,
terms are scoped to their query field.

## Implementation and evidence

Production behavior was implemented in `src/highlight.rs`, with request wiring
in `src/lib.rs` and index/query support in `src/core_index.rs`:

- `hl.requireFieldMatch=false` and the absent default use cross-field query
  terms; `true` uses field-scoped terms.
- Original-highlighter fragment selection is preserved, and
  `hl.mergeContiguous=true` merges only contiguous selected fragments, retaining
  their original intervening text rather than adding a synthetic separator.
- Empty fields are guarded before self-merging, preventing the discovered panic.

Solr-reference evidence was added in `solr-ref/capture.sh` and
`solr-ref/responses/`:

- `hl_require_field_match_false.json`
- `hl_require_field_match_true.json`
- `hl_merge_contiguous_false.json`
- `hl_merge_contiguous_true.json`

`docs/solr-ref-findings.md` records findings 113--114: true is field-scoped
query-term extraction rather than document-level matched-clause filtering, and
contiguous merging joins adjacent original fragments only while a real gap
keeps fragments separate.

Regression coverage in `tests/highlighting.rs` includes the false path and the
absent default, a custom case-sensitive analyzer, the empty-field panic guard,
and the original-fragment contiguous-merge behavior. Mutation checks deliberately
broke the validation/compatibility paths and confirmed the relevant regression
tests failed, then restored the implementation.

## Verification

All reported commands completed successfully:

- Targeted highlighting tests: green.
- `cargo fmt --check`: green.
- `cargo clippy --all-targets -- -D warnings`: green.
- `cargo test`: green.

## Review outcome

Round 1 found and the implementation fixed: absent-default behavior,
distinct-term scoring, stale documentation, and a dead-code wrapper. Round 2
found and the implementation fixed: custom-analyzer case handling and an
empty-field self-merge panic; both have regression tests.

The two-round review cap was reached. The foreground Orchestrator accepted the
work only after the final full-green gates above; this is the recoverable
post-cap resolution, not approval merely because the cap was reached.

## Follow-ups and risks

No follow-ups remain. No unresolved risks were identified beyond the existing,
documented highlighter `ponytail:` simplifications.
