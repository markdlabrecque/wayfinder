# #398 — repeated `suggest.dictionary` values

**Date:** 2026-08-06. **Issue:** #398. **Status:** complete, independently reviewed and approved.

## Approved spec / defect

Repeated `suggest.dictionary` values were incorrectly first-wins. `/suggest` now uses every supplied dictionary value; when absent it uses `und`. It validates all requested dictionaries before lookup and emits one response key per dictionary. `Params::get` behavior and spellcheck are unchanged.

## Implementation summary

- `src/lib.rs` collects and validates all repeated `suggest.dictionary` values before dictionary lookup, preserving the absent-parameter `und` default.
- `tests/suggest.rs` adds repeated-dictionary coverage.
- `docs/PRD.md` records §2 item 13: this is a Wayfinder/client contract; no new Solr capture was made.

## TDD and verification evidence

The new `de` + `fr` repeated-dictionary test was initially red: the response contained only `de`, proving the first-wins defect. It was green after the implementation. Reviewer follow-up added mixed `de` + unknown-dictionary coverage, confirming validation occurs for every value before lookup.

```text
cargo test --test suggest                              # 52 passed
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test  # exit 0
```

## Review outcome

Round 1 requested the mixed valid/unknown dictionary test; it was added. Round 2 approved with no findings. No review cap was reached.

## Follow-ups and risks

No unresolved risks, accepted deviations, or follow-ups.
