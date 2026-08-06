# SPEC-389 — accent folding, synonyms, StandardTokenizer, index/query split

Issue: https://github.com/markdlabrecque/wayfinder/issues/389
Branch: `markdlabrecque/issue-389-accent-folding-synonyms` (off `origin/main` @ 6773c6f)
Scope decision: **all four steps**, landed as four sequential commits on one PR.

## Ground truth (already verified by the orchestrator — do not re-derive)

`solr-ref/search-api/configset/` already vendors every resource file. No download needed.

### The two shipped chains, verbatim

`text_en` — `schema_extra_types.xml:45`:

```
index:  MappingCharFilter(accents_en.txt)
        StandardTokenizer
        Stop(ignoreCase, stopwords_en.txt)
        WordDelimiterGraph(catenateNumbers=1 generateNumberParts=1
                           protected=protwords_en.txt splitOnCaseChange=0
                           generateWordParts=1 preserveOriginal=1
                           catenateAll=0 catenateWords=1)
        Length(min=2 max=100)
        LowerCase
        SnowballPorter(English, protected=protwords_en.txt)
        RemoveDuplicates

query:  MappingCharFilter(accents_en.txt)
        StandardTokenizer
        SynonymGraph(synonyms_en.txt, expand=true, ignoreCase=true)
        Stop(ignoreCase, stopwords_en.txt)
        WordDelimiterGraph(catenateNumbers=0 generateNumberParts=1
                           protected=protwords_en.txt splitOnCaseChange=0
                           generateWordParts=1 preserveOriginal=1
                           catenateAll=0 catenateWords=0)
        Length(min=2 max=100)
        LowerCase
        SnowballPorter(English, protected=protwords_en.txt)
        RemoveDuplicates
```

`text_und` — `schema_extra_types.xml:166`: the same two chains with `_und` resources,
**no SnowballPorter**, and an extra `FlattenGraph` after WDGF on the **index** side only.
`stopwords_und.txt` and both `protwords_*.txt` are **empty**; `synonyms_und.txt` is one line
(`drupal, durpal`).

`suggestAnalyzerFieldType` is **not** a distinct field type: `solrconfig_extra.xml:39,51`
points the `en` and `und` suggesters at `text_en` / `text_und`. So there are exactly two
field types to reproduce, each with two analyzers — four chains total.

### Resource-file facts that change the design

1. **`synonyms_en.txt` has no multi-word entries.** 17,477 lines, every line
   comma-separated single tokens (`abettor, abetter`). Verified: zero lines where any side
   contains a space. `synonyms_und.txt` is `drupal, durpal`.
   **Consequence:** with the shipped data `SynonymGraphFilter` emits only same-position
   stacked tokens with `positionLength == 1`. It produces **no graph**. The issue's claim
   that synonyms invalidate `last_is_prefix` does not hold for the shipped tables — only
   WDGF's `preserveOriginal`/`catenate*` does. State this in the code comment; do not
   silently rely on it without saying so.

2. **`accents_*.txt` contain length-changing mappings.** 92 mappings in `accents_en.txt`,
   74 in `accents_und.txt`. Sixteen expand: `Æ`→`AE`, `Ĳ`→`IJ`, `Œ`→`OE`, `Þ`→`TH`,
   `æ`→`ae`, `ĳ`→`ij`, `œ`→`oe`, `ß`→`ss`, `þ`→`th`, `ﬀ`→`ff`, `ﬁ`→`fi`, `ﬂ`→`fl`,
   `ﬃ`→`ffi`, `ﬄ`→`ffl`, `ﬅ`→`st`, `ﬆ`→`st`.
   **Consequence:** Lucene's `MappingCharFilter` keeps an offset-correction map so token
   offsets refer to the **original** text. `core_index.rs:4177`'s
   `*last_end == query.len()` compares against the original query's byte length, so the
   folding stage **must** preserve original offsets. A naive "fold the string, then
   tokenize" implementation breaks this. This is the single highest-risk detail in the
   whole change.

3. **`accents_en.txt:20` is `"\U0106" => "C"` with a capital `U`.** Every other line uses
   lowercase `\uXXXX`. Lucene's `MappingCharFilterFactory` parse of an unknown escape is
   **not assumed** — it is a capture target (see F5 below). Do not implement `Ć` folding
   until the fixture says whether real Solr folds it. `accents_und.txt` has no such line.

### Wayfinder's current state

- Everything analyzer-side is `src/schema.rs`. `build_tokenizers` at `src/schema.rs:1118`.
- One `TokenizerManager`, cloned into Tantivy's indexing **and** fast-field managers
  (`src/core_index.rs:889,890,902,903`). **Query-time analysis uses the indexing
  tokenizer everywhere** — `src/core_index.rs:2516`, `:3811`, `:4148`. There is no
  index/query analyzer seam. The one field with two analyzers (`boost_term_payload`)
  exploits the indexing-vs-fast-field manager split, which is *not* reusable here.
- No char-filter stage exists at any level.
- Existing custom `TokenFilter`s, all `src/schema.rs`: `PorterTerminalYFilter` (`:679`),
  `LengthFilter` (`:743`), `SimpleLowerCaseFilter` (`:819`), `RemoveDuplicatesFilter`
  (`:885`), `DelimitedPayloadStripFilter` (`:951`).
- `ANALYZER_CONTRACT` and the `wayfinder_text_en_v2` naming precedent: `src/schema.rs:183-199`,
  `:134`. **An index-side chain change is a term-format change** → new contract version and
  reindex gate. A query-only change is not.
- The four gaps are already enumerated in the `ponytail:` block at `src/schema.rs:1209-1239`.
  Delete the entries this branch closes; do not leave them.
- Nothing in `src/` reads any `solr-ref/` resource file today. There is no Solr
  resource-file parser.

### Test surfaces that must not regress

- `tests/schema_layer.rs` — primary analyzer suite (`text_presets_tokenize_as_expected:174`,
  the analyzer-contract tests at `:218`–`:526`).
- `tests/suggest.rs` — ~47 fixture-pinned `suggest_q_*` tests. This is the suite an accent
  or tokenizer change is most likely to break.
- `tests/terms.rs` — its module doc (`:25-70`) explicitly disclaims `accents_en.txt` as
  unverified at `:61`. Update that disclaimer when it stops being true.
- `tests/query_types.rs:24,710` depend on `SimpleTokenizer` token shapes.

## Phase 0 — capture the fixtures (blocking; no behaviour change)

Per the repo contract: pin every divergence before changing behaviour. Follow the repo's
convention of capturing **observable endpoint responses**, not `/analysis/field` — there
are no analysis fixtures today and Wayfinder serves no analysis handler.

`/terms` pins the **index**-side token stream exactly (it lists indexed terms).
`/select` pins the **query** side. Use both.

Rules:
- Append **one block at the end of `capture.sh`**, in the established style (dedicated
  one-off container, own port, own core name, `search-api` configset, `SOLR_MODULES=analysis-extras`,
  wait-for-ping loop, `docker rm -f` afterwards).
- Use a port not already claimed in `capture.sh`. Check.
- **Run `capture.sh` with its `ONLY` regex argument** so nothing else is re-captured.
  A bare re-run does `rm -rf "$OUT"` and truncates the manifest. Commit new fixtures
  before doing anything else with `solr-ref/`.
- **Do not add manifest rows — not to `manifest.tsv`, not to `manifest-errors.tsv`.**
  The differential harness (`manifest.tsv` + `manifest-errors.tsv` +
  `tests/differential.rs`) is being retired as of 2026-08-05. Its single-core replay loop
  cannot host fixtures captured from a `search-api`-configset core, and the
  `manifest-errors.tsv` workaround costs a new schema const, app fn, and another parameter
  on `app_and_request_url`'s already-21-arg signature per core.
  The project `CLAUDE.md` still calls the differential harness "the evidence for the
  compatibility claim" — **the repo doc is stale on this point**; this spec overrides it.
  Instead: capture into `solr-ref/responses/` via a `capture.sh` block as usual, then
  assert each fixture from an ordinary integration test that loads and compares it.
  Verified precedents, both of which are in **neither** manifest:
  `select_fl_ss_wildcard` (`tests/select_fl_wildcard.rs:357`, via the `fixture()` helper)
  and the `suggest_q_*` set (`tests/suggest.rs`) — a file-local schema const + corpus fn +
  app builder, a `QTime` normaliser, and one thin `#[tokio::test]` per fixture.
  Read `tests/select_fl_wildcard.rs` before writing the capture block, so the fixture you
  capture is shaped the way the test that will consume it needs.

Fixtures to capture, each named `an389_*`:

- **F1 accent folding, index side.** Index docs whose `text_en`/`text_und` field holds
  `Café Central`, `Æther`, `Straße`, `ﬁle`, `Ćwik`. `/terms` on that field. Pins exactly
  which folded terms Solr indexes.
- **F2 accent folding, query side.** `/select?q=` for `cafe`, `aether`, `strasse`, `file`,
  `cwik` against the same corpus. Pins the match/miss the issue's headline claim rests on.
- **F3 StandardTokenizer vs SimpleTokenizer.** Corpus with a contraction (`don't`), a
  hostname (`www.example.com`), a decimal (`3.14`), an underscore (`foo_bar`), a hyphen
  (`e-mail`), CJK (`東京都`), an email (`a@b.com`). `/terms` plus targeted `/select`.
  This is where UAX#29 and WDGF interact — capture enough to separate the two.
- **F4 synonyms, query side only.** `/select?q=durpal` and `q=drupal` against a corpus
  containing `drupal` (present in **both** `synonyms_en.txt` and `synonyms_und.txt`, so it
  works for `text_en` and `text_und`). Also an `en`-only pair from `synonyms_en.txt`
  (e.g. `abettor` / `abetter`). Then the asymmetry proof: `/terms` must show the synonym
  is **not** in the index (query-side only).
- **F5 the `\U0106` escape.** `Ćwik` in the corpus, `/terms` to see whether it indexed as
  `cwik` or `ćwik`, and `/select?q=cwik`. Decides whether `accents_en.txt:20` folds at all.
- **F6 WDGF asymmetry.** A token with an internal number and case boundary
  (`WiFi-2000`, `ABC123`, `SKU-42`). `/terms` (index side: `catenateNumbers=1`,
  `catenateWords=1`) and `/select` probes for the catenated vs split forms
  (query side: both `0`). The index/query split is only justified if this fixture shows
  the asymmetry — if it does not, say so and drop the claim rather than building to it.
- **F7 `last_is_prefix` under a graph filter.** `/suggest?suggest.q=` probes on phrases
  whose WDGF output makes the last token's end offset **not** the max end offset. Enough
  to re-derive the rule, since step 4 invalidates the current derivation.

Also record a finding per learned Solr fact in `docs/solr-ref-findings.md` (append,
numbered), and cite finding numbers in the tests.

**Gate:** Phase 0 lands as its own commit (fixtures + capture.sh block + findings). No
`src/` change, no manifest change. `cargo test` still green.

## Phase 0 RESULTS — captured and committed (commit 2a6ff0d)

> **STATUS: REFERENCE, NOT BINDING.** Read "PREMISE CHANGE (2026-08-05)" below before acting
> on anything in this section. These are accurate observations of Solr, and still the best
> description of what the shipped chains do — but the *instructions* embedded here ("Phase 2
> implements the bug, not the intent", "Do not reach for `unicode-normalization`", "Do not
> 'fix' this", "must be reproduced") are **void**. The project no longer matches Solr.
> Where this section and the PREMISE CHANGE section conflict, PREMISE CHANGE wins.

82 fixtures `solr-ref/responses/an389_*.json`, findings **195-204** in
`docs/solr-ref-findings.md`. In neither manifest; assert with the `fixture()` helper
(`tests/common/mod.rs:465`), as `tests/select_fl_wildcard.rs` and `tests/suggest.rs` do.
Corpus shape: every doc carries the same value in a `tm_X3b_en_*` (`text_en`) and
`tm_X3b_und_*` (`text_und`) twin, so each `_en`/`_und` pair isolates one variable.
**Every `/select` fixture here is a bare `response` object with no `responseHeader`/`QTime`**
(the shipped handler sets `omitHeader=true`, `solrconfig_extra.xml:115`); only the
`/suggest` fixtures need a `QTime` normaliser.

Facts that **override** the "Ground truth" section above:

- **F5 / finding 195: `accents_en.txt:20` `"\U0106" => "C"` does NOT fold `Ć`.**
  Lucene's `MappingCharFilterFactory` unescapes `\uXXXX` but has no throw for an
  unrecognised escape — the backslash is dropped — so the capital-`U` line installs a
  literal ASCII rule `U0106` → `C`. Evidence: `an389_terms_accent_en` indexes `ćwik` /
  `ćwikowski` unfolded and contains **`zzczz`** for input `zzU0106zz`;
  `an389_terms_accent_und` (no `\U` line) keeps `zzu0106zz`.
  **Phase 2 implements the bug, not the intent**, with a comment saying so — otherwise a
  later reader deletes it as an obvious typo. Total real mappings: 91 for `en`, not 92.
- **Finding 196: `MappingCharFilter` is codepoint-literal and nothing in the chain
  normalises.** `an389_terms_accent_en` lists **both** `cafe` (precomposed U+00E9) and
  `café` (NFD, U+0301), and the two `/select` probes each return a different single doc.
  **Do not reach for `unicode-normalization`** — NFC-folding the input first would make
  those two documents collide.
- **Finding: synonyms cannot produce a graph with the shipped tables** (confirms fact 1
  above). `an389_terms_syn_en` contains only `abettor`, `drupal` — the synonyms are absent
  from the index — while `q=durpal` and `q=abetter` each match. Query-side-only, proven.
  `q=abetter` on `und` returns 0 (`synonyms_und.txt` is only `drupal, durpal`).
- **F6 / finding: the WDGF index/query catenate asymmetry IS observable, in both
  directions.** `an389_select_cat_joined_en` (`q=wifirouter`) → **2** docs;
  `an389_select_cat_delim_en` (`q=wifi_router`) → **1**. Same on `und`, so not a stemming
  artefact. Numbers likewise: `q=102030` → 2, `q="10.20.30"` → 1.
  **So Phase 1's split is required, not speculative.**
- **Finding: `q.op=AND` is NOT an isolation tool** for the missing query-side catenation —
  query-side WDGF stacks alternatives at one position and `q.op` combines positions, not
  intra-position alternatives. Both AND probes still return 1. Do not use it in Phase 4.
- **Index-side token streams, verbatim from `an389_terms_tok_en`:**
  `14, 3.14, 314, b.com, bar, bcom, com, exampl, foo, foo_bar, foobar, mail, www,
  www.example.com, wwwexamplecom, don, don't, dont`
  (`_und` is identical but for `example` instead of the stemmed `exampl`). So:
  hyphen splits at the **tokenizer** (`e-mail` → only `mail`; `e` dies to `min=2`;
  `q=email` → **0**), underscore splits at **WDGF** (`foo_bar` → `foo_bar, foo, bar,
  foobar`; `q=foobar` → 1), `3` dies to `min=2`.
- **CJK is unindexable in the shipped types.** `東京都` yields **no terms at all** and
  `q=東京都` → 0 on both `en` and `und`: StandardTokenizer emits unigrams and
  `LengthFilter(min=2)` deletes every one. Do not "fix" this.
- **F7 / finding 203-204: `last_is_prefix` under WDGF.** The prefix branch survives only
  while the analyzed stream ends in a *single* token reaching the input's end; every
  earlier token becomes a *required exact* term. Observed: `foo`→1, `foo_`→0, `foo_ba`→0,
  `foo_bar`→1, `wifi_router`→0, `wifi_router_20`→0, `wifi_router_2000`→1, `3.`→0, `3.1`→1,
  `e-m`→0, `e-ma`→1, `don'`→1, `don't`→1, and `dont`→1 **with no `<b>` highlight span at
  all** (finding 204: a match can have no highlightable span). WDGF therefore breaks
  mid-delimiter autocomplete — that is Solr's behaviour and must be reproduced.

Also: `capture.sh`'s `ONLY` is a **flag, not positional** (`capture.sh:31-41`) —
`--only '^an389_'`. And `SOLR_PORT=9983` is needed because the script's preamble waits on
the pre-existing `wayfinder-solr-ref` container (8983 is held by `ddev-router`).

## Phase 1 — index/query analyzer split

Structural prerequisite. No observable behaviour change on its own.

- Register a second analyzer per field type under a query identity, and give the
  query paths (`src/core_index.rs:2516`, `:3811`, `:4148`) a lookup that selects it.
  Prefer an additive helper over editing a signature every call site shares.
- When no query analyzer is registered for a type, the query path must fall back to the
  index analyzer, so every existing field type behaves exactly as before.
- **Do not bump `ANALYZER_CONTRACT`** in this phase — no index term changes.
- Tests: a field type with deliberately different index and query chains analyzes
  differently on the two paths; every existing chain is unaffected.

## PREMISE CHANGE (2026-08-05) — the Solr contract no longer binds

**The project has broken away from the Solr compatibility contract.** Mark confirmed this
after Phase 1 was implemented. The four features of #389 still ship, but they are now
justified by **search quality**, not by parity, and they are to be implemented **correctly**
rather than bug-compatibly.

The compatibility contract itself (the project `CLAUDE.md` section, the framing of
`docs/solr-ref-findings.md`, the differential harness) is being handled by **another ticket
currently in flight**. So in this branch:

- **Do not** edit `CLAUDE.md`'s compatibility contract, or reframe the findings doc.
- **Do not** extend or re-run `capture.sh`, and do not capture new fixtures.
- **Keep** the committed Phase 0 fixtures and findings 195-204 exactly as they are. Their
  status changes from *assertion* to *reference*: they record how Solr behaves, which is
  useful context, and are no longer the thing we must match.

### What this reverses

Findings 195, 196, the CJK result, and finding 203-204 documented Solr behaviours that are
**defects**. Phase 0's instructions to reproduce them are void:

| Phase 0 said | Now |
|---|---|
| Implement the `\U0106` → literal `U0106` bug, "bug and all" | **No.** Fold `Ć` → `C` properly. |
| "Do not reach for `unicode-normalization`" — keep NFC/NFD as distinct terms | **Reversed.** Normalise, so `café` matches `café` however it is encoded. |
| CJK unindexable via `LengthFilter(min=2)`; "do not fix this" | **Fix it.** CJK must be searchable. |
| `foo_` → 0 suggestions; WDGF breaks mid-delimiter autocomplete, "must be reproduced" | **Do not reproduce.** Mid-delimiter autocomplete should work. |
| `e-mail` → only `mail` (`e` dies to `min=2`); `3.14` loses `3` | **Do not reproduce** the blind `min=2` cut. |

### The one hard constraint

Roughly 47 existing tests in `tests/suggest.rs`, plus parts of `tests/schema_layer.rs`,
`tests/terms.rs`, and `tests/query_types.rs`, pin Solr-derived behaviour — including some of
the defects above. **Correcting a behaviour these tests pin is out of scope for this branch
unless it is one of the four features.** Where implementing a feature correctly forces an
existing test to change:

1. Do **not** edit, weaken, or `#[ignore]` the test.
2. **Escalate to the orchestrator** with the test name, its current expectation, what the
   correct behaviour would be, and why the feature cannot be implemented without changing it.

That call is mine, not a stage's. Silently rewriting a pinned expectation is the specific
failure mode to avoid here — under the old contract it was forbidden, and under the new one
it is *tempting*, which makes it more dangerous, not less.

## Phase 2 — accent folding, done correctly

Goal: `cafe` matches `Café Central`, and equivalent text matches regardless of Unicode
encoding form. This is the highest-value, lowest-risk feature and the issue's headline.

- **Implement folding as a `TokenFilter`, not a char filter.** This is a deliberate
  departure from Solr's `MappingCharFilter` position in the chain, and it is a large
  simplification: a post-tokenization filter leaves token offsets pointing at the original
  text, so the whole offset-correction problem — and the risk to `last_is_prefix`
  (`src/core_index.rs:~4212`), which compares a token end offset against `query.len()` —
  disappears. Accented letters are still letters under UAX#29, so token boundaries are
  unaffected by folding either way. Say this in the comment, including why the Solr
  position was not copied.
- **Do not hand-transcribe `accents_*.txt`.** A 91-entry table was the wrong tool; it misses
  most of Unicode and contains a parse bug. Implement the standard approach instead:
  NFKD-normalise, strip combining marks (Unicode category `Mn`), then apply a small explicit
  expansion table for the letters that do not decompose — at minimum `ß`→`ss`, `æ`→`ae`,
  `œ`→`oe`, `þ`→`th`, `ð`→`d`, `ø`→`o`, `đ`→`d`, `ł`→`l`, and the uppercase forms. This is
  what Lucene's own `ASCIIFoldingFilter`/`ICUFoldingFilter` do, and it is well-trodden.
  NFKD already handles the `ﬁ`/`ﬂ`/`ﬃ` ligatures and `Ĳ`.
- Apply to **both** index and query analyzers of the text presets and the
  `wayfinder_suggest_*` chains. Folding must be symmetric or it silently stops matching.
- A new dependency (`unicode-normalization`) carries a justifying comment in the style of the
  rest of `Cargo.toml`.
- **Bump `ANALYZER_CONTRACT`**; follow the `wayfinder_text_en_v2` → `_v3` precedent. This
  changes terms on disk and needs the reindex gate.
- Mutation-test the folding: break the expansion table and the `Mn` strip independently, and
  confirm a test catches each.
- The `an389_accent_*` fixtures are now **reference, not assertions**. Expect to *diverge*
  from `an389_terms_accent_en` deliberately: we fold `ćwik`, Solr does not. Note the
  intentional divergences in the test comments so the next reader does not "fix" them back.

## Phase 3 — UAX#29 tokenizer

- Replace `SimpleTokenizer` (split on non-alphanumeric) with UAX#29 word segmentation via
  `unicode-segmentation`, dropping punctuation-only and whitespace-only tokens. Dependency
  comment as above.
- **CJK must be indexable and searchable.** The shipped chain's `LengthFilter(min=2)` erased
  every CJK unigram; that is the defect. Keep CJK unigrams (single-codepoint CJK tokens are
  meaningful; a length floor measured in characters must not apply to them), or bigram them
  if you can justify it. `東京都` must be findable. Test it.
- Do not carry over the blind `min=2` cut that costs `e-mail` its `e` and `3.14` its `3`.
- `tests/query_types.rs:24,710` assert `SimpleTokenizer` shapes and will change. Per the hard
  constraint above: **escalate, do not edit.** Same for any `tests/suggest.rs` or
  `tests/terms.rs` expectation this moves.

## Phase 4 — word-delimiter splitting and query-side synonyms

- **Word-delimiter splitting** on both sides, so `wifi_router`, `WiFi2000`, and `SKU-42` are
  findable by their parts and by the catenated form. Solr's asymmetric
  `catenateWords`/`catenateNumbers` (index `1`, query `0`) exists to control index size, not
  because it is right — Phase 0 proved it makes `q=wifi_router` find fewer documents than
  `q=wifirouter`, which is backwards from a user's perspective. **Prefer symmetric
  behaviour** unless you can show a concrete reason not to; if you keep an asymmetry, justify
  it in a comment on its own merits, not by citing Solr.
- **Query-side synonyms**, expand-style, case-insensitive. Query-side-only is a genuinely
  sound design (it keeps the index small and lets the table change without reindexing) — keep
  it, and say *that* is why, not "Solr does it".
  On the table: `synonyms_en.txt` is 17,477 single-token pairs and 440 KB. Decide and
  document whether Wayfinder ships it, ships a smaller curated set, or makes it
  configuration. **Ask me if that is not obvious** — shipping 440 KB of WordNet possessives
  (`abettor's, abetter's`) as a default is a product decision, not an implementation detail.
- **Re-derive `last_is_prefix`** (`src/core_index.rs:~4212`). The current comment's
  derivation ("tokens come out in order, so the last one's end IS `maxEndOffset`") breaks once
  a graph filter is in the chain. Derive the new rule from **what autocomplete should do** —
  a user mid-word wants prefix matching, and `foo_` should keep matching `foo_bar` rather
  than returning nothing. Do not reproduce the Solr behaviour table in finding 203. Replace
  the comment entirely; state the rule and what it deliberately does differently.
  Note that with single-token synonym data the synonym filter produces no graph, so
  word-delimiter splitting is the only graph source — and that this is a property of the data,
  not of the filter.
- Delete the `ponytail:` entries in `src/schema.rs` that this phase actually closes, and
  rewrite any whose justification was "Solr does X".
- If any `EXPECTED_DIVERGENCES` entry starts matching, delete it — while the harness still
  runs, a listed entry that starts matching fails the build.

## Non-goals

- The compatibility contract, the findings doc's framing, and the differential harness —
  **another in-flight ticket owns these.** Do not touch them.
- #388 (the global presets' `LengthFilter min=2` and lowercasing) stays separate.
- No new fixture capture; no `capture.sh` changes.
- No `/analysis/field` handler.
- Correcting Solr-derived behaviour that is **not** one of the four features. Escalate
  instead.

## Standing rules for every phase

- Tests before implementation, confirmed red for the right reason.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean.
- The whole suite green at every hand-off. No stage hands off with a red test.
- Expected values are derived from the **stated correct behaviour** and asserted directly.
  `solr-ref/responses/` is reference material now — cite it when you agree with it, and note
  the reason when you deliberately differ.
- Deliberate divergences from the fixtures get a comment saying they are deliberate. An
  unexplained divergence is indistinguishable from a bug.
- Never edit an existing test to accommodate new behaviour. Escalate.
