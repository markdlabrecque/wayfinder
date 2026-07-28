# Report: server config TOML + tuning knobs (issue #12)

- Branch: `server-config` (worktree off `main` @ `f3f8aa4`)
- Commit: `308e692`
- Scope: PRD §6 — a second, server-level TOML file distinct from the per-core
  schema, with every knob optional and a missing file meaning all defaults.

## Pipeline deviation (read this first)

The task spec asked for the four-stage subagent pipeline (test-writer →
implementor → reviewer → reporter). This agent runs as a **fork**, and the fork
contract forbids spawning subagents, so the stages could not be delegated. They
were executed in order by one agent instead:

1. Integration tests written first and **confirmed red** for the right reason —
   `cannot find function app_with_config in crate wayfinder` and
   `cannot find ServerConfig in wayfinder`, no other errors.
2. Implementation to green, with no edits to the red tests.
3. A self-review pass over the diff (findings below), not an independent one.
4. This report.

**Consequence:** the review is not independent, and the module-level unit tests
in `src/config.rs` were authored alongside the implementation rather than
red-first. This work would benefit from a genuine `reviewer` pass.

## What was built

New `src/config.rs` (~200 lines + unit tests): `ServerConfig` and four sections
(`indexing`, `query`, `resources`, `commit`) plus top-level `strict_params`,
each `#[serde(deny_unknown_fields, default)]`, so a partial section keeps the
other defaults and any typo — key or section — fails by name.

Interface, per the orchestrator constraint that overrode the issue's proposed
signature change:

```rust
pub fn app(schema_path, data_dir)                    // unchanged: all defaults
pub fn app_with_config(schema_path, data_dir, config) // new
```

Zero existing call sites changed; the 12 tracer-bullet tests still pass
untouched, which is the evidence for that. `src/main.rs` reads the config path
from `WAYFINDER_CONFIG` (unset, or a path that does not exist, means defaults).

### Knobs that are live

| Knob | Effect | How it is proven |
|---|---|---|
| `strict_params` | unknown request param → 400 in Solr's error envelope (code + `responseHeader.status` mirroring HTTP status, per finding 10) | 4 integration tests, `/select` and `/update` |
| `query.rows_limit` | clamps `rows`; `numFound` unaffected | 2 integration tests |
| `indexing.writer_heap` | reaches `IndexWriter` | a heap below Tantivy's minimum is a **startup error**, not a silent fallback |
| `indexing.writer_threads` | reaches `IndexWriter` | indexes and searches correctly at 2 threads |
| `indexing.merge_policy` (+ `merge_min_layer_size`, `merge_level_log_size`) | `LogMergePolicy` or `NoMergePolicy` on the writer | `no_merge` accepted; unknown value rejected by name |
| `resources.doc_store_compression`, `doc_store_blocksize` | Tantivy `IndexSettings` at index creation | asserted by reading `index_settings` out of Tantivy's own `meta.json` |

### Knobs parsed and exposed but deliberately inert

Documented as such in `src/config.rs`, the README table, and marked with
`ponytail:` comments where they are declared:

- `query.time_allowed` — Tantivy has no query deadline; enforcing it needs a
  deadline check inside the collector.
- `query.facet_limit_max` — `facet.limit` itself is not implemented (issue #3).
- `commit.autocommit_max_docs` / `autocommit_max_time` — consumed by issue #9.
- `resources.searcher_pool_size` — no Tantivy 0.26 equivalent; searchers are
  created on demand, not pooled. Kept because PRD §6 names it.

An inert knob is a small lie unless it is labelled, so each is labelled in all
three places rather than quietly accepted.

### Decision recorded

`rows_limit` **clamps** rather than 400s. Solr has no equivalent request cap, so
there is no captured behaviour to match; clamping keeps a client that asks for
too much working, where a 400 would break it. Documented in the README.

## Test evidence

`command cargo test` (bypassing shell aliases), all suites:

```
running 8 tests   (src/lib.rs unit tests — src/config.rs)
test config::tests::empty_config_is_all_defaults ... ok
test config::tests::a_partial_section_keeps_the_other_defaults ... ok
test config::tests::missing_file_is_all_defaults ... ok
test config::tests::index_settings_reflect_the_resource_knobs ... ok
test config::tests::bad_enum_values_are_rejected_by_value ... ok
test config::tests::zero_writer_threads_is_rejected ... ok
test config::tests::unknown_key_in_a_section_is_rejected_by_name ... ok
test config::tests::unknown_key_is_rejected_by_name ... ok
test result: ok. 8 passed; 0 failed

running 18 tests  (tests/server_config.rs)
test empty_config_file_means_all_defaults ... ok
test no_merge_policy_is_accepted ... ok
test strict_params_allows_every_implemented_param ... ok
test commit_and_budget_knobs_parse_and_are_exposed ... ok
test rows_limit_clamps_a_larger_requested_rows ... ok
test strict_params_rejects_unknown_param_on_update ... ok
test doc_store_knobs_reach_the_tantivy_index_settings ... ok
test missing_config_file_means_all_defaults ... ok
test unknown_doc_store_compression_is_rejected ... ok
test strict_params_rejects_unknown_param_with_solr_error_envelope ... ok
test strict_params_still_accepts_the_commit_param_on_update ... ok
test unknown_key_inside_a_section_is_rejected_by_name ... ok
test rows_below_the_limit_is_untouched ... ok
test unknown_merge_policy_is_rejected ... ok
test unknown_section_is_rejected_by_name ... ok
test unknown_top_level_key_is_rejected_by_name ... ok
test writer_heap_below_tantivys_minimum_is_a_startup_error ... ok
test multiple_writer_threads_still_index_and_search ... ok
test result: ok. 18 passed; 0 failed

running 12 tests  (tests/tracer_bullet.rs — unchanged, no edits)
test result: ok. 12 passed; 0 failed
```

38 tests total, hermetic (no network, no Docker). `cargo fmt --check` clean.
`cargo clippy --all-targets -- -D warnings` — the exact CI command — passes
clean.

Note on the two clippy warnings the tracer-bullet report listed as pre-existing
(`result_large_err` `src/lib.rs:68`, `collapsible_if` `src/lib.rs:131`): they do
**not** reproduce on this toolchain, before or after this change. They were not
deliberately fixed here; CI's `-D warnings` currently passes either way.

## Self-review findings

Nothing was found that warranted a code change. Recorded for the reviewer:

1. `strict_params = true` rejects params Wayfinder does not implement yet —
   `sort`, `facet.limit`, `facet.mincount`, `json.nl`, `commitWithin`,
   `overwrite`, `softCommit`. That is the flag's purpose (gap discovery), and
   the default is off, so no captured-Solr behaviour is contradicted. Each
   sibling branch should add its params to `SELECT_PARAMS` / `UPDATE_PARAMS` in
   `src/lib.rs`.
2. `Compressor::Lz4` depends on Tantivy's default `lz4-compression` feature.
   Building with `--no-default-features` would not compile `src/config.rs`. No
   guard added — the project uses default features.
3. Doc-store settings apply only at index creation; re-opening keeps what the
   index was built with (Tantivy's rule). Commented at the call site and in the
   README, but only the creation path is tested.
4. `rows_limit = 0` would clamp every request to zero rows. Operator's choice,
   not validated against.

## Follow-ups

1. `strict_params`/clamp errors use the existing hand-rolled `error_response`.
   **This wants rebasing onto issue #11's central error type** once that lands —
   flagged as instructed rather than building a second envelope.
2. Enforce `query.time_allowed` (needs a collector-level deadline).
3. Apply `query.facet_limit_max` when `facet.limit` lands (#3).
4. Consume `commit.autocommit_*` in the update pipeline (#9).
5. Get an independent `reviewer` pass over this diff — see the pipeline
   deviation above.

## Pointers

- New: `src/config.rs`, `tests/server_config.rs`, `README.md`
- Modified: `src/lib.rs` (module + `app_with_config` + `check_params` + rows
  clamp), `src/core_index.rs` (`open` takes `&ServerConfig`; index settings,
  writer threads/heap, merge policy), `src/params.rs` (added `keys()`),
  `src/main.rs` (`WAYFINDER_CONFIG`)
- Untouched by design: `src/schema.rs` (#10), `tests/common/mod.rs` and
  `tests/tracer_bullet.rs` (#1), `solr-ref/` (no new fixtures needed)
