# Issue #232 — `/select` ignored-parameter warnings

## Goal and scope

Objective: make ignored function-query parameters visible, not implement function queries. Fixture inventory found no committed fixture, manifest row, or Search API trace containing `bf` or function-query `boost`; it found only numeric `boost` and implemented `bq`. Spellcheck is implemented by #223 and is excluded. Accepted harmless parameters such as `TZ` and `function` are not consequential under current semantics.

## Implementation

`src/lib.rs` now adds `responseHeader.warnings` for any `bf` and for non-`f32` `boost`, with the exact warning that the parameter is ignored because function queries are unimplemented. Numeric `boost` and `bq` do not warn. This warning precedes and coexists with facet warnings, and is absent otherwise. Coverage is in `tests/select_warnings.rs`.

## Evidence and review

- Initial targeted test: failed 2/3 because warnings were absent.
- Targeted test after implementation: passed.
- Mutation check: forcing the `bf` condition false made the targeted `bf` test fail; the mutation was restored.
- Implementer full gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed.
- Independent Reviewer full gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed.
- Reviewer verdict: **APPROVE**; no findings.

## Follow-ups and risks

No accepted deviations, deferred follow-ups, or unresolved risks. Function-query implementation remains v4 scope.
