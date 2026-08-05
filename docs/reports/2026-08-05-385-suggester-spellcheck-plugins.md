# #385 — `search_api_wayfinder`: reimplement the Suggester and Spellcheck autocomplete plugins

**Date:** 2026-08-05. **Branch:** `markdlabrecque/issue-385-support-suggester-spellcheck`.
**Status:** green, reviewed (two rounds, approved), ready for PR. All work is PHP,
inside `drupal/search_api_wayfinder/`; no Rust source, test, or fixture file is
touched.

This completes the per-plugin plan #351's report laid out: Terms→`/terms`
(shipped, #291), Spellcheck→`/select`, Suggester→`/suggest`'s `suggest.q` read
path (server side shipped by #384, now merged to `main` as `6773c6f`/PR #391).
All three stock `search_api_solr` autocomplete plugins are now reimplemented
against real Wayfinder endpoints.

## What was built

- `QueryBuilder::buildAutocompleteSpellcheck(QueryInterface $query, string $user_input): array`
  — mirrors `Spellcheck.php::setAutocompleteSpellCheckQuery`. Emits
  `spellcheck`/`spellcheck.q`/`spellcheck.dictionary` (one per resolved
  language, via a new shared `spellcheckDictionaryParam` helper so this and
  the existing search-path `buildSpellcheck()` cannot drift), `rows=0`,
  `omitHeader=true`. No `q`, no `fq`, no `spellcheck.count` (see ceilings,
  below).
- `QueryBuilder::buildAutocompleteSuggester(QueryInterface $query, string $user_input, array $contextFilterTags = []): array`
  — mirrors `Suggester.php::setAutocompleteSuggesterQuery`, including the
  upstream dictionary-derivation branch structure (explicit langcode tag;
  `multilingual` + one resolved langcode collapses to it; `multilingual` +
  several emits one dictionary per langcode and rewrites the tag into the
  grouped `(<encoded>lang1 <encoded>lang2)` form; `any` drops the langcode tag
  and stays at `und`), `suggest.cfq` via the new
  `buildSuggesterContextFilterQuery` (ported from `Utility.php:476`:
  `+`-prefixed, `encodeSolrName`d unless already encoded, space-joined, the
  param omitted entirely when there are no tags), `suggest.count` from the
  query limit (default 10), `suggest.highlight=false`, `omitHeader=true`.
- `WayfinderClient::suggest(array $params): array` — new transport, `GET
  {core}/suggest`, `wt=json` forced, same error handling as
  `terms()`/`select()`/`mlt()`.
- `WayfinderBackend::getSpellcheckAutocompleteSuggestions()` and
  `::getSuggesterAutocompleteSuggestions()` — both catch `SearchApiException`
  and return `[]` so a failing autocomplete query never breaks the search
  widget, both dedupe on suggested keys (mirroring upstream's
  `filterDuplicateAutocompleteSuggestions`), and both parse via a shared
  extraction path rather than duplicating the decode.
- `ResponseParser::extractSpellcheckSuggestions()` — the flat
  `spellcheck.suggestions` decode (including the `extendedResults`
  `{word, freq}` shape) moved out of the private `parseSpellcheck()` into a
  method shared with the autocomplete path, so the search path and the
  autocomplete path cannot disagree on the envelope shape — the same sharing
  upstream gets from `SolrSpellcheckBackendTrait`.
- `FieldMapper::encodeSolrName()` made public (was private), plus a new
  `decodeSolrName()` — both needed by the `suggest.cfq` "already encoded?"
  check and by `DocumentBuilder`'s new field, rather than duplicating the
  regex a second time.
- Two new `search_api_autocomplete` suggester plugin classes —
  `src/Plugin/search_api_autocomplete/suggester/Spellcheck.php` and
  `Suggester.php` (plus a shared `BackendTrait.php`) — `#[SearchApiAutocompleteSuggester]`
  ids `search_api_wayfinder_spellcheck` / `search_api_wayfinder_suggester`
  (deliberately not the `search_api_solr_*` ids, to avoid fighting the real
  module if both are installed). `search_api_autocomplete` stays out of
  `.info.yml` dependencies and composer `require` (soft dependency preserved,
  per the existing `composer.json` arrangement) — the classes are only ever
  discovered when that module is installed and scans the directory itself.
- `DocumentBuilder`: new `sm_context_tags` field —
  `[encodeSolrName('search_api/index:' . $index->id()),
  encodeSolrName('drupal/langcode:' . $item->getLanguage())]`, multi-valued,
  mirroring `SearchApiSolrBackend.php:1343-1347` minus the site-hash tag.
- `config/schema/search_api_wayfinder.schema.yml` — one entry per new
  suggester plugin (added in round 1 of review, see below).

## Two scope additions beyond the issue text, and why they were necessary

The issue as scoped (SPEC-385, "Deliverables A–D") covers only the builder,
client, and backend-parse methods. Two things beyond that text were added
because without them nothing would ever invoke the new code:

1. **The two `search_api_autocomplete` suggester plugin classes.** The stock
   Drupal **Server** suggester (`search_api_solr`'s own default) maps
   autocomplete requests only to the Terms path
   (`getAutocompleteSuggestions()`). Nothing in `search_api_autocomplete`
   would ever call `buildAutocompleteSpellcheck`/`buildAutocompleteSuggester`
   or the new backend methods unless a real plugin exists to be selected in
   the module's admin UI and dispatch to them. The plugin classes (deliverable
   D3 in the spec) are what make the feature reachable, not an optional
   wrapper.
2. **`DocumentBuilder` indexing `sm_context_tags`.** `suggest.cfq` filters on
   the document's `sm_context_tags` field (`src/core_index.rs:4859`), and this
   module indexed no such field before this change. Without it, every
   context-filtered Suggester lookup (the `search_api/index` or
   `drupal/langcode` radios on the plugin's config form) would silently
   return zero results — a correctly-built request answered by an empty
   response, indistinguishable from "no suggestions" to the end user. This
   was deliverable E in the spec, but is called out here because it is easy to
   read the spec as "builder/client/parse only" and miss that the feature is
   dead on arrival for any context-filtered site without it.

## Two deliberate ceilings (`ponytail:`-marked in source)

1. **No `spellcheck.count`.** Not in `SELECT_PARAMS`; sending it 400s under
   `strict_params`. Wayfinder's `fn spellcheck` returns exactly one correction
   per token, which equals the Spellcheck plugin's own `count` default of 1
   (`$query->getOption('limit') ?? 1`), so the ceiling costs nothing at the
   default but caps a widget asking for `limit > 1` at one correction per
   token regardless. The comment matches the existing note at
   `QueryBuilder::buildSpellcheck()` (`:168-177`) verbatim in substance so the
   two do not contradict each other.
2. **No `search_api_solr/site_hash` context tag.** This module indexes no
   site hash — `DocumentBuilder.php` already ponytails it out of document ids
   (a pre-existing decision, not new to this issue). So the Suggester plugin's
   config form omits the upstream "restrict to this site only" radio; every
   context filter this module can express is index-id or langcode, never
   site-hash.

## Test evidence

```
$ vendor/bin/phpunit
...
Tests: 453, Assertions: 800, PHPUnit Deprecations: 306.
OK, but there were issues!
```

453 green: 427 pre-existing baseline + 24 from SPEC-385's cases 1–16 (several
cases cover more than one assertion) + 2 pinning defects the reviewer found in
round 1 (see below). Re-run independently twice after the review round and
once more just before writing this report, all identical. The 306 deprecation
notices are pre-existing PHPUnit/PHP version noise, unrelated to this change.

**Red-first is checkable from git**, not just asserted: `bb3bc07` and
`53f22f6` are both test-only commits that precede `70f8c12` (the
implementation commit), and `70f8c12`'s diff touches zero files under
`tests/`. `bb3bc07`'s message states every new test failed on an undefined
method or missing field before implementation.

**Mutation test** (required by CLAUDE.md for code whose value is failing
correctly): reintroduced `spellcheck.count` into
`buildAutocompleteSpellcheck()`. `testBuildAutocompleteSpellcheckOmitsCountQAndFq`
went red with `Failed asserting that an array does not have the key
'spellcheck.count'`. Reverted; confirmed green again.

**Rust side:** no Rust file is touched by this branch (`git diff
origin/main..HEAD --stat -- src/ tests/ solr-ref/` is empty), consistent with
the spec's constraint to leave the Rust tree to #384's sibling worktree.
`cargo fmt --check` passes cleanly on this worktree as a sanity check, but
there is no Rust diff for `cargo clippy`/`cargo test` to gate here — the
relevant Rust work (#384) already landed on `main` as `6773c6f` (PR #391)
before this branch's `1c58ab4` rebase, and this branch's own commits carry no
production or test changes for that gate to re-run against.

## Review outcome

Two rounds, ending in approval. Reporting this honestly, because it is the
most instructive part of this issue.

**Round 1** cleared all six weaknesses it was aimed at, including verifying
that `body["spellcheck"]` in the `/select` handler is unconditional on
`spellcheck_requested` — so a `spellcheck.q`-only request with `rows=0` and no
`q` does still return the `spellcheck` block (premise 2 in the spec, confirmed
rather than assumed). It raised two must-fix items: the missing
`config/schema/search_api_wayfinder.schema.yml` entries (real, fixed), and a
`suggest.dictionary` ceiling.

**Half of that second must-fix was fabricated.** The reviewer reported a
server-side dictionary allowlist (`SUGGEST_CONFIGURED_DICTIONARIES`) and a 400
`No suggester named <x> was configured`, cited as shipped behaviour at
`src/schema.rs:1074`, and concluded the Suggester plugin would be silently
dead on `de`/`fr`/`pt-br` sites. The implementor could not find either symbol
anywhere in #384's shipped code and refused to write a comment citing code it
could not locate, escalating instead of guessing. Verified: zero matches for
`SUGGEST_CONFIGURED_DICTIONARIES` or that error string anywhere in the tree;
the only occurrence of that idea at all is a line in #384's own report, filed
explicitly as "hypothesis needing one cheap capture" — not shipped behaviour.
The real code is `params.get("suggest.dictionary").unwrap_or("und")`, with no
allowlist, and `dictionary_tokenizer` falls an unshipped dictionary back to
the `und` analyzer chain while registering one chain per `LANGUAGES` code —
so `de`/`fr` get real per-language stemmers and `pt-br` (not in `LANGUAGES`)
degrades to `und` rather than 400ing.

**Root cause**, self-reported by the reviewer in round 2: it had grepped a
*different in-flight worktree* (`issue-390-two-unpinned-behaviours`), whose
uncommitted tree was mid-implementation of exactly that allowlist hypothesis.
`src/lib.rs:4338` happening to resolve identically in both trees made the
fabricated `schema.rs` citation look corroborated when it was not present in
the tree actually under review.

The surviving, real half of that item is documented: `suggest.dictionary` is
first-wins server-side (finding 193, recorded on `main` in commit `2c444aa`
before this branch existed), so the `multilingual`-with-several branch this
issue's `buildAutocompleteSuggester` implements only ever gets the *first*
langcode served back, regardless of how many dictionaries the client asks
for. That is a real, load-bearing fact about the feature this issue ships,
correctly caught — the allowlist and the 400 around it were not.

**Process lesson**, drawn from the reviewer's own note: cite only paths inside
the worktree actually under review, or a named committed ref (a SHA on
`main`, an issue number with a merged PR) — never a bare file:line that could
resolve against a sibling worktree. The reviewer's self-report also states
this cost a review round, so the diff received less adversarial attention in
round 1 than "two rounds, approved" on its own suggests; per the pipeline's
2-round default cap, this work would benefit from a further pass if one is
available.

**Round 2** approved, and additionally caught that a `DocumentBuilder`
comment asserted two user-mapped fields cannot collide onto the same
`sm_*` name — they can, via the same missing `_X` guard
`FieldMapper::encodeSolrName()`'s own KNOWN DIVERGENCE note documents
(`'a-b'` and `'a_X2d_b'` both map to `sm_a_X2d_b`). The merge behaviour itself
(append rather than overwrite) is still correct — it was only the
justification that overclaimed impossibility. Corrected in `1c58ab4`, which
also dropped stale "(#384, unmerged)" parentheticals now that #384 is on
`main`.

## Follow-ups deferred (none fixed in this PR)

Recording each with a recommendation on where it belongs. None of these are
regressions introduced here except where noted as parity with an existing
upstream bug.

1. **Hyphenated langcodes break the `multilingual`-several `suggest.cfq`.**
   The grouped-tag form appends each langcode raw, giving
   `drupal_X2f_langcode_X3a_pt-br`, while `DocumentBuilder` indexes
   `drupal_X2f_langcode_X3a_pt_X2d_br` — the two can never match. Verified to
   be upstream's own bug, verbatim (`Suggester.php:252` +
   `SearchApiSolrBackend.php:1347`): parity, not a regression introduced by
   this issue. **Recommend a new issue** (not a findings-doc entry — it is a
   module-code bug with a concrete fix, not a Solr-behaviour finding), scoped
   to whether Wayfinder should diverge from upstream here or stay in parity.
2. **`WayfinderBackend` around line 613: one inner `is_array()` guard is
   still missing** — `(is_array($phrases) ? $phrases['suggestions'] ?? [] :
   [])` warns if `suggestions` is present but scalar. Smaller sibling of the
   defect `53f22f6` already pinned at the outer level. **Recommend folding
   into the same follow-up issue as #2 below**, or its own small issue if a
   maintainer wants it separately — it is a one-line hardening fix.
3. **The pre-existing Terms autocomplete path does not dedupe**, though
   upstream's equivalent calls `filterDuplicateAutocompleteSuggestions()`; the
   helper this issue introduced for the Spellcheck/Suggester paths now exists
   and could be reused there. **Recommend a small follow-up issue** — cheap
   fix, but out of this issue's scope (Terms path, not touched here).
4. **`testSuggestCfqDoesNotDoubleEncodeAnAlreadyEncodedTag` is tautological**
   — its input is a fixed point of the encoder, so the assertion can't
   actually fail on a broken "already encoded?" check. The real coverage for
   that branch is the multilingual-several test. **Recommend fixing the test
   itself** in a future test-focused pass; not urgent since the behaviour is
   covered elsewhere, but the test as written proves less than its name
   claims.
5. **The `empty($dictionary)` tag-drop branch is unreachable via the
   plugin** — parity-only dead code, carried over from upstream's own
   structure. No action recommended beyond noting it; removing it would be
   pure code-golf against a mirror of upstream's branch shape.
6. **`hex2bin()` on an odd-length `_Xabc_` run warns and returns false.**
   Needs a literal uppercase `_X` inside an index id, which Drupal machine
   names cannot contain, so this is not reachable in practice. Upstream has
   the identical hole. No action recommended.
7. **`suggest.count` is unclamped.** `limit = 0` sends `suggest.count=0`,
   which the server answers with a 500, and the backend method degrades that
   to `[]` (via the existing exception-to-`[]` catch) rather than a useful
   result. **Recommend a small follow-up issue**: either clamp client-side to
   a minimum of 1, or confirm a 500-to-`[]` degrade is the intended behaviour
   and document it.
8. **The `_X` collision divergence in `encodeSolrName` is now load-bearing in
   three places** — field names, context tags, and the (tautological, #4
   above) already-encoded test. The reviewer named this as the area most
   deserving a further pass. **Recommend a dedicated follow-up issue** that
   audits every `encodeSolrName`/`decodeSolrName` call site together, rather
   than patching each collision report ad hoc as it is found (this report is
   the second: round 2 already found one).
9. **Two open questions are now cheap to settle by capture, not reasoning**,
   now that #384's 34 `suggest_q_*` fixtures are on `main`: the multi-dictionary
   response shape (no existing fixture repeats `suggest.dictionary`), and a
   `suggest_q_dict_unknown` capture that would either discharge or promote the
   finding-193-adjacent 400 ponytail from #1/#7 above. **Recommend adding both
   to `solr-ref/capture.sh`** in whichever issue picks up #384-adjacent
   fixture gaps — this is a capture task, not a findings-doc entry, since
   nothing is currently claimed as fact that a capture would contradict.

None of the above is proposed as a `docs/solr-ref-findings.md` entry on its
own initiative — items 1, 2, 4, 6 are module-code (PHP) facts, not captured
Solr behaviour, so the findings doc (which records Solr/Tantivy facts learned
from source or capture) is the wrong home; the Solr-side fact underlying #9
(`suggest.dictionary` first-wins) is already finding 193 on `main`. If a
maintainer wants any of 1–8 tracked, they read as GitHub issues, not findings.

## Verification commands run

```
vendor/bin/phpunit                 # 453 passed, 800 assertions, 0 failures
git log --oneline origin/main..HEAD   # 4 commits: 2 red-test, 1 impl, 1 doc-fix
git diff origin/main..HEAD --stat -- src/ tests/ solr-ref/   # empty — no Rust touched
cargo fmt --check                  # clean (sanity check only; no Rust diff here)
```
