# Issue #154 — accept repeated `add` command keys in one `/update` body

- Branch: `154-update-command-batch`
- Worktree: `/Users/mark/Projects/wayfinder-154`
- Head: `aee10de`
- Findings: appended as #96 in `docs/solr-ref-findings.md`

## The gap, narrower than the ticket implied

The ticket framed this as a missing parse path — as if `/update` could not handle Solr's
command-object format at all. It already could: issue #9 built `add`/`delete`/`commit` command
handling. The actual bug was one layer down. `parse_update_commands` parsed the body through
`serde_json::Value`, and `Value::Object` is a map — a duplicate top-level JSON key collapses to
its last occurrence during that parse, before any Wayfinder logic runs. The module's real update
body (`solr-ref/search-api/trace/00001.json`) is exactly this shape: six repeated `add` keys in
one object. Wayfinder's old parse silently kept one of the six and returned 200. A silent wrong
answer, not an error — no 400, no exception, just one document indexed where the wire body asked
for six.

## What was built

- A new `UpdateBody` type in `src/lib.rs` with a hand-written `serde::Deserialize` that drives
  the top level off `MapAccess` instead of `Value`, so every occurrence of a repeated key is
  kept, in body order, as `Vec<(String, Value)>`. Each command's own *value* is still parsed as
  an ordinary `Value` — only the top level needed the fix.
- `parse_update_commands` now returns an ordered `Vec<UpdateCommand>` (`Add`, `DeleteIds`,
  `DeleteQuery`, `Commit`) instead of a single struct with pre-summed fields.
- The `update` handler executes that list in body order, coalescing only *consecutive* `Add`s
  into one `add_documents` batch and flushing/committing in place whenever a `delete` or `commit`
  command interrupts a run of adds.
- The whole body is still fully validated before anything executes — a bad command later in the
  body still voids adds that came earlier in it (see below).
- `ponytail:` on `UpdateBody` names the ceiling: only the top level is duplicate-tolerant. A
  repeated key inside a command's own value (`{"add":{"doc":{...},"doc":{...}}}`) still collapses
  to the last occurrence, because that inner value is parsed as ordinary `Value`. Unobserved in
  any capture or trace; Solr's own `JsonLoader` reads a single `doc` per `add` anyway.

## An adjacent divergence fixed in the same change

Independent of the duplicate-key bug, Wayfinder previously executed all adds, then all deletes,
regardless of the body's actual order. A body that deletes an id and then re-adds it in the same
request lost the doc — the delete ran after the add unconditionally. Real Solr keeps it
(`update_repeated_add_delete_before.json`: `r4` is deleted then re-added, and survives with the
new title). Order-preserving execution (this change) fixes this as a side effect. This is a scope
expansion beyond the ticket's stated acceptance criteria, made because the new parse already
carries body order and the fix is nearly free once that's in hand — not attempted separately.

## Fixtures

Twelve new fixtures, captured against a one-off `solr:9` (port 8992, `update9` core, same schema
and `u1..u5` seed convention as `capture.sh`'s existing `update9` block). Rows went into
`solr-ref/manifest-errors.tsv`, never `manifest.tsv` — these are POSTs and their follow-up
same-core selects, not core-relative GETs the differential harness can replay verbatim. The
`capture.sh` block is appended at EOF and is the only fully commented-out block in the file, so
these twelve fixtures are the only ones the script cannot regenerate on a re-run; that was a
deliberate choice given the corpus's stateful nature (see below), not an oversight.

The first capture pass had to be redone. The initial `update_select_after_*` fixtures used
whole-corpus `q=*:*` selects, which went red in the differential harness — it replays
`manifest-errors.tsv` sequentially against one accumulated core, so an earlier row's leftover
documents polluted a later row's whole-corpus count. Rescoped every follow-up select to just the
ids each POST body actually touches, which fixed it and is more precise regardless.

Findings recorded as #96 in `docs/solr-ref-findings.md`: not last-wins, execution in body order
(not grouped by kind), two adds of the same id leave the last (ordinary `overwrite=true`
replace-by-uniqueKey), and a bad command aborts everything before it in the same body.

## Review outcome — one round, one must-fix

The reviewer verified rather than restated:

- Ran the hand-written `Deserialize` against `origin/main`'s across 21 adversarial bodies
  (truncated JSON, deeply nested objects triggering serde's recursion limit, wrong top-level
  type, empty body, non-UTF8) and confirmed byte-identical error messages, including serde's own
  recursion-limit column numbers — the new parser is not silently more lenient or differently
  worded on the failure path.
- Confirmed the `ponytail:` on the nested-duplicate-key collapse is accurate by testing
  `{"add":{"doc":{...},"doc":{...}}}` directly rather than trusting the comment.
- Confirmed the delete-then-re-add scope expansion is real-Solr-backed
  (`update_repeated_add_delete_before.json`), not derived from Wayfinder's own prior output.
- Fixture hygiene: all twelve are pure additions, zero modifications to existing fixtures or
  `manifest.tsv`.
- The `EXPECTED_DIVERGENCES` guard flip for this issue is strictly stronger, not just moved.
- Error atomicity holds even when a `commit` command precedes the invalid command later in the
  same body — a preceding commit does not exempt earlier adds from being voided by a later
  parse failure.

The must-fix: a **fifth surviving mutant**. Deferring the in-place `commit` (on a body `commit`
key) to end-of-body instead of executing it there and then passed all 774 tests and the
differential harness, despite the code comment explicitly asserting commits happen in place.
Probing both the real binary and the mutant with `{"add":c1,"commit":{},"add":c2}` showed the
divergence directly: real binary leaves `c2` at `numFound` 0 (uncommitted), mutant at 1
(committed). Closed by `an_add_after_a_body_commit_key_stays_uncommitted` in
`tests/update_pipeline.rs`, with its own `ponytail:` naming the ceiling honestly — this
expectation is inferred from command-stream semantics, not fixture-derived (no capture puts an
add after a body `commit`), and a future capture that disagrees wins over this test.

Per CLAUDE.md's default two-round cap for the reviewer stage: this review closed in one round.
The cap was not reached, so there is no standing escalation — but per the pipeline's own rule,
one clean round is not evidence the work has had all the review it could use.

## Evidence

Re-run for this report, on the current head (`aee10de`):

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test` — 775 passed, 40 suites, 0 failed.
- `cargo test --test differential` — 27 passed, 0 failed.
- `cargo run -- coverage --format json` — **63/75** overall (up from prior baseline), endpoints
  **9/9**, request_semantics **41/51**, response_fields **13/15**.
  `update.json-command-add-batch` flips from uncovered to covered, confirmed by the coverage
  output's own trace/evidence fields (`00001.json`, runtime-probe against the strict routed
  handler plus rendered JSON) rather than a bare 200.

## Outstanding at time of writing

Finding **96** in `docs/solr-ref-findings.md` is claimed by three unmerged branches (#140, #141,
#154). Whichever lands later will need to renumber its finding on rebase — this is a known
merge-order cost of the numbered-findings convention, not a defect in this branch.

## Bottom line

The root cause was narrower than the ticket implied: command-format parsing already existed
(issue #9); the actual defect was `serde_json::Value`'s duplicate-key collapse silently dropping
five of six adds in the module's real update body. Fixed with a hand-written top-level
`Deserialize` that preserves body order and every key occurrence, plus order-preserving execution
in the handler — the latter also fixing an adjacent, previously-unfixtured delete-then-re-add
divergence from real Solr. Twelve new fixtures back both behaviours; all local gates are green
(775/40 tests, differential 27/27, fmt and clippy clean), and coverage moved to 63/75 with the
targeted item confirmed to assert real semantics rather than a bare 200. Review closed in one
round after finding a real must-fix (a fifth surviving mutant on commit timing) that the full
test suite alone had not caught; the fix is pinned by a new test that is explicit about resting
on inferred semantics rather than a capture.
