# Report: bench run.sh hardening (issue #62)

- Branch: `62-bench-run-hardening`
- Scope: harden `bench/run.sh` against three edge cases (memory-unit
  misparsing, a schema-check that silently discarded the response body,
  an undocumented cold-start timing exclusion), add a hermetic test seam
  for `bench/run.sh` (previously zero coverage), and wire that seam into
  CI (previously never run there at all).

## Defects fixed

1. **`solr_mem_mb` unrecognized-unit fallback.** The awk unit-parsing
   branch matched `GiB`/`KiB` and fell through to a bare `MiB` branch for
   anything else. `512B` (bytes) and `1.5TiB` (tebibytes) both matched
   the fallback and were silently scaled as if they were MiB — a 2000x
   and ~1,000,000x understatement respectively, with no error. Fixed by
   adding explicit `B` and `TiB` branches, anchoring every unit match to
   `^[0-9.]+UNIT$` (so `KiB` can no longer be mistaken for a `B` suffix
   match, and vice versa), and making anything that matches none of the
   five units `exit 1` with a message on stderr naming the unrecognized
   token, instead of defaulting.
2. **Schema add-field response body discarded on failure.** The POST to
   Solr's schema API used `curl -sSf`, so curl itself aborted and
   discarded the response body on any non-2xx HTTP status before the
   script's own `grep -q '"errors"'` check ever ran — that check was
   dead code for the non-2xx case, and a failure produced no diagnostic
   body at all. Fixed by dropping `-f`, capturing the HTTP status via
   `-w '%{http_code}'` and the body via `-o` to a file, and adding a new
   `check_schema_add_field_response(status, body)` function that fails
   loudly with the body on stderr for either a non-2xx status or a 2xx
   status whose body still contains `"errors"` (Solr's schema API can
   return HTTP 200 with an errors payload on a rejected add-field).
3. **`COLD_START_PATH` excludes container-create time.** The cold-start
   timer (`wait_for_ping`) only starts after `docker run -d` returns, so
   the reported cold-start number understates true cold start by
   however long `docker run -d` itself takes to create the container.
   Per the issue, fixed with the "cheap option": a doc comment directly
   above the `docker run -d` line stating the exclusion explicitly,
   rather than restructuring the timing to capture container-create
   time (left as a documented limitation, not a behavior change).

## New test seam

`bench/run.sh` had no test coverage before this change. Added:

- `bench/tests/support/mod.rs` — a bash-function-extraction harness that
  sources individual functions out of `run.sh` for isolated testing
  without running the full script (or Docker).
- `bench/tests/run_sh_mem_units.rs` — pins `solr_mem_mb` behavior across
  `B`/`KiB`/`MiB`/`GiB`/`TiB`, including the two originally-misparsed
  cases (`512B`, `1.5TiB`) and confirms an unrecognized unit now exits
  non-zero instead of defaulting.
- `bench/tests/run_sh_schema_check.rs` — pins
  `check_schema_add_field_response` across a clean 2xx, a non-2xx with
  a body, and a 200-with-`"errors"` body.
- `bench/tests/run_sh_cold_start_doc.rs` — asserts the exclusion is
  documented (regex match on the comment above `docker run -d`), so the
  doc comment can't silently rot away in a later edit.
- `bench/tests/results_table.rs` was reduced from 147 to a smaller
  extraction-harness-based form as part of adopting the shared
  `support/mod.rs` seam.

## Review rounds

- **Round 1**: verified all bash logic correct via independent mutation
  testing — reviewer deliberately broke each guard (the unit-anchoring
  regex, the `check_schema_add_field_response` non-2xx branch, the
  `"errors"`-in-200-body branch), confirmed the corresponding test
  caught the break, then reverted. No logic defects found. Bounced on
  one structural item: `bench/` is a standalone Cargo crate outside the
  root workspace, so none of `.github/workflows/ci.yml`'s existing
  steps (which all target the root crate) ever touched it — meaning the
  entire new test seam was inert in CI, passing locally but never
  actually gating anything.
- **Round 2** (commit `4073456`): added three CI steps mirroring the
  root crate's existing gates, each scoped with
  `--manifest-path bench/Cargo.toml`:
  - `cargo fmt --check --manifest-path bench/Cargo.toml`
  - `cargo clippy --manifest-path bench/Cargo.toml --all-targets -- -D warnings`
  - `cargo test --manifest-path bench/Cargo.toml`

  Round 2 review: **APPROVED, ready to merge.**

## Test evidence

- `bench/` crate: `cargo test` — 26 passed (11 suites), 0 failed.
- `bench/` crate: `cargo fmt --check` — clean.
- `bench/` crate: `cargo clippy --all-targets -- -D warnings` — clean,
  zero warnings.
- Root workspace: `cargo test` — 485 passed (22 suites), 0 failed
  (confirms the `bench/` change didn't disturb the root crate).

## Follow-ups (not actioned here, deferred by the approved review)

- `.github/workflows/ci.yml`'s `Swatinem/rust-cache@v2` step has no
  `workspaces:` input, so `bench/target` is never cached — the bench
  crate's dependencies recompile from scratch on every CI run. Suggested
  fix for a follow-up: add
  `with: workspaces: ". -> target\nbench -> bench/target"`.
- The new bench CI steps are appended after the root crate's
  `cargo test` (the slowest step), so a fast bench-only fmt/clippy
  failure surfaces late in the CI run. Optional reordering (group both
  crates' `fmt --check` steps first) for a future pass.
- (From round 1, verified-clean-but-worth-recording) `bench/run.sh`'s
  `docker stats` awk loop has no `END { if (!NR) ... }` guard for
  zero-output; a Docker hiccup that produces no `docker stats` line
  fails downstream in `render_report.rs` with a `bad number` panic
  rather than failing at the source — not silently wrong, just fails
  late and away from the actual cause.
- Only `$WORK` is on `run.sh`'s `EXIT` trap, not `$SOLR_CONTAINER` — a
  container leak (and host port 18983 held) is slightly more reachable
  now that the schema-check path can abort loudly mid-run. Consider
  folding `docker rm -f "$SOLR_CONTAINER"` into the trap in a follow-up.

This branch's review used its full two-round allowance (round 1 bounced
a real structural gap; round 2 fixed it and approved) — per the
subagent-pipeline rule, this record notes explicitly that the work
capped out at 2 rounds and could still use further review passes if
more issues surface.

## Pointers

- Production code: `bench/run.sh` (`solr_mem_mb`,
  `check_schema_add_field_response`, cold-start doc comment),
  `.github/workflows/ci.yml` (new bench CI steps)
- Tests: `bench/tests/run_sh_mem_units.rs`,
  `bench/tests/run_sh_schema_check.rs`,
  `bench/tests/run_sh_cold_start_doc.rs`,
  `bench/tests/support/mod.rs`, `bench/tests/results_table.rs`
- Commits: `4073456` (fix + round-2 CI-wiring addition), on top of merge
  base `39975215924472875efb6ed3b0f9bd209187ac68`
</content>
