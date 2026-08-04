# Batch specs — 2026-08-04 source-sweep follow-ups

Specs for handoff. Each file is self-contained: it states the premises to verify
before writing code, the files it owns, the files its siblings own, and what
"done" means. Read `CLAUDE.md` at the repo root first — the workflow (claim the
issue, branch `<issue>-<slug>`, PR with `Closes #<n>`, CI is the gate) and the
compatibility contract (fixtures are ground truth) apply to every one of these.

## Sequencing

Two prep items land on `main` before anything fans out. This is not optional:
both are shared infrastructure that every branch in the batch depends on.

| Order | Item | Why it blocks |
|---|---|---|
| 1 | `PREP-1-vendor-source.md` | Every spec below cites line numbers in `search_api_solr` 4.4.0 files that are **not in the repo**. Without it none of the premises are checkable. |
| 2 | `357-online-snapshot-flake.md` | A parallel worktree batch is sustained CPU contention, which is exactly what makes this test flake. Fan out first and every branch's gate becomes unreliable. |

After both land, three groups. Within a group, branches are safe to run
concurrently; across groups, respect the order.

**Group A — Drupal module, no `src/lib.rs` contention.** Run all five at once.

- `358-string-sort-copy.md`
- `359-spellcheck-multi-dictionary.md`
- `360-extended-results-shape.md`
- `361-querybuilder-fl.md`
- `362-sort-copy-fanout.md` (measurement first; may end in "no change")

**Group B — `src/lib.rs` contention.** `350` and `353` both add to
`SELECT_PARAMS`. Give them to the same person, sequentially, or land 353 first
(it is a pure list addition) and rebase 350 onto it.

- `353-highlight-params.md`
- `350-form-encoded-post.md`

**Group C — new endpoints, sequenced by dependency.**

- `352-suggest-buildall.md` — no dependencies, land first
- `351-autocomplete-endpoint.md` — depends on 352 (suggest component), 359 and
  360 (spellcheck component)
- `354-admin-endpoints.md` — **owns the coverage denominator**; land last so it
  recomputes against the final endpoint set
- `355-finding-132-amendment.md` — docs only, land any time

## Shared contracts

These are the things that actually conflict. Decided once, here, so no branch
invents its own answer.

**`docs/solr-ref-findings.md` amendments.** The file's convention is to *append*
a numbered finding. Two specs (351, 355) need to *correct an existing* one. The
convention for that, from now on: **leave the original finding's text in place,
append a bold `**Amended by finding N (YYYY-MM-DD):**` line to its end**, and
write the correction as a new numbered finding at the bottom. Never edit a
finding's body in place — other documents cite these by number and content, and
`tests/finding_citations.rs` checks the citations.

**`SELECT_PARAMS` in `src/lib.rs:198`.** Alphabetical within the existing
grouping. Add entries only; never reorder the list, since that turns every
addition into a conflict.

**The coverage denominator.** `354` owns it. If your endpoint changes the
endpoint count, say so in your PR body and leave the number alone — 354
recomputes once, at the end. See #225 for what the number is allowed to claim.

**Fixture capture.** Append your `capture.sh` block **at the end of the file**,
never in the middle. Never re-run the whole script: it rewrites every fixture and
the `QTime`/`_version_`/`rid` churn dirties every branch. Use
`capture.sh --only <prefix>`. Fixtures your branch captures but has not committed
are untracked, and `git checkout -- solr-ref/` will not restore them — commit
them before doing anything else.

## Standing rules that apply to all of these

- **Tests come before implementation, and are confirmed red for the right
  reason first.** A test that passes before the implementation exists is not
  evidence of anything.
- **Expected values come from fixtures in `solr-ref/responses/`**, never from
  what the implementation happens to produce. If they disagree, the
  implementation is wrong — do not widen a normaliser to hide it.
- **Any deliberate skip or descope gets a guard that fails when its reason stops
  holding.** See `tests/version_descope_guard.rs` and
  `tests/edismax_descope_guard.rs` for the pattern. A skip nobody rechecks looks
  like coverage and is not.
- **Do not paper over a wrong premise.** Several specs below flag premises that
  the repo contradicts. If you find another, say so in the PR and stop — the
  correction is the work, not an obstacle to it.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must both
  be clean. That clippy invocation is CI's exact command.
- Substantive work leaves a report at `docs/reports/YYYY-MM-DD-<slug>.md`; the PR
  body summarises and links it rather than retyping it.
