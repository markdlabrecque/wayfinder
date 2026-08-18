# Wayfinder — working agreement

A Solr-wire-compatible search backend in Rust on Tantivy. Read `docs/COMPATIBILITY.md` before
changing routes, parameters, envelopes, or scope boundaries. Read `docs/CONFIGURATION.md` before
changing schemas or server settings.

## Workflow

**Every change goes through a PR. No direct commits to `main`.**

1. **Update local `main` from the remote before branching off it.** A branch cut from a
   stale local `main` is not built on the latest base — a green branch plus a stale base does
   not prove a green merge. Sync first (`git fetch origin` then `git pull --ff-only` on
   `main`); in a secondary worktree where `main` is checked out elsewhere, branch directly
   off `origin/main` (`git checkout -b <branch> origin/main`) and sync `main` in its own
   worktree.
2. **Claim the issue before starting.** Assign it to yourself
   (`gh issue edit <n> --add-assignee @me`) and say in a comment that you are picking it up.
   An unassigned issue is fair game for someone else; claiming it is what makes parallel work
   safe. Do this before writing code — not when opening the PR.
3. **Branch name starts with the issue number**: `<issue>-<short-slug>`, e.g. `10-schema-layer`,
   `2-sort-parameter`. Off `main`, worktree-friendly. Work with no issue behind it (a chore,
   a doc fix) uses a plain descriptive slug and says in the PR why there is no issue.
4. **Open a PR** with `Closes #<issue>` in the body, so the merge closes the issue
   automatically. Do not close issues by hand — the PR does it, and a hand-closed issue loses
   the link to the change.
5. **CI is the gate, not a notification.** `.github/workflows` runs fmt, clippy, and tests.
   Wait for green before merging; do not merge a PR with checks pending or failing.
   Self-approval is not required — merge your own PR as soon as checks pass.
6. **Rebase onto `main` before merging** when other branches have landed in the meantime, and
   re-run the gates locally afterwards. Concurrent branches here routinely conflict in
   `src/lib.rs` and `tests/common/mod.rs`; a green branch plus a green `main` does not imply a
   green merge.
7. **Record substantive work in the PR.** Summarise changed behavior, verification, review, and
   unresolved follow-ups in the PR body instead of adding repository-local implementation reports.

## Hot files, for parallel work

The general rules for running several issues at once are in the global `~/.claude/CLAUDE.md`
("Parallel batches"). What is specific to this repo is *which* files contend. Every v1 issue
wants some of these, so assign ownership per branch before starting:

| File | Who touches it | Notes |
|---|---|---|
| `src/lib.rs` | almost every issue | routing, handler bodies, `SELECT_PARAMS`/`UPDATE_PARAMS` |
| `src/core_index.rs` | search/index features | the index-creation call is a repeat conflict site |
| `tests/common/mod.rs` | every test suite | shared helpers; the `dead_code` allow lives here as an inner attribute — do not add a second one on `mod common;` |
| `solr-ref/FINDINGS.md` | historical wire/client evidence | retain numbered findings; product scope lives in `docs/COMPATIBILITY.md` |

Sequencing that has already proven necessary: error-envelope work lands before features that
produce errors, and schema-layer work lands before features that need new field types.

## Compatibility contract

- Wayfinder retains its Solr-compatible wire because that is how it was built and how existing
  clients reach it; Solr parity is not an ongoing goal.
- **Fixtures in `solr-ref/responses/` are the frozen regression baseline.** Expected values come
  from fixtures, never from implementation output. They remain factual evidence for the shipped
  wire and do not create product scope.
- No new Solr-parity work or captures are planned. Do not alter the frozen fixtures to make an
  implementation pass; preserve fixture-derived assertions for the behaviours they record.
- **Wire format only.** The current wire uses Solr parameter names, semantics, and JSON envelopes,
  never Solr configuration files.
- `solr-ref/FINDINGS.md` records historical Solr and client evidence. Current deliberate
  differences and unsupported boundaries live in `docs/COMPATIBILITY.md`.
- Implementing a new request param? Add it to `SELECT_PARAMS`/`UPDATE_PARAMS` in `src/lib.rs`,
  or `strict_params = true` will 400 a param Wayfinder actually supports.

## Testing

- `cargo test` must be green and stay hermetic: no network, no Docker.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must both be clean. That
  clippy invocation is CI's exact command.
- Tests come before implementation, and are confirmed red for the right reason first.
- Code whose whole value is *failing* correctly (a validation check, a compatibility guard) gets
  mutation-tested: break it deliberately, confirm a test catches it, revert.

## Conventions

- Conventional commits (`feat(schema):`, `fix(config):`, `test:`, `docs:`). ASCII-only messages.
- Prefer root-cause fixes over surface workarounds; prefer the smallest change that holds.
- `ponytail:` comments mark deliberate simplifications and name the ceiling — leave them in.
- Don't paper over a wrong ticket premise. Three v1 issues so far stated things that captured
  Solr or the Tantivy source contradicted; the right move each time was to flag the correction,
  not silently build to the wrong spec.
