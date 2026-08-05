# #390 — pin the two behaviours #384 left unpinned

**Date:** 2026-08-05. Issue #390. Branch
`markdlabrecque/issue-390-two-unpinned-behaviours` off `main`, pushed.
**Status:** green, reviewed (two rounds, approved), rebased onto `main`
including #394. Not yet merged — this report documents the work for
handoff.

## What was built

#384's report filed two hypotheses as "unfiled, needs one cheap capture,"
not known bugs:

1. An unknown `suggest.dictionary` — does it 400 like real Solr's
   `SuggestComponent` (`No suggester named <x> was configured`), or does it
   fall back to the `und` chain the way Wayfinder's shipped code does?
2. `suggest.cfq=` (empty string) — does it match nothing (an empty
   BooleanQuery under a MUST clause), or is it treated as absent?

Both were captured against real `solr:9` with the shipped Drupal configset
and **both hypotheses were wrong** — captured Solr diverged from both
Wayfinder's current behaviour and from what #384 guessed the *other*
answer might be:

- Unknown dictionary: 400, confirmed. Wayfinder was serving 200 under the
  `und` chain.
- Empty `suggest.cfq`: identical to no `cfq` at all, **including
  highlighting** — `suggest_q_cfq_empty.json` is byte-identical to
  `suggest_q_prefix_en.json` (same query, no `cfq`) modulo `QTime`. This is
  a third reading, not either of the two #384 posed: Wayfinder's document
  set was already correct (empty `cfq` parses to no clauses, so
  `cfq_passes` admits everything), but it suppressed highlighting, because
  the handler derived `highlight` from `cfq.is_none()` and an empty string
  is `Some("")`.

Fixes:

- `src/schema.rs`: `is_configured_suggester`, gating the `suggest.q` lookup
  path in `src/lib.rs`. The configured set is `LANGUAGES` plus
  `SUGGEST_UNDEFINED_DICTIONARY` — **not** the two suggesters the shipped
  `solrconfig_extra.xml` happens to declare (`en`, `und`). That narrower set
  was the first implementation's must-fix (see review, below): confirmed
  from source that `search_api_solr` emits one `SuggestComponent` per
  *installed* language field type
  (`config/optional/search_api_solr.solr_field_type.text_fr_7_0_0.yml:215-226`
  declares `name: fr`) and the client sends the site's Drupal langcode as
  the dictionary (`suggester/Suggester.php:247`, passed through at `:280`).
  An absent `suggest.dictionary` still defaults to `und`, which is
  configured, so only an explicitly-supplied unconfigured name 400s.
- `src/core_index.rs`: `cfq_engages_filter`, asking the parser
  (`Cfq::is_engaged`) whether any MUST/SHOULD/MUST_NOT clause was actually
  produced, rather than whether the parameter string was present. `highlight`
  in `src/lib.rs` now derives from this instead of `cfq.is_none()`. Empty
  and whitespace-only `cfq` follow the "absent" path (highlighted); `+()`
  — an empty MUST group that IS a clause and matches nothing — stays on the
  context-filtered, unhighlighted path.
- `docs/solr-ref-findings.md`: findings 195 and 196, recording both
  captures, every reading each one falsified, and (195) the langcode gap
  below.
- Fixtures: `solr-ref/responses/suggest_q_dict_unknown.json`,
  `suggest_q_cfq_empty.json`. Capture block appended to `solr-ref/capture.sh`
  (own container `wayfinder-solr-390`, port 9015, #384 corpus verbatim —
  it needs its own container because the #384 block releases its container
  at its own end, so an appended block reusing it would have had nothing to
  talk to).

Commits (post-rebase hashes, HEAD `4608c0c`):

- `a8c7e3c test(suggest): capture the two behaviours #384 left unpinned`
- `4959a14 test(suggest): pin the two #390 unpinned suggest behaviours (red)`
- `43edafe fix(suggest): reject an unknown dictionary, treat empty cfq as absent`
- `4608c0c docs(suggest): name the langcode gap the dictionary gate leaves`

## Process note: a filtered `capture.sh` run died silently

`capture.sh --only '^suggest_q_...'` could not be run as written. The
script's unguarded first block reuses an already-running
`wayfinder-solr-ref` container **by name** while addressing it **by port**.
A sibling session had that container up on port 9983 while the script's
`SOLR_PORT` default is 8983; the name check matched the running container,
the script assumed it was already listening on the default port, the ping
against 8983 never succeeded, and the run died in the first block before
reaching any `suggest` capture — with exit 0, no error surfaced. The
capture was instead run by extracting the committed #390 block plus the
script's `want`/`want_any`/`release` helpers into a scratch runner, so the
code that produced the fixtures is exactly what is committed in
`solr-ref/capture.sh`. This container-reuse-by-name-vs-port mismatch is a
latent trap for any filtered run in a multi-worktree setup; listed as a
follow-up below rather than fixed here (out of this issue's scope).

## Test evidence

```
cargo test --no-fail-fast          # 1544 passed, 0 failed, 1 ignored (73 suites)
cargo fmt --check                  # clean
cargo clippy --all-targets -- -D warnings   # clean
```

The 1 ignored is the pre-existing `tests/sort_copy_bloat.rs` #362
measurement test, unrelated to this change. Re-run by the orchestrator
independently after every stage (test-writer, implementor, both review
rounds, and again after the rebase onto `main`) rather than trusting the
subagents' self-reports.

New tests in `tests/suggest.rs`:
- `suggest_q_dict_unknown_matches_fixture` — an unconfigured dictionary 400s
  matching the fixture.
- `suggest_q_no_dictionary_still_served_as_und` — guard: an *absent*
  `suggest.dictionary` must keep defaulting to `und`, so the new check
  cannot be widened onto the default path.
- `suggest_q_per_language_dictionary_is_served` — guard: `fr` and `de` both
  serve 200 keyed by the requested dictionary, so the configured set cannot
  be re-narrowed back to `{en, und}` silently.
- `suggest_q_cfq_empty_matches_fixture` — empty `cfq` matches the captured
  fixture.
- `suggest_q_cfq_empty_matches_no_cfq_body` — deliberately a comparison
  against the *sibling* no-`cfq` response, not only the fixture, so a future
  edit cannot reproduce the fixture's bytes by some other means while
  reintroducing an empty-string special case.

**Mutation testing**, all reverted after confirming the intended test caught
each: dictionary check disabled entirely; configured set narrowed to
exclude `und`; configured set re-narrowed to `{en, und}` (caught by
`suggest_q_per_language_dictionary_is_served`); highlight reverted to
`cfq.is_none()`; `Cfq::is_engaged` forced to always return `false`. Each
mutation went red on a named test; each was reverted.

## Review outcome

**Two rounds, approved on round 2.**

Round 1 bounced with a real must-fix: the first implementation hardcoded
the configured set as `["en", "und"]`, justified from the shipped
`solrconfig_extra.xml`. That would 400 a dictionary real clients send —
verified independently by the orchestrator against both citations before
accepting the bounce (`search_api_solr.solr_field_type.text_fr_7_0_0.yml`
and `Suggester.php`, above) rather than taking the reviewer's word for it.

Round 2 widened the configured set to `LANGUAGES` +
`SUGGEST_UNDEFINED_DICTIONARY` and added
`suggest_q_per_language_dictionary_is_served` as the regression guard.
Round 2 approved, and the orchestrator independently mutation-verified that
re-narrowing the set back to `{en, und}` fails exactly that one test.

## A ceiling the rebase surfaced, not either review round

Rebasing onto `origin/main` after #394 (the `search_api_wayfinder`
Suggester/Spellcheck PHP reimplementation) landed surfaced a real gap
neither review round could have seen, because the PHP client that exercises
it did not exist yet when either round ran:
`drupal/search_api_wayfinder/src/QueryBuilder.php:497` sends the Drupal
langcode as `suggest.dictionary`, and Drupal has far more langcodes than
`LANGUAGES` has entries (18). A `ja`, `pl`, or `zh-hans` site now gets a 400
where its own real Solr — having installed `text_ja` — answers 200; before
this gate landed, the same request got 200 with `und` results.

Neither answer is right for both inputs: `xx` is the fixture's captured
400, `ja` is real Solr's captured 200, and with no configset to consult
nothing server-side can tell the two apart. The 400 was kept, because
Wayfinder has no `text_ja` analyzer chain at all — a `ja` dictionary was
never actually *served*, only silently substituted with `und` before this
change, and failing loudly names the ceiling instead of hiding it. Recorded
as a `ponytail:` comment on `is_configured_suggester` and as part of
finding 195 in `docs/solr-ref-findings.md`. The real fix is `LANGUAGES`
growing to cover Drupal's langcode space — filed as a follow-up, not fixed
here (out of scope for this issue, and no fixture exists yet for any
specific missing langcode).

## Findings recorded

`docs/solr-ref-findings.md`:
- **195** — unknown `suggest.dictionary` is a 400, not a fallback; also
  documents the langcode-gap ceiling above.
- **196** — empty `suggest.cfq` is identical to no `cfq` at all, including
  highlighting; documents all three readings the capture discharged.

## Follow-ups (none fixed in this branch)

1. **The `LANGUAGES` set (18 entries) vs Drupal's langcode space.** A
   `ja`/`pl`/`zh-hans` site now 400s on `suggest.dictionary` where before
   this gate it silently degraded to `und`. The real fix is growing the
   configured language set; recommend a follow-up issue scoped to which
   langcodes to add and whether a full Drupal-langcode-to-analyzer mapping
   is the right shape, or something coarser.
2. **`capture.sh`'s first block reuses a container by name while addressing
   it by port.** In a multi-worktree setup with another session's
   `wayfinder-solr-ref` container up on a non-default port, a filtered run
   dies silently in block 1 with exit 0 and no capture ever happens.
   Recommend either checking the port the running container is actually
   bound to, or naming the container per port so a stale name match can't
   occur.
3. **A repeated `suggest.dictionary` is first-wins server-side**
   (`Params::get`), so #385's multilingual branch (`Suggester.php:253`,
   which sends one `suggest.dictionary` per resolved langcode) only ever
   gets the *first* dictionary's suggestions back from a `de`+`fr` site —
   German only, French silently dropped. Real Solr accepts several
   `suggest.dictionary` params and answers keyed per dictionary. Uncaptured
   — no #384 fixture repeats the param, so this is inference from source
   (`QueryBuilder.php`'s own `ponytail:` already names it) rather than a
   captured fact. This is server-side work (multi-dictionary lookup and
   response-shape support), not something the PHP client can fix alone.
4. **The two `/suggest` error paths' `omitHeader` behaviour is an
   inference, not a captured fact.** Both now honour `omitHeader`, but no
   fixture in this branch or #384's set specifically pins that Solr's error
   envelope for these two paths honours it. Low-risk (both other `/suggest`
   error paths already honour it), but worth naming rather than silently
   assuming.

Per the pipeline's default cap, this work went two review rounds and was
approved on round 2 — it did not hit the cap, so no additional-passes
caveat is owed here, but follow-ups 1–4 above are exactly the kind of thing
a further pass would most likely pick up next, particularly #1, which has
a live client already sending the langcode that breaks it.

## Verification commands run

```
cargo test --no-fail-fast                             # 1544 passed, 1 ignored, 73 suites
cargo fmt --check                                      # clean
cargo clippy --all-targets -- -D warnings              # clean
git log --oneline origin/main..HEAD                    # a8c7e3c, 4959a14, 43edafe, 4608c0c
```
