# Schema name guards: reject duplicate names and builtin-shadowing field types

- Issues: #173, #170
- PR: #195
- Branch: `173-schema-name-guards`
- HEAD: `ef8f356`

## What was built

**#173** — exact-duplicate `[[fields]]` `name`s and `[[dynamic_fields]]` `pattern`s are now
load-time `anyhow` errors, guarded before any `add_*_field` call (`src/schema.rs`). Overlapping
globs (`tm_*` alongside `tm_X3b_*` and a bare `*`) stay legitimate; only exact duplicates fail.
Both guards are keyed on the name/pattern alone, not on the name plus type, so a duplicate is
rejected regardless of whether the two declarations agree on configuration.

**#170** — all 26 names from `schema::builtin_type_names()` plus `TEXT_EN_TOKENIZER` are now
reserved against `[[field_types]]`, replacing a check that protected only `text_en`. Outright
rejection at load time, no override path. `boolean` is deliberately not reserved — see below.

`src/lib.rs`'s `schema_fieldtypes` de-duplication filter for a shadowed built-in name is now dead
code (shadowing can no longer occur), and is kept as defence-in-depth with a comment explaining
why, rather than deleted.

`docs/schema.md` gained two new subsections, "Reserved names" and "Duplicate names", documenting
both guards, the exact 26+1-name reserved set, and the deliberate non-reservation of `boolean`.
Neither was previously documented.

## Ticket premises that were wrong

Both tickets had a factual premise that did not match the actual system, and correcting each was
the most valuable part of the work:

1. **#173** claimed two same-named `[[fields]]` entries create two Tantivy fields under one name,
   with `field_handles` "last-wins," silently orphaning the first. Checked against tantivy 0.26.1:
   `SchemaBuilder::add_field` (`src/schema/schema.rs:202` in the tantivy source) *panics* on a
   second field with the same name — no orphan is ever allocated, there is no silent-loss path
   for `[[fields]]`. The real defect is narrower than described: a `schema.toml` typo crashes the
   whole process from inside a dependency, instead of producing an ordinary schema-load error like
   every other mistake in that file. The `[[dynamic_fields]]` half of the ticket, by contrast, was
   accurate and genuinely silent: `match_dynamic`'s `max_by_key(|d| d.pattern.len())` returns the
   *last* of two equal-length patterns, so an earlier duplicate rule is dead code, and if the two
   duplicates carry different types they also disagree about which catch-all container
   (`_dynamic` vs `_dynamic_text`) the values belong in.
2. **#170** listed `boolean` as an unprotected builtin. It is not a builtin at all: there is no
   `resolve_type` match arm for it, and it is absent from `builtin_type_names()`. Confirmed
   directly against `src/schema.rs`'s `resolve_type` function during this report — no `"boolean"`
   or `"bool"` string appears anywhere in its match arms. `docs/schema.md` now states this
   explicitly as a deliberate non-reservation, not an oversight.

## Review outcome

Independent reviewer (Opus), 2 rounds, **approved**. Round 1 raised two must-fix findings, both
real and both fixed before approval:

- The reserved-list comment originally claimed the list was derived so that a new builtin could
  never be left shadowable. True for the 17 language-derived names (from `LANGUAGES`) but false
  for the 9 in `NON_LANGUAGE_BUILTIN_TYPES`, a second hand-written copy of `resolve_type`'s match
  arms that nothing in the type system keeps in sync with it. A future
  `"boolean" => ResolvedType::Bool` arm could be added to `resolve_type` without updating that
  list, silently reopening #170's exact bug with the whole suite still green. Fixed by adding an
  expiring guard test, `type_names_absent_from_the_reservation_list_are_still_unresolvable`
  (`tests/schema_layer.rs:803`), which asserts a fixed set of plausible future names remain
  unresolvable; it fails, and names the two lists to update, the moment one of them gains a
  `resolve_type` arm.
- Stage 1's rewrite of `schema_fieldtypes_custom_chain_shadowing_a_builtin_is_reported_once`
  (inverted, since shadowing is now a load error rather than a runtime state) dropped the suite's
  only assertion that a non-shadowing custom `[[field_types]]` chain reports
  `class == "solr.TextField"`. Restored in the rewritten test
  (`schema_fieldtypes_custom_chain_shadowing_a_builtin_is_rejected_at_load_time`,
  `tests/schema_fieldtypes.rs:429`) on a second, non-shadowing `text_de_custom` chain, with a
  comment recording that this is now the only place in the suite that assertion lives.

A follow-up pass widened the expiring guard from four names to nine, adding Solr's point-type
aliases `pint`/`plong`/`pfloat`/`pdouble`/`pdate` — modern Solr's names for types Wayfinder already
has under other names, and so the likeliest future `resolve_type` arms on a wire-compatible
backend; `pdate` already appears in a captured fixture trace. This guard is a **heuristic net, not
a proof**: a test cannot enumerate `match` arms exhaustively, so no fixed list of plausible names
closes the underlying hole (an un-synced second copy of `resolve_type`'s arms) completely. Record
this limitation explicitly rather than treat the guard as a fix for the root problem.

Reviewer reproduced the test suite and mutation results independently in a SHA-verified `git
archive` copy, rather than accepting the stage reports' claims at face value.

## Mutation testing

Seven mutants, each introduced deliberately, confirmed to be caught by a named test, then
reverted with a targeted `Edit` (never `git checkout`):

1. Fields duplicate guard keyed on `(name, type_)` instead of `name` alone → caught by
   `duplicate_field_names_are_rejected_when_differently_configured` (and, per the reviewer's
   check, nothing else regressed).
2. Fields duplicate guard reduced to an adjacent-only `windows(2)` scan → caught by
   `duplicate_field_names_are_rejected_when_separated_by_other_fields`.
3. Dynamic-fields duplicate guard keyed on `(pattern, type_)` → caught by
   `duplicate_dynamic_field_patterns_are_rejected_when_differently_configured`.
4. Dynamic-fields duplicate guard reduced to adjacent-only → caught by
   `duplicate_dynamic_field_patterns_are_rejected_when_separated_by_other_rules`.
5. #170 guard narrowed back to `["text_en", TEXT_EN_TOKENIZER]` → caught by three tests:
   `every_builtin_field_type_name_is_reserved`,
   `formerly_silent_shadowing_of_a_non_text_en_builtin_is_now_rejected`, and
   `schema_fieldtypes_custom_chain_shadowing_a_builtin_is_rejected_at_load_time`.
6. `"string"` dropped from `NON_LANGUAGE_BUILTIN_TYPES` → still caught, but not by the reservation
   loop — by the pre-existing `core.unique_key must be an unanalyzed string-typed field`
   precondition, since the fixture's `id` field is `type = "string"`.
7. `"boolean"` added to `resolve_type`'s `Str` arm → caught only by the new expiring guard, at its
   `Ok(_)` expiry arm, with a diagnostic naming both lists (`builtin_type_names()` /
   `NON_LANGUAGE_BUILTIN_TYPES`) to extend.

Mutant 5 deserves a note beyond "caught": with the guard narrowed, `name = "string"` still
produced an error, but the *wrong* one — the unrelated
`core.unique_key must be an unanalyzed string-typed field` error, because the fixture's uniqueKey
field `id` is `type = "string"` and shadowing that type retypes the uniqueKey out from under it.
An `expect_err`-only assertion would have passed against this mutant. It was the
`contains("reserved")` substring assertion on the error text that actually caught it. This is
direct, reproduced evidence for asserting on error *content*, not merely on `is_err()`, in
validation-guard tests.

## Test evidence

- `cargo test`: **825 passed across 41 suites**, reproduced directly for this report
  (`cargo test` → `cargo test: 825 passed (41 suites, 46.52s)`).
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean (CI's exact invocation).
- Both gates and all seven mutant/revert cycles were also reproduced independently by the
  reviewer in a SHA-verified `git archive` copy of the branch, not merely restated from the
  implementor's report.

## Documentation

`docs/schema.md` gained:
- A "Reserved names" subsection: the full reserved set, why shadowing is dangerous (silent
  numeric-to-text retyping breaking range queries and sort), and the deliberate non-reservation of
  `boolean`.
- A "Duplicate names" subsection: both new duplicate guards, and an explicit statement that
  overlapping globs remain legitimate and only exact duplicates fail.

Both subsections describe behaviour (26 reserved names hard-failing at load with no override) that
was previously undocumented.

## Follow-ups deferred by the reviewer

- **Filed as issue #194** (open): a `[[fields]]` entry named `_dynamic` or `_dynamic_text`,
  alongside any `[[dynamic_fields]]` rule, reaches the same tantivy `add_field` panic by a
  different door — the catch-all fields are not `[[fields]]` entries, so they never enter the
  #173 duplicate-name guard's tracked set. Reviewer confirmed this is the *only* remaining route to
  that panic on this branch, and that it requires at least one dynamic rule present to trigger.
- **Precedence wart, accepted as-is**: at the dynamic-pattern duplicate guard, two identical
  *invalid* patterns now report `duplicate dynamic field pattern` rather than the pre-existing
  `is not supported`, because the duplicate check runs before `validate_pattern`. Judged
  deterministic and acceptable — both diagnoses are true and either sends the operator to the same
  line of `schema.toml` — and recorded in a code comment so it reads as a decision, not an
  oversight.
- **Process note for future batches**: stage 1 (tests) and stage 2 (implementation) landed in a
  single combined commit on this branch (`f7ea30b`), so "the implementor edited no test" was not
  verifiable from `git` history alone; the reviewer had to validate the test changes on their
  merits instead of by diff provenance. Stage 1 should commit separately from stage 2 in future
  waves so this is checkable mechanically.

This work went through the full 2-round reviewer cap with concrete must-fix findings surfaced and
resolved in round 1; it was not rubber-stamped, but per the pipeline's default cap, only 2 rounds
were run. The reviewer treated the second round as sufficient to approve, but the general
observation from the pipeline rules stands: a 2-round cap is a default, not a proof the diff has no
further issues a third pass might find.

## Discrepancies found while writing this report

None. Every claim in the handoff summary — the diff shape, both wrong ticket premises, the two
round-1 review findings and their fixes, all seven mutants and the tests that caught them, mutant
5's specific false-positive-then-real-catch story, the test/suite counts, gate cleanliness, and the
three follow-ups — was checked directly against `git diff 2640d4e..HEAD`, `src/schema.rs`'s actual
`resolve_type` arms, a fresh `cargo test`/`cargo fmt --check`/`cargo clippy` run, and `gh issue view
194`, and all matched.
