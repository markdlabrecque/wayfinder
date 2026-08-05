# #384 — suggest: serve the `suggest.q` read path

**Date:** 2026-08-05. Issue #384 (open, assigned). Branch
`markdlabrecque/issue-384-serve-suggest.q-read` off `main`.
**Status:** implementation complete, all gates green locally. **Not yet merged** —
this report documents the work for handoff; PR/merge is a separate step.

## What was built

The `/suggest` read path: Wayfinder now answers real `suggest.q` lookups so
`search_api_solr_autocomplete`'s Suggester plugin is supportable. Params
`suggest=true`, `suggest.q`, `suggest.dictionary`, `suggest.count`,
`suggest.cfq`, `suggest.highlight`. Wire shape
`{"suggest":{"<dictionary>":{"<suggest.q>":{"numFound":N,"suggestions":[...]}}}}`.

Four commits carry the detail; read them rather than taking this summary as
the source:

- `695910b feat(suggest): serve the suggest.q read path (#384)` — first cut
  plus fixtures
- `cb81f3f test(suggest): capture the 9 fixtures the round-3 review needed (#384)`
- `9a0ba99 test(suggest): red tests for the 9 round-3 fixtures (#384)`
- `10a8722 feat(suggest): match Lucene's highlight and cfq semantics (#384)`

Key files: `src/lib.rs` (routing, `SUGGEST_PARAMS`, the handler and its
`suggest.count <= 0` error envelope), `src/core_index.rs` (`suggest_lookup`,
`suggest_match`, `highlight_spans`, `parse_cfq`/`Cfq`/`cfq_passes`),
`src/schema.rs` (the `wayfinder_suggest_<code>_v1` chains,
`SimpleLowerCaseFilter`, `dictionary_tokenizer`), `src/error.rs`
(`with_suggest`), `solr-ref/capture.sh` (the capture block, container
`wayfinder-solr-384`, port 9014), `tests/suggest.rs` (46 tests).

## The part this report most needs to record: what was wrong, and how it was caught

This went **four review rounds**, one more than the reviewer's 2-round default
cap — the round-4 escalation is recorded below, not silently absorbed.

Round 3 found that four behaviours had been *guessed* with no fixture pinning
them, including a reachable 500 (`suggest.q=ke` against a phrase containing
U+212A KELVIN SIGN panicked on a non-char-boundary slice). All four were
settled by capturing against real `solr:9`, and the review was right on every
one:

| Claim | Captured verdict |
|---|---|
| Panic in highlight span arithmetic | Confirmed — `ke` -> `<b>Ke</b>lvin degrees` bolds two CHARACTERS; the code added byte lengths |
| A non-final token bolds the stem length, not the whole surface | Confirmed — `studies show` -> `case <b>studies</b> <b>show</b> progress` |
| A trailing separator makes the last token exact | Confirmed — `qui ` returns 0 where `qui` returns 3 |
| `parse_cfq`'s paren arm was broken | Confirmed, and had to be *implemented*, not deleted — `+(site_alpha site_beta)` returns 2 |

The captures also produced two findings nobody had asked for:
`suggest.q=k` and `suggest.q=i` return 0 because both shipped dictionary
analyzers carry `LengthFilterFactory min="2"`, and `istanbul` fails to match
`Istanbul`-with-U+0130 because Rust's `to_lowercase` is the FULL Unicode
mapping where Java's `Character.toLowerCase` is the SIMPLE 1:1 one. Both
root-caused to one structural gap — the suggest path was reusing the global
`text_*` presets instead of reproducing `suggestAnalyzerFieldType` — so the
fix was one new analyzer chain, not three patches.

Round 4 then found two more, both confirmed against sources rather than
guessed: `RemoveLongFilter::limit(40)` is BYTE-based, so the chain's real
bound was 39 bytes and not the 100 characters three comments claimed (a
45-byte word or a 14-character CJK token survives Solr and was dropped here);
and the ponytail named one of four remaining analyzer gaps. Both fixed in
`10a8722`.

**Process note worth recording:** author/reviewer separation was lost
mid-task — the implementor subagent died twice to API 529 and the
orchestrator applied a round-2 must-fix itself. It was restored by running a
real test-writer -> implementor pipeline, with the red tests committed
separately in `9a0ba99` so "the implementor edited no test" stays checkable
from git. The round-3 reviewer's own escalation said the self-reviewed
material is where the new defects were — and rounds 3 and 4 both bore that
out. Round 4 escalated rather than bounced, because four rounds exceeds the
2-round cap; this report is that escalation's record.

Also record the false-green root cause the reviewer named: a fixture could be
committed with no test asserting it. `tests/suggest.rs` now has an orphan
guard, `every_suggest_q_fixture_is_asserted_by_a_test`.

## Evidence

Re-run locally (not merely restated from the agents' claims) on
2026-08-05, worktree `issue-384-serve-suggest.q-read`, HEAD `10a8722`:

```
cargo fmt --check                                    # clean
cargo clippy --all-targets -- -D warnings             # clean, "No issues found"
cargo test --no-fail-fast                             # 1522 passed, 1 ignored, 72 suites
cargo test --test suggest                             # 46 passed
cargo test --test suggest --test differential         # 46 + 44 = 90 passed
ls solr-ref/responses/suggest_q_*.json | wc -l         # 34
```

All green. No red tests, no ignored suggest tests, no `#[ignore]` in
`tests/suggest.rs`.

The `suggest_q_*` fixtures are deliberately absent from
`solr-ref/manifest.tsv` (they need their own core and corpus), so they get no
differential-harness coverage and rely entirely on `tests/suggest.rs`, where
`assert_lookup_matches` compares the whole response body modulo `QTime`.

## Review outcome

**Four rounds, capped-out escalation — not a clean approval.** Round 1 and 2
findings were fixed in place; round 3 (four confirmed bugs, two unrequested
findings, listed above) drove a full test-writer -> implementor cycle
(`cb81f3f` + `9a0ba99` + part of `10a8722`); round 4 (two more confirmed
issues) was fixed in `10a8722` and then escalated rather than sent to a fifth
round, per the reviewer's 2-round default cap. Per the pipeline rules, this
work could use further review passes — a fifth round was not run, and the
follow-ups below (particularly the two unfiled hypotheses) are exactly the
kind of thing an additional pass would most likely surface next.

## Follow-ups

- **#388** (open) — global `text_en`/`text_general` presets still lack
  `LengthFilter min=2` and still use Rust's full-Unicode lowercasing, so
  `/select` keeps diverging. Filed.
- **#389** (open) — four analyzer components missing from the suggest and
  `text_*` chains: accent folding `MappingCharFilter` (both analyzers),
  `SynonymGraphFilter` (query-side only), `StandardTokenizer` vs
  `SimpleTokenizer`, `WordDelimiterGraphFilter`. Includes the note that
  closing the WDGF gap invalidates `core_index.rs`'s `last_is_prefix`
  derivation ("the last token's end IS `maxEndOffset`" holds only while no
  graph filter is in the chain), so both must move together. Filed.
- **Unfiled**, from the round-4 review, each a **hypothesis needing one
  cheap capture**, not a known bug:
  (a) an unknown `suggest.dictionary` currently answers under the `und`
  chain, where `SuggestComponent` may instead 400 with `No suggester named
  <x> was configured` — the shipped configset defines only `en` and `und`, so
  Wayfinder's 18-language fan-out is invention;
  (b) `suggest.cfq=` (empty string) currently means "no filter" and admits
  every doc, where an empty BooleanQuery under a MUST clause would match
  nothing.
- The orphan guard matches the fixture name anywhere in the file, so a name
  in a doc comment satisfies it. All 34 fixtures do reach a real
  `fixture(...)` call today (confirmed by reading the test), so it is not
  lying, but tightening the needle to `fixture("<name>")` would make it
  enforce what its comment says.
- Branch name `markdlabrecque/issue-384-serve-suggest.q-read` does not follow
  the project's `<issue>-<short-slug>` convention (e.g. `384-suggest-q-read`).
- Not yet done at report time: no PR opened, no merge, `main` not updated.
  Issue #384 is still open and assigned to `markdlabrecque`.
