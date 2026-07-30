# Issue #99: internal `_version_` field

## Goal

Implement the PRD's narrowed v3 `_version_` scope: every successfully indexed document receives an internal monotonic `i64` fast field, and the existing stats component can return its maximum in the request shape used by `search_api_solr`. Atomic updates and optimistic concurrency remain out of scope.

## Plan

1. Verify real Solr behavior for `stats.field=_version_&function=max(_version_)`, capturing and committing a focused fixture plus capture/manifest/findings updates if no existing fixture proves it.
2. Add a minimal fixture-backed hermetic test that proves the request succeeds, uses the existing stats envelope, and returns the captured maximum; add focused schema/index tests for the internal field's `i64` fast options, automatic population, and monotonic increase.
3. Confirm those tests fail for the missing `_version_` behavior before changing production code.
4. Add `_version_` to the Tantivy schema internally rather than to `schema.toml`; make schema metadata resolve it as a statable numeric fast field without exposing it as a user-defined field.
5. Seed a per-core atomic version source from current Unix-epoch milliseconds and allocate increasing values while indexing documents. Assign only to documents that pass validation and reach `IndexWriter::add_document`; preserve monotonicity across calls and within batches. Document restart behavior and the guarantee boundary.
6. Route `_version_` requests through the existing `stats` validation and aggregation path, with no new aggregation implementation. Support the captured `function=max(_version_)` request shape only to the extent real Solr requires.
7. Mutation-check the version assignment/counter behavior by temporarily breaking it and proving the targeted test fails, then restore it.
8. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`; obtain review approval and record behavior, fixture evidence, mutation evidence, gates, and follow-ups in `docs/reports/2026-07-30-99-version-field.md`.
9. Commit conventionally, push `99-version-field`, open a PR containing `Closes #99` and linking the report, then wait for green CI before merge.

## Acceptance criteria

- `_version_` exists in every successfully indexed Tantivy document as an internal `i64` fast field and is absent from user `schema.toml` configuration.
- Version values increase monotonically per core across documents and `add_documents` calls; startup seeding deliberately avoids ordinary pre-restart collisions and is documented.
- A captured Solr fixture establishes the accepted `stats.field=_version_` / `function=max(_version_)` request and response behavior.
- Existing stats aggregation returns `_version_` metrics, including the correct maximum, without a parallel aggregation path.
- No atomic-update modifiers, update-version responses, stale-write checks, or 409 behavior are added.
- Targeted red evidence, mutation evidence, the full local gate, and reviewer approval are recorded.
