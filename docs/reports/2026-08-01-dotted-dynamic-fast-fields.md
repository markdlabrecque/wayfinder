# Issue #176: dotted dynamic fast-field coverage

## Spec

Add regression coverage proving that a dotted field name matched by a fast dynamic-field rule uses the same Tantivy columnar key on write and read paths for both sorting and field faceting.

## Changed behavior

- Extended the dotted dynamic-field test schema with a fast `ss_*` string rule.
- Added a sort test for `ss_region.code` that asserts value order rather than fallback document order.
- Added a facet test for the same dotted shape that asserts exact, non-empty bucket counts.
- No production behavior changed.

## Verification

- `cargo test --test dotted_dynamic_fields` — passed, 8 tests.
- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test` — passed.

## Review

Independent read-only review approved with no findings. The reviewer reran the full gate and confirmed that missing-column sort fallback would produce insertion order, not the asserted order, and that both sort and facet production paths open the resolved fast column.

## Follow-ups

None.
