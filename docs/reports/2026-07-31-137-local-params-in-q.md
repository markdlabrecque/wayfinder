# Issue #137 — local-params support in `q` (`{!edismax qf='...'}`)

**Branch:** `137-local-params-q` (worktree `/Users/mark/Projects/wayfinder-wt/137`)
**Status:** staged (`git diff --cached`), not committed, no PR opened — orchestrator owns both.

## What was built

`search_api_solr` sends `q=({!edismax qf='...'}+"quick" +"rocket")`, and Wayfinder had zero
local-params support (`grep -rn '{!' src/` returned 0 matches before this change), so it 400'd
on the raw syntax. This lands the shared local-params parser: the prep PR that unblocks #138 and
#140.

Diff, staged, 7 files, 1631 insertions / 13 deletions:

- **`src/local_params.rs`** (new, 634 lines) — the parser itself: `parse_block`,
  `bound_token_len`, `extract_nested_queries`. Recognises `{!edismax ...}` local-params blocks,
  computes how much of the trailing text binds to the nested parser as an inline nested query,
  and extracts nested queries for downstream building.
- **`src/core_index.rs`** (+181/-?) — nested-query building and `NestedQueries` threading through
  query construction. Also fixes `build_field_disjunction`: an unquoted multi-token clause now
  builds a boolean OR instead of a `PhraseQuery`, taking the quoted/unquoted distinction from the
  grammar's own `Delimiter` rather than always assuming phrase.
- **`src/lib.rs`** — one `mod local_params;` line.
- **`docs/PRD.md`** (+43/-1) — two entries: §5 divergence 6 (any local-params type other than
  `edismax` in `q` is a hard 400, where Solr parses it — documented rather than fixture-backed,
  since no captured trace exercises `{!lucene}`/`{!term}`/`{!func}`) and a §2 entry recording the
  bug-compatible decision below, including that issue #137's own title is wrong-premised.
- **`docs/solr-ref-findings.md`** (+68) — findings 90 (the per-trace `numFound` table proving the
  binding model), 91 (bound-run terminators: whitespace at any paren depth, or an unbalanced `)`),
  and 92 (`autoGeneratePhraseQueries` defaults off; documentation-derived, not fixture-derived,
  and explicitly flagged as resting on an XML *comment*, not a config setting — one grep hit only).
- **`tests/local_params.rs`** (new, 706 lines) — the new suite, including the two
  `numFound == 0` bug-compatibility guards and the #147 expiry guards (phrase-vs-OR and
  debugQuery binding-rule confirmation).
- **`tests/search_api_coverage.rs`** (+10/-3) — coverage fraction moves.

**Coverage: 42/75 → 45/75.** The three `select.q.local-params-edismax.{and,or,single-term}`
semantics flip to covered; denominator unchanged.

## The decision that shaped it — bug-compatible, not "make it useful"

The user decided to match the fixtures **including the low-recall outcome**, not to make Shape-B
search "just work." The captured `/select` defaults are `defType=lucene`/`df=id`
(`solr-ref/search-api/configset/solrconfig_extra.xml:110-118`), so `{!edismax qf='...'}` is an
**inline nested query**, not a position-0 local-params block re-selecting the parser for the
whole `q`. It binds only the next run of characters after `}`; the remainder is parsed by the
outer lucene parser against `df=id` and matches nothing.

Traces 00004/00008 (`+"quick" +"fox"`) return `numFound: 0` **even though `entity:node/1` contains
both terms** — decisive evidence for the model, per finding 90's per-trace table. Wayfinder
reproduces that low-recall outcome deliberately; the two `numFound == 0` assertions in
`tests/local_params.rs` are the guard against a later well-meaning rewrite to high recall.

**Issue #137's own title ("so `{!edismax qf=...}` keyword search works") is wrong-premised.**
Search does not start working for a Shape-B client — it starts failing the way real Solr fails.
The fix belongs upstream in `search_api_solr`. This is recorded in both PRD entries (§5 divergence
6, §2), per the project's standing rule (CLAUDE.md) that a wrong ticket premise gets flagged, not
silently built to.

## Pipeline history

- **Stage 1 (test-writer)** verified the per-trace `numFound` table from the fixtures before
  writing anything, found no contradiction to the "bug-compatible" reading, and correctly refused
  to unilaterally decide the bug-compatible-vs-useful-recall question — escalated it instead. It
  also corrected its own early assumption: pre-#137 Wayfinder did not silently mis-match on Shape
  B, it 400'd with a `SyntaxError`.
- **Stage 2 (implementor) corrected stage 1's model.** Stage 1's working theory was "bound to the
  next whitespace-delimited token"; traces 00006/00021 have *no whitespace* after the bound run —
  it's terminated by a `)` that closes a paren opened before the block. Implemented terminators:
  whitespace at any paren depth, or `)` at run-local depth 0.
- **Review round 1 found a real bug**: the `__wf_nested_query_N__` sentinel used internally to
  hold extracted nested queries was user-reachable. `q=({!edismax qf='…'}+"quick"
  +__wf_nested_query_0__)` returned `200`/`numFound=2`, where real Solr parses the literal as a
  mandatory term against `df=id` and returns 0 — i.e. user-controlled input was changing which
  query ran. Round 2 fixed it by re-keying (`unique_prefix` extends the base while
  `q.contains(&prefix)`), with a collision-freedom argument: every user-derived run in the output
  is a substring of `q`, while the prefix provably is not. Round-2 review attacked the fix with 12
  adversarial input shapes and it held.
- **Review round 1 also dismantled the evidence for a scope excursion.** `build_field_disjunction`
  was changed to build a boolean OR (not a `PhraseQuery`) for an unquoted multi-token clause,
  originally justified by citing `solr-ref/search-api/configset/schema.xml:63`. That line is
  **inside an XML comment** documenting the `version` attribute's history, not a setting — it
  governs only because `schema.xml:52` declares `version="1.6"` and the attribute is never
  explicitly set. **No fixture distinguishes phrase from OR** — all 21 `defType=edismax` manifest
  rows are quoted phrases or `+`-as-space single tokens — and the only thing asserting the new
  behaviour is a coverage probe whose expected `Some(2)` was authored speculatively in `bb44cc4`
  (#105), for an entry that could not then pass. The change buys exactly one coverage entry and is
  observable on plain `/select?defType=edismax` (`q=state-of-the-art` → 2 now, 1 before).
- **The user accepted the OR-semantics change on documented terms**: keep it, make the comment
  honest about being documentation-derived rather than fixture-derived, and open **issue #147** to
  own `capture.sh` and settle it with two real captures. #147 also answers #137's open question 2
  (the binding rule was never confirmed with `debugQuery=true`).
- **Round 2 review approved.** It independently verified the "not a regression" claim in PRD
  divergence 6 against `origin/main` itself: neither prologue rewrite touches the raw `{!` strings,
  so they already reached Tantivy's grammar and 400'd there before #137 — only the error message
  improved.
- **A final polish pass** applied seven review follow-ups. The load-bearing one: the #147
  debugQuery expiry guard originally scanned only the top-level manifests, but the Shape-B evidence
  lives in `solr-ref/search-api/manifest.tsv` — verified to stay green when a `debugQuery` row was
  added there, meaning #147 could land its capture without the skip ever expiring. Fixed to cover
  all three manifests, and confirmed it now reddens under that test. Two smaller fixes in the same
  pass: the mid-token `%2B` guard over-fired on `q=%28%2Bquick+%2Bfox%29` with a false message
  (fixed by percent-decoding before scanning), and finding 92's original "grep: zero hits" claim
  was literally false — there is exactly one hit, the XML comment being cited — corrected in both
  `src/core_index.rs` and finding 92 itself.

## Test evidence (re-run for this report)

- `cargo test` — **638 passed, 33 suites**, 0 failed.
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean, no issues found.
- `solr-ref/` confirmed untouched (`git status --short solr-ref/` empty); `capture.sh` never run
  for this branch.

## Review outcome

Two rounds, both with substantive findings (a real user-reachable sentinel-collision bug in round
1, an evidence-thin scope excursion also caught in round 1), both resolved and re-verified in round
2, which approved. Two rounds is this pipeline's default cap — per CLAUDE.md, that means this is
the point at which the work is handed off with escalation available, not a claim that no further
review would find anything. The reviewer explicitly re-derived the "not a regression" claim from
`origin/main` rather than trusting the diff's assertion, and re-attacked the sentinel fix with 12
adversarial shapes before accepting it.

## Follow-ups and open items deferred

- **Issue #147** (new, opened during this work) owns two real Solr captures: (1) phrase-vs-OR for
  an unquoted multi-token clause under `defType=edismax` (currently documentation-derived from an
  XML comment, per finding 92), and (2) confirming the inline-nested-query binding rule with
  `debugQuery=true` (issue #137's open question 2). Two expiry guards in `tests/local_params.rs`
  are wired to fail once those fixtures land, across all three manifests (`solr-ref/manifest.tsv`,
  `solr-ref/manifest-errors.tsv`, and `solr-ref/search-api/manifest.tsv` — the last one is where
  the Shape-B evidence actually lives, and was the guard's original blind spot until the polish
  pass fixed it).
- **The error message no longer names a request param.** `CoreIndex::parse_query` serves `q`,
  `fq`, `bq`, `facet.query`, `/mlt`'s `q`, and delete-by-query. Threading the param name through
  each caller would touch `src/lib.rs` at 2 sites and `src/facet.rs` at 1 — and `src/lib.rs` is
  this repo's documented hottest conflict file. Deliberately deferred; worth its own follow-up
  issue naming that ownership so it doesn't collide with concurrent work.
- **#138 is unblocked**: `{!key=X}` appears only in `facet.field` (traces 00018/00019), which does
  not route through `parse_query`, and `parse_block` already parses type-less blocks.
- **#140 still needs its own decision**: the `f.<field>.*` wildcard-allowlist question is untouched
  by this PR.
- **Note for #135**: `SELECT_PARAMS` in `src/lib.rs` is untouched by this change.

## Bottom line

Bug-compatible local-params support for the one Shape-B pattern the captured client actually
sends, landed with two review rounds (one substantive user-reachable bug found and fixed, one
evidence-thin scope excursion found and resolved by descoping to a documented, expiry-guarded
follow-up rather than either reverting or shipping unverified). All local gates green;
`solr-ref/` untouched. Coverage 42/75 → 45/75. Follow-ups above (#147's two captures, the
param-name-in-error-message gap, #138/#140 status) are the reviewer's and the user's explicit
deferrals, not omissions.
