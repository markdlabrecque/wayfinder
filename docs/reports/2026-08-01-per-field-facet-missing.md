# Issue #140 — per-field `f.<field>.facet.missing` override

- Branch: `140-per-field-facet-missing`
- Worktree: `/Users/mark/Projects/wayfinder-140`
- Commits: `d0c6bb9` (feat), `62e9f44` (round-2 review follow-ups), rebased onto `0f0b068`
  (#154/#190)

## What was built

`Params::per_field(field, param)` (`src/params.rs`) reads `f.<field>.<param>` —
`per_field("category", "facet.missing")` reads `f.category.facet.missing`.

`split_per_field_key(key, honoured)` recognises the shape and hands back the field and the
base param it overrides. It anchors on the **suffix**, not the first `.`, because a field name
can itself contain dots since #164/#180 landed dotted dynamic field names: `split_once('.')`
would truncate `f.ss_field.name.facet.missing` to field `ss_field` and base param
`name.facet.missing`, which matches no honoured entry. Anchoring on the suffix resolves it
correctly to field `ss_field.name`.

`PER_FIELD_PARAMS = &["facet.missing"]` is the one base param this issue honours in the
per-field form. `check_params` (`src/lib.rs`) accepts a key `f.<field>.<p>` only when `<p>`
appears in **both** `PER_FIELD_PARAMS` and the endpoint's own allowlist — so
`f.x.facet.missing` still 400s on `/update`, `/mlt`, and `/terms`, none of which have
`facet.missing` in their allowlist at all.

`facet_fields` (`src/facet.rs`) resolves the per-field override first, falling back to the
global `facet.missing` when no override is present for that field. The override wins
unconditionally — whether the global is unset, `true`, or `false` — per finding 97.

Five fixtures captured against a one-off `solr:9` (port 8992), no `manifest.tsv` rows (see
below): `facet_missing_field_override_alone`, `_mixed_multi_field`,
`_unrelated_field_no_effect`, `_wins_over_global_false`, `_wins_over_global_true`.

## A deliberate scope decision, stated as such

The `f.<field>.<param>` mechanism built here is general — `Params::per_field` and
`split_per_field_key` take any base param name — but `PER_FIELD_PARAMS` lists only
`facet.missing`. Every other `f.<field>.facet.*` Solr accepts (`.limit`, `.mincount`, `.sort`,
`.prefix`) is unimplemented and must keep 400ing under `strict_params`, pinned by
`strict_params_still_rejects_an_unrelated_f_dot_param`
(`tests/facet_field_missing_override.rs`). The `PER_FIELD_PARAMS` comment names the reason
explicitly: allowlisting a per-field param whose value is then ignored converts a loud 400 into
a silently wrong answer — a client asking for `f.category.facet.limit=5` would get the global
limit with no indication the override was dropped. That is the failure mode this batch hit in
#139/#181, so the upgrade path is stated at the call site: implement the override where the
global is read in `src/facet.rs`, then add the base param name to `PER_FIELD_PARAMS` in the
same change. Adding the name alone is the bug.

## Review outcome — two rounds plus an escalation

**Round 1** confirmed the behaviour correct by running a live server and probing the boundary
directly: `f..facet.missing`, `f.facet.missing`, `f.ss_type.facet.missing.extra`, casing
variants, and all four unimplemented siblings (`.limit`, `.mincount`, `.sort`, `.prefix`) all
400; dotted dynamic fields work end-to-end; precedence matches all five fixtures. But it bounced
on **four surviving mutants**, each of which left the full suite green — including the
endpoint-scope check collapsing to `.is_some()` (which would have made
`/update?f.x.facet.missing=true` return 200) and the suffix split reverting to
`split_once('.')` (which breaks dotted fields). `split_per_field_key` had zero direct tests.

**Round 2** added them: a new `#[cfg(test)]` module in `src/params.rs` (seven cases covering
the plain split, the suffix-anchoring on dotted fields, an empty field name, no field segment
at all, trailing text after the base param, a base param outside the honoured list, and no `f.`
prefix), three stub-router tests in `src/coverage.rs`, and strict-mode 400 integration tests for
`/update`, `/mlt`, and `/terms`. All four mutants died. Production code was unchanged in this
round — 151 lines added, all inside `#[cfg(test)]` modules.

**Round 2 review** then found a **fifth** mutant, and it was the worst of the five: swapping the
coverage probe's query string from `f.category.facet.missing=true` to the global
`facet.missing=true` left all 788 other tests green — and so did doing that *and* reverting the
feature itself in `src/facet.rs`. The coverage artifact, which this repo's CLAUDE.md treats as
compatibility evidence, would have certified `select.facet.per-field-missing` as covered on an
implementation that ignores the per-field param entirely. Root cause was the same class as
#167: `select_only_probe`'s stub router matched on path only, so the stub pinned the probe's
predicate and nothing about its request — the probe could ask for the global param, or nothing
at all, and every stub assertion would still pass.

Fixed by making the stubs query-sensitive via `RawQuery`, with a whole-segment matcher
(`query_carries`) so `facet.missing=true` cannot substring-match
`f.category.facet.missing=true`, and three server models: one that honours only the per-field
override, one that honours only the global (and must therefore read as *uncovered* by the
probe), and one that honours neither. Verified: the correct probe plus the reverted feature now
drops the coverage artifact by one and reds `search_api_coverage` 3/8; before the fix that same
combination stayed green. (The mutation run was made before the rebase onto #154, so the figures
observed were 62/75 against a 63/75 baseline; the equivalent on the shipped tree is 63/75
against 64/75. The mutants were not re-run after that rebase — `src/coverage.rs` is
byte-identical across it, and `src/params.rs`/`src/facet.rs` differ only in a comment's finding
number.)

One correction the implementor made to the reviewer's expectation: under the mutated probe,
`cargo test --test search_api_coverage` still passes 8/8 and cannot be made to fail, because the
real app answers the global param correctly — the failure necessarily lands in the
`#[cfg(test)]` stubs in `src/coverage.rs`, which is the right instrument, matching the #162/#167
precedent.

Per CLAUDE.md's default two-round cap for the reviewer stage: this review used both rounds and
still surfaced a fifth mutant on the second pass, so the work would benefit from further review
passes beyond the cap rather than being treated as exhaustively checked.

## An expired rationale corrected

`solr-ref/capture.sh` and `docs/solr-ref-findings.md` both previously justified omitting
`manifest.tsv` rows for this capture with "Wayfinder does not implement `f.<field>.facet.*`
yet" — which stopped holding on this very branch once the feature landed. Both now give the
real, narrower reason: a manifest row would put facet bucket *ordering* through the
differential harness, which is a separate change from the precedence semantics these five
fixtures settle. The compatibility claim stays pinned by `assert_matches_fixture` asserting all
five bodies whole in `tests/facet_field_missing_override.rs`.

## Evidence

Re-run after the rebase onto `0f0b068` (#154/#190):

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test` — 799 passed, 41 suites, 0 failed.
- `cargo test --test differential` — 27 passed.
- `cargo run -- coverage --format json` — 64/75.
- Mutation testing: five deliberate mutants introduced across two rounds (endpoint-scope check,
  suffix split, and three coverage-probe/stub variants); all five died once the corresponding
  tests landed.

## Outstanding at time of writing

- ~~**Finding renumbering / coverage fraction contended.**~~ **Resolved.** #154 (PR #190)
  merged first and took finding 96, so this branch rebased onto it and renumbered to **97**.
  The fraction was re-derived from `cargo run -- coverage --format json` on the rebased tree as
  **64/75**: both branches independently took 62/75 to 63/75, and the merged state is 64/75.
  This is the third renumbering for this block — written against 94, moved to 96 when #139 took
  94/95, then to 97 when #154 took 96 — which is why the findings block now carries an explicit
  `Claiming finding 97` preamble line.
- **#187** — filed during this review: Wayfinder's boolean parsing is stricter than Solr's
  `StrUtils.parseBool`, discovered while probing casing variants of `facet.missing` values.
