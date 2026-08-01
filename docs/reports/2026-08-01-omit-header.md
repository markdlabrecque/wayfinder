# Issue #143 — honour `omitHeader`, accept `TZ`

- Branch: `143-omit-header`
- Worktree: `/Users/mark/Projects/wayfinder-143`
- Commits: `2324216` (feat), `9418915` (test fix, applied by the orchestrator), `450c02e`
  (round-2 review corrections)
- Findings: appended as #93 in `docs/solr-ref-findings.md`
- Follow-up filed: #179 (open)

## The gap

`search_api_solr` sends `omitHeader=true&TZ=UTC` on nearly every request. Neither param was in
the `SELECT_PARAMS`/`MLT_PARAMS`/`UPDATE_PARAMS` allowlists, so `strict_params = true` would 400
the exact request shape the module sends, and even where it did not 400, `omitHeader=true` was
silently ignored — Wayfinder always answered with a `responseHeader` the client had explicitly
asked to suppress. 20 of the 28 captured `search_api_solr` traces send `omitHeader=true`, and
every one of those has a response with no `responseHeader` key at all (see finding #93).

## The change

- `Params::omit_header()` (`src/params.rs`) is a single shared gate: `self.get("omitHeader") ==
  Some("true")`.
- `omitHeader` and `TZ` added to `SELECT_PARAMS` and `MLT_PARAMS`; `omitHeader` added to
  `UPDATE_PARAMS` (no `TZ` there — the module never sends one on `/update`).
- Suppression wired into `select`, `mlt`, and `update_success`, which now takes `&Params`.
- `/terms` (issue #155) had already implemented this exact check inline
  (`params.get("omitHeader") != Some("true")`). The implementor **extracted** that into
  `Params::omit_header()` and repointed `/terms` at the shared helper rather than writing a
  second mechanism. The reviewer verified the extraction is exactly equivalent for every input
  class checked (absent, empty, `"TRUE"`, `"1"`, repeated key).

## Two things deliberately not done, both recorded as ceilings

1. **Error responses are untouched.** No fixture settles whether `omitHeader=true` suppresses
   the header on an error envelope — all 28 `search_api_solr` traces are 200s, and no
   `manifest.tsv`/`manifest-errors.tsv` row uses `omitHeader` at all. Guessing would either
   regress the landed error-envelope work or bake in an unverified divergence. Marked with a
   `ponytail:` comment on `Params::omit_header` and filed as **#179**. The reviewer confirmed no
   leak on this branch: `omit_header()` has four call sites (`select`, `mlt`, `update_success`
   x2 via the two call sites in `update`, `terms`), all on success envelopes; `src/error.rs`
   builds its own header independently with no reference to the gate.
2. **`/update`'s `omitHeader=true` suppression is generalised, not fixtured** — the module's own
   trace (`00001`) only ever sends `omitHeader=false`. `update_success`'s doc comment says so
   explicitly and names what would settle it (a capture of a real `solr:9`
   `/update?commit=true&omitHeader=true`).

## Review outcome — bounced round 1 on two must-fix items

1. **A green branch and a green `main` did not imply a green merge.** `git merge-tree`
   auto-merged this branch onto `main` with no conflict, yet the merged suite failed:
   `d1147c0` (issue #162, already on `main`) had hard-coded a second pin of the overall coverage
   fraction at `57/75`, and this branch legitimately moves it to `59/75` by flipping
   `request.omitHeader` and `request.timezone.utc` from uncovered to covered. This is exactly
   the case CLAUDE.md's workflow rule 5 exists for, and it was caught only because the reviewer
   built the merge tree and ran the suite *there* rather than trusting a clean auto-merge. This
   is the transferable process lesson from this issue: clean-merge is not evidence of
   green-merge, and checking it costs one extra `git merge-tree` + test run.
2. **The `TZ` accept-and-ignore justification was factually wrong on two of its three claims.**
   The first draft implied Wayfinder has no date type and no date faceting at all; it has both
   (`ResolvedType::Date`/`add_date_field` in `src/schema.rs`; `parse_date`/`parse_date_gap`/
   `RangeEnd::Date` in `src/facet.rs`). The conclusion (accept-and-ignore is currently safe)
   held, but for different reasons: `facet.range.start`/`.end` must be a literal RFC3339 instant
   with no `NOW` or date math, calendar gaps `+1MONTH`/`+1YEAR` are refused by name, and a
   fixed-length gap walked over absolute instants is timezone-invariant. Rewritten as a
   `ponytail:` on the `SELECT_PARAMS` entry naming the real ceiling and the exact condition that
   ends it — if `NOW`/date-math parsing or MONTH/YEAR gaps land, ignoring `TZ` becomes a silent
   wrong answer, not a no-op.

Plus a five-minute item: `src/params.rs`'s doc comment claimed the strict `== Some("true")`
parsing matched Solr's. It does not — Solr's `StrUtils.parseBool` also accepts `1`, `yes`,
`TRUE`/mixed case. It matches *this codebase's* own boolean-reading convention (the same
`== Some("true")` pattern `commit`, `softCommit`, `facet`, `stats`, `hl`, `mlt.boost`, `terms`
all use), and the divergence from Solr's laxer parser is real but unfixtured. Reworded and
cross-referenced to #179, which now also carries this strict-boolean divergence and the missing
error-envelope guard.

Round 2's implementor was told the two flipping items were `response_fields` and did not take
that on trust: it built `origin/main` in a scratch worktree, diffed the covered-item sets, and
found the two items are actually `request_semantics` items — `response_fields` is unchanged at
13/15 across the whole branch, which *strengthens* rather than undermines the guarded
assertion's stated intent (a coverage bump caused by a genuinely-landed request feature, not by
a probe quietly stopping short of real data). It wrote the corrected claim into the test comment
rather than the plausible-but-wrong one it had first drafted (`450c02e`).

**Process note on the escalation:** the round-1 implementor hit a red stage-1 test —
`select_omit_header_true_leaves_response_block_unaffected` — and escalated instead of editing
it, which was the right call and right on the merits. The test compared the raw fixture
verbatim and tripped on `_version_`/`_root_`, internal fields Wayfinder deliberately omits and
which `common::normalize_envelope` exists to drop. The orchestrator applied
`normalize_envelope` to both sides of that one comparison (`9418915`) and flagged the edit to
the reviewer as unreviewed test code; the reviewer confirmed the test's intent survives the
edit and that the raw, unnormalised version would have failed identically even with
`omitHeader` absent — i.e. the fixture mismatch was pre-existing and orthogonal to this feature,
not a symptom of a bug in the new suppression logic.

Per CLAUDE.md's default two-round cap for the reviewer stage: this review used both rounds — the
first bounced two must-fix items plus the five-minute item above, the second closed them. The
cap was reached, not exhausted with anything outstanding, so there is no standing need flagged
for a third pass on this specific diff — but per the pipeline's own rule, two rounds is the
default cap, not evidence the work has had all the review it could use.

## Evidence

Re-run for this report, on the current HEAD (`450c02e`), not copied from an earlier commit's run:

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test` — 752 passed, 40 suites, 0 failed.
- Coverage: **59/75**, endpoints **9/9** (up from 57/75; `request.omitHeader` and
  `request.timezone.utc` both flip from uncovered to covered — confirmed both probes assert the
  actual semantic, not a bare 200).
- Mutation test (corrected counts — an earlier draft of this report said 4+1, which was wrong):
  making `omitHeader=true` a no-op kills 3 tests in `tests/omit_header.rs` plus 1 in
  `tests/terms.rs`; making `omitHeader=false` *also* suppress kills a different 3 in
  `tests/omit_header.rs` plus 1 in `tests/terms.rs`.
  `select_omit_header_true_leaves_response_block_unaffected` survives both mutations, because it
  compares only the normalised `response` block, not the header's presence.

Branch was already rebased onto the current `main` tip at write time (`origin/main` HEAD
`4ad05e1abe3ed9049c2c93e4bf5ee6429b79023a` is this branch's immediate parent).

## Nothing to delete

No `EXPECTED_DIVERGENCES` entry and no `manifest.tsv`/`manifest-errors.tsv` row uses
`omitHeader`. The ticket's acceptance criterion to delete a stale divergence entry does not
apply here: the differential harness was already green before this change only because no
manifest row exercises `omitHeader`, not because of any masking this branch introduces or
removes.

## Follow-ups deferred (not actioned on this branch)

- **#179** — open. Carries three unresolved items from this issue: (1) whether `omitHeader=true`
  suppresses `responseHeader` on error responses (unfixtured — no manifest row and no captured
  trace exercises this), (2) the missing error-envelope guard this implies if the answer turns
  out to be "yes", and (3) the strict-boolean (`== Some("true")` vs Solr's `StrUtils.parseBool`)
  divergence in `Params::omit_header` and every sibling boolean read in `src/lib.rs`.
- Finding **#93** appended to `docs/solr-ref-findings.md`, following that file's numbering
  convention: `omitHeader=true` yields no `responseHeader` key at all across the 20 traces that
  send it — not a present-and-empty one — and the corpus is silent on error envelopes, which is
  exactly the #179 gap.

## Bottom line

`search_api_solr`'s two near-universal envelope params are now handled: `omitHeader`/`TZ` no
longer 400 under `strict_params = true`, and `omitHeader=true` suppresses `responseHeader` on
`/select`, `/mlt`, `/update`, and (via extraction rather than duplication) `/terms`. All local
gates are green (752/40, fmt and clippy clean), coverage moved 57/75 -> 59/75 with both flipped
items confirmed to assert real semantics, and the fix is mutation-confirmed in both directions.
Review took both rounds of the default two-round cap: round 1 caught a real would-be-red merge
that a clean `git merge-tree` auto-merge had hidden (a second hard-coded coverage pin already on
`main`) and a doc comment overstating what `TZ` being inert actually rests on; round 2 closed
both correctly, including the round-2 implementor independently re-deriving a coverage claim
rather than accepting the brief's framing of it. Two ceilings are on record rather than guessed
past: error-envelope behaviour under `omitHeader` (#179) and `/update`'s specific case being
generalised, not fixtured.
