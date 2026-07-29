# Report: unique_key contract (issue #40)

- Branch: `40-unique-key-contract`
- Scope: `src/schema.rs`, `tests/schema_layer.rs` only — no changes to
  `src/lib.rs`, `src/core_index.rs`, or `solr-ref/`.
- Commits: `3310b91` (fix), `b6b952d` (test coverage for round-1 review fix)

## Summary

Issue #40 was opened as a follow-up from the #9 update-pipeline review: the
load-time check on `core.unique_key` verified the field was string-*kind*
but left two gaps open that would surface as silent, wrong behaviour at
request time rather than a load-time refusal:

1. A `multi_valued` unique_key was accepted at load. The update pipeline
   resolves `core.unique_key` as a single Tantivy term
   (`Term::from_field_text`, `src/core_index.rs:397,446`), which has no
   defined meaning for a field holding multiple values — overwrite/delete-by-id
   would be undefined for a doc like `id: ["a", "b"]`.
2. An *analyzed* text unique_key (e.g. `text_en`) passed the old check.
   `value_kind_of` folds both `ResolvedType::Str` (`string`/`keyword`, raw)
   and `ResolvedType::Text { .. }` (analyzed presets and custom
   `[[field_types]]` chains) into the same `ValueKind::Text`, so the check
   couldn't distinguish them. But `Term::from_field_text` builds a single
   exact term from the raw value — an analyzed field would tokenize
   `"Hello World"` into `["hello", "world"]` at index time, so the document
   would no longer match itself by uniqueKey term lookup.

## Fix

### Gap 1 — analyzed unique_key

Before:

```rust
let unique_key_kind = value_kind_of(&unique_key_field_config.type_, &parsed.field_types)
    .with_context(|| format!("on core.unique_key `{}`", parsed.core.unique_key))?;
if unique_key_kind != ValueKind::Text {
    bail!(
        "core.unique_key `{}` must be a string-typed field (`string`/`keyword`), got \
         `{}` ({unique_key_kind:?}) — the update pipeline resolves the uniqueKey as a text \
         term",
        parsed.core.unique_key,
        unique_key_field_config.type_,
    );
}
```

After — narrowed from `value_kind_of(...) == ValueKind::Text` (folds `Str`
and analyzed `Text{..}` together) to `resolve_type(...) == ResolvedType::Str`
(only the unanalyzed variant):

```rust
let unique_key_resolved_type =
    resolve_type(&unique_key_field_config.type_, &parsed.field_types)
        .with_context(|| format!("on core.unique_key `{}`", parsed.core.unique_key))?;
if !matches!(unique_key_resolved_type, ResolvedType::Str) {
    bail!(
        "core.unique_key `{}` must be an unanalyzed string-typed field (`string`/`keyword`), \
         got `{}` — the update pipeline resolves the uniqueKey as a single exact text term \
         via `Term::from_field_text`, and an analyzed type (e.g. `text_en`, `text_general`, \
         or a custom analyzed [[field_types]] chain) would tokenize the value so a document \
         no longer matches itself",
        parsed.core.unique_key,
        unique_key_field_config.type_,
    );
}
```

This is a root-cause fix, not the issue's softer alternative (widening the
error message to warn about analyzed types while still accepting them). The
old check is deleted outright; `ResolvedType::Str` is the only shape
`Term::from_field_text` can safely resolve against for the update pipeline's
exact-match semantics.

No new live-Solr fixture capture was done to back this — the issue listed
that as optional/"if cheap" only. The decision to narrow to
`ResolvedType::Str` is reasoning from the existing `Term::from_field_text`
call sites in `src/core_index.rs`, not a new captured behaviour.

### Gap 2 — multi_valued unique_key

New check added directly after the type check:

```rust
if unique_key_field_config.multi_valued {
    bail!(
        "core.unique_key `{}` must not be multi-valued — the update pipeline resolves the \
         uniqueKey as a single term, and a multi-valued field has no single value to \
         resolve against",
        parsed.core.unique_key,
    );
}
```

### Beyond the issue's ask — required = true enforcement

The issue only said "consider requiring required=true" as an option; this
work went further and made it a hard load-time rejection:

```rust
if !unique_key_field_config.required {
    bail!(
        "core.unique_key `{}` must be declared with `required = true` — every document \
         needs a value for the field the update pipeline overwrites/deletes by",
        parsed.core.unique_key,
    );
}
```

Why this is safe to make a hard requirement rather than a warning: every
existing schema fixture in the repo (all `tests/*` schema TOMLs, including
`FULL_SCHEMA_TOML` in `tests/schema_layer.rs`) already declares
`required = true` on its unique_key field. This was verified by reading the
fixtures rather than assumed — no existing schema needed changing to keep
passing after adding this check, confirmed by the full green `cargo test`
run below.

## Tests added (`tests/schema_layer.rs`)

- `multi_valued_unique_key_is_rejected_at_load_time` — asserts a
  `multi_valued = true` unique_key field fails to load, error names
  `unique_key` and the field.
- `analyzed_unique_key_is_rejected_at_load_time` — asserts a `text_en`
  unique_key field fails to load, same error-content assertions.
- `non_required_unique_key_is_rejected_at_load_time` — asserts a
  `required = false` unique_key field fails to load, error also names
  `required`. Added in the round-2 remediation commit (`b6b952d`) after
  reviewer feedback; not present in the initial fix commit.
- `single_valued_string_unique_key_still_loads` — control: the exact shape
  the two rejection tests above are carving out as forbidden must still
  load cleanly.

(`non_string_unique_key_is_rejected_at_load_time`, the pre-existing test from
the #9 review-round-1 fix, is unchanged and continues to pass — confirms the
non-`Str` rejection path for a non-text type, e.g. `int`, still works after
the narrowing.)

## Review history

- **Round 1**: reviewer found one must-fix — the new `required = true` check
  in commit `3310b91` shipped with zero test coverage. Mutation-testing it
  (deleting the check) would have left the suite green, meaning the check's
  entire value (loud refusal) was unverified. Per the pipeline's mutation-
  testing convention for validation/compatibility-guard code, this is
  exactly the class of gap the convention exists to catch.
- **Fix**: implementor added `non_required_unique_key_is_rejected_at_load_time`
  in commit `b6b952d`, closing the gap. During round-2 remediation the
  implementor's working copy transiently held a dead `if false && ...`
  branch around the required check (would have tripped clippy as a logic
  bug) but this was corrected before the commit was made — reading the
  current `src/schema.rs` (lines ~570-615, shown in full under "Fix" above)
  confirms there is no dead branch in the landed code.
- Work stayed within round 2; no third round or orchestrator escalation was
  needed. That said, per the pipeline's cap-out convention, two review
  rounds is fewer passes than a change to a compatibility-guard path might
  ideally get — recorded here per the reporting rule that a 2-round cap-out
  must say so.

## Gate results (verified in this worktree, `40-unique-key-contract`)

- `cargo test`: 280 passed across 12 suites (0 failed). `schema_layer`
  itself: 30 passed, including all four new/changed tests above.
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean, no issues.

## Not done

- No live-Solr capture was performed for analyzed-uniqueKey or
  multi-valued-uniqueKey behavior against real Solr. The issue marked this
  optional ("if cheap"); it was not done here. The fix logic is derived from
  reading `Term::from_field_text`'s existing call sites in
  `src/core_index.rs`, not from a new captured fixture. If this is later
  found to diverge from Solr's actual behaviour (e.g. Solr's own handling of
  an analyzed uniqueKey at schema-load time), that would need a fixture and
  a correction, per this repo's compatibility-contract rule that divergence
  from captured Solr behaviour is a bug.
- Review only ran the standard two rounds available to this pipeline; a
  compatibility-guard change of this kind could support a further pass.

## Pointers

- Production code: `src/schema.rs` (unique_key load-time validation block,
  ~lines 570-615)
- Tests: `tests/schema_layer.rs` (unique_key contract section)
- Commits: `3310b91` (fix), `b6b952d` (round-1 review remediation test)
</content>
