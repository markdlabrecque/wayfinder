# Report: numeric/date `facet.field` enumeration — the premise was false (issue #24)

- Branch: `24-numeric-facet-enumeration` (worktree)
- Issue: #24, v1 milestone. Filed on the strength of Tantivy's source
  (`term_agg.rs`'s zero-fill runs only for `ColumnType::Str`) and assumed Solr fills numeric/date
  facet buckets at 0 the way it does string ones. Read first: `CLAUDE.md`, `docs/PRD.md` §2 and
  §5, `docs/reports/2026-07-28-faceting-aggregation.md`.
- Three commits: `83d9354` (fixtures), `a59d912` (the fix, including round-1 corrections),
  `7a7809e` (round-2 fixes: warnings key order, findings renumbering).

## The headline: the ticket premise was false, and the fixtures falsified it

Solr 9's `pint`/`pdate` are Points-based fields with **no indexed term dictionary** to walk, so
Solr does not enumerate zero-count numeric/date buckets either. Wayfinder's existing hit-set-only
behaviour was already a match.

Evidence, fixture by fixture (all present in `solr-ref/responses/`, verified by reading them
directly):

- `facet_field_numeric_subset.json` — `q=id:r1` (one hit) against `views` (values 5/15/25/35
  across the 4-doc corpus) returns `"views":["5",1]` only. No `15`/`25`/`35` at count 0.
- `facet_field_date_subset.json` — same shape for `created`.
- `facet_field_string_control_subset.json` — **the control, and the reason the above is
  trustworthy.** Same container, same `facets` core, same 4-doc corpus, same `q=id:r1`, same
  params — the only dimension that differs is `facet.field=id` (a string column) instead of
  `views`. It *does* enumerate: `["r1",1,"r2",0,"r3",0,"r4",0]`. The behaviour is field-type
  driven, not an artifact of the capture setup. I re-derived this independently from the fixture
  files rather than trusting the summary handed to me, and it checks out exactly as described.
- Solr says why itself, in `responseHeader.warnings` on `facet_field_numeric_all.json` et al.:
  `"Raising facet.mincount from 0 to 1, because field views is Points-based."`

### 1. What became of the original divergence?

It never existed. Wayfinder's hit-set-only numeric/date faceting was already correct — Tantivy's
`ColumnType::Str`-only zero-fill matches Solr's own inability to enumerate a Points-based column,
rather than being a gap relative to it. This is **not** "emits a warning instead of matching
counts": the counts already matched before this issue touched anything; the warning is an
additional envelope element on top of already-correct counts, not a substitute for correctness.

Nothing was added to `docs/PRD.md` §2's ratified-divergences subsection, and nothing was escalated
to the user, because there is no divergence to ratify. Confirmed directly in the diff: the new
PRD entry (`docs/PRD.md`, item 11 under "Verified envelope facts", §2) explicitly closes with "Not
a divergence — see findings 27-30," and the `#### Ratified divergences` subsection is untouched by
this branch.

That subsection's own scope note is directly on point here: its divergence #2 records that issue
#26 had to narrow a ratified divergence that turned out to rest on a fixture captured against a
container whose schema had been polluted by an earlier schemaless probe — "a ratified divergence
is only as good as the cleanliness of the fixture behind it." This issue is why the string-control
fixture was captured before any conclusion was drawn: one dimension changed (field type), same
container/core/corpus/hit-set, so the conclusion doesn't depend on trusting the capture setup was
clean elsewhere.

### 2. Is `responseHeader.warnings` a captured fact?

Yes, and both sides of the gate are captured, not inferred:

- Nine of the eleven new fixtures carry the warning (every `facet.field=views`/`created`
  capture at the default/implicit `facet.mincount=0`).
- `facet_field_numeric_mincount_one.json` (`facet.mincount=1` given explicitly) does **not**
  carry it — verified by reading the fixture directly, no `warnings` key present.
- `facet_field_string_control_subset.json` (string column) does **not** carry it either.
- No pre-existing `facet_range_*` fixture carries it (the raise is specific to `facet.field`).

So the trigger — a `facet.field` naming a numeric/date column with an *effective*
`facet.mincount` of 0 — is captured on both the positive and negative side, not guessed at.

What is genuinely inferred, not captured: that an explicit `facet.mincount=0` behaves like the
omitted default (only the omitted-default case was captured), and the ordering of multiple
warning strings when several qualifying fields are requested together (only single-field
`facet.field` requests were captured). Solr's wording, including the literal string
"Points-based", is reproduced verbatim from the fixture — the code comment in `src/facet.rs`
says as much, naming the fixture it is copied from.

## What was actually built

Two real, fixture-backed changes (see `git diff origin/main...HEAD` for the full patch; not
restated here beyond what's needed to record outcomes):

- **Numeric/date facet terms now order by value, not by the lexical form of the rendered key.**
  `src/facet.rs` previously sorted on the rendered `String` term, giving `"15","25","35","5"`
  for `facet.sort=index`; Solr gives `5,15,25,35` (`facet_field_numeric_all.json`,
  `facet_field_numeric_sort_index_all.json` — these two were the genuinely red tests). Fixed at
  the root: `CoreIndex::term_facet` now returns a typed `FacetOrderKey` alongside the rendered
  term (`src/core_index.rs`), carrying an `i64`/`u64`/`f64`/`Str` variant so the comparison
  happens before the value collapses to a string. A date bucket's order key is reconstructed by
  parsing the rendered RFC3339 term back into an `OffsetDateTime` (seconds + fractional-second
  remainder), because a date bucket's Tantivy key is already `Key::Str(rfc3339)`
  (`term_agg.rs:1054-1060`), not an f64-millis histogram key — see round 1 below.
- **`responseHeader.warnings`**, emitted to match Solr's mincount-raise message, inserted
  **first** in `responseHeader` — `warnings, status, QTime, params` — per
  `facet_field_numeric_all.json` (`src/lib.rs`).

## Fixture capture: process notes worth recording

- Eleven fixtures captured from a real `solr:9`. Docker **was** available, but issue #25 was
  capturing concurrently against `wayfinder-solr-ref` on port 8983, and `capture.sh`'s top rebuilds
  that container destructively — so the canonical script's existing container was **not** reused
  for this work. Verified in the diff: the appended block in `solr-ref/capture.sh` stands up its
  own container (`wayfinder-solr-24`, port 8985), its own core (`facets`), schema, and corpus,
  following the `wayfinder-solr-ref-strict` precedent already in the script. The block's comment
  says explicitly it is not runnable standalone (it depends on `$OUT`/`$HERE` from the top of the
  script and `capf` appends to `manifest-errors.tsv` unconditionally) — the whole script must be
  run, and I take that at face value rather than having re-run it myself (my instructions say no
  Docker, no network, no `capture.sh`).
- The corpus and schema in that block are byte-identical to the issue-#3 `facets` block already in
  `capture.sh` (same four docs `r1..r4`, same `views`/`created`/`note` field defs) — confirmed by
  reading both blocks side by side in the diff.
- These are `facets`-core GETs, so all eleven rows landed in `manifest-errors.tsv`
  (confirmed: `git diff` shows exactly 11 new lines there, base URL `http://localhost:8985/solr`
  as the 6th column), not `manifest.tsv` — the same precedent `facet_range_*` set. No
  `manifest.tsv` row was added, so the differential harness (`cargo test --test differential`,
  18 tests, unaffected by this branch) is untouched, and no `EXPECTED_DIVERGENCES` entry was added
  or removed.
- **Findings claimed: 27-30** in `docs/solr-ref-findings.md` — confirmed by reading the file: the
  new section header reads "Claiming findings 27-30 (issue #25 landed concurrently and took 21-26
  above)." This branch originally numbered from 21-24 and had to renumber after #25 landed on
  `main` first — worth recording as a repeat of the same numbering collision issues #2 and #3 hit
  earlier (both numbered from 16 concurrently). The falsified "known Wayfinder divergence"
  paragraph that issue #3 had left under "Not yet captured" in `docs/solr-ref-findings.md` was
  removed in this diff.

## Review: two rounds, both real

- **Round 1 bounced**, with two must-fix items:
  - The date sort-key doc comment described a Tantivy mechanism that does not exist — it claimed
    an f64-millis histogram key, when a date terms-bucket key is actually `Key::Str(rfc3339)` via
    `format_date` (`term_agg.rs:1054-1060`), and `key_as_string` is populated only for `Bool`
    (`intermediate_agg_result.rs:728-734`). Corrected in `a59d912` to parse the rendered RFC3339
    term back into an instant instead.
  - A `ponytail:` comment naming a still-live ceiling had been deleted; restored.
  - Two 5-minute items: a comment claiming an unreachable code path that is in fact reachable for
    `f64` columns via `NumericalValue::normalize`; and an overclaim that the `capture.sh` block
    for this issue runs standalone (both corrected in `a59d912`, and the standalone-runnability
    overclaim is explicitly disclaimed in the current comment, confirmed by reading it above).
- **Round 2 approved**, with three follow-ups (recorded below). This is the pipeline's 2-round
  cap: per its own rule, the work has now used its full allotment, and **this work could use more
  review passes** if anything else surfaces later — there is no built-in headroom for a third
  round without escalating to the orchestrator.

## Mutation testing (per CLAUDE.md)

- **The ordering fix**: reverting `FacetOrderKey` back to a lexical `String` comparison is caught
  by `facet_field_numeric_all_matches_fixture` and `facet_field_numeric_sort_index_all_matches_fixture`.
- **The warnings gate**: making the warning unconditional (always emitted regardless of column
  type / mincount) fails 19 of the 54 tests in `tests/faceting.rs`.
- **The one mutation result that most needs its own callout, as concrete evidence for the
  fixture-diff harness limitation `docs/PRD.md` §8 already records.** The reviewer moved the
  `warnings` insertion in `src/lib.rs` back to last in `responseHeader` on a scratch copy. That
  mutation was caught by `tests/json_key_order.rs::response_header_warnings_leads_not_trails`
  (confirmed present at line 438 in the current file — one line off the round-2 handoff's `:210`,
  consistent with the file having grown since). But the **same mutated build**, run against
  `--test faceting`, reported **54 passed, 0 failed** — the entire fixture-diff suite is blind to
  a key-order regression, because `serde_json`'s comparison of parsed `Value`s (backed by
  `IndexMap` under the `preserve_order` feature) is order-insensitive. The
  `response_header_warnings_leads_not_trails` test is the *only* guard against this regression.
  Straight about its provenance: it was written alongside the fix, not committed red ahead of it,
  and the mutation result above is what establishes that it is a real guard rather than a
  decorative assertion.
- The reviewer additionally verified the date sort key stays monotonic across the epoch boundary:
  `unix_timestamp() as f64 + nanosecond() as f64 / 1e9` is correct because `nanosecond()` is the
  positive remainder above the civil-second floor and `unix_timestamp()` floors to match
  (`time-0.3.54/src/offset_date_time.rs:530-538`), so `1969-12-31T23:59:59.5Z` yields `-0.5`, not
  `-1.5`. A parse failure on the reconstructed instant cannot take a query down: the fallback path
  is `Err(_) => FacetOrderKey::Str(term)`.

## Test evidence

Verified independently by this reporter, not copied from the handoff.

`cargo test` — full run, by suite:

```
     Running unittests src/lib.rs        -> 8 passed; 0 failed
     Running unittests src/main.rs       -> 0 passed; 0 failed
     Running tests/differential.rs       -> 18 passed; 0 failed
     Running tests/error_shapes.rs       -> 12 passed; 0 failed
     Running tests/faceting.rs           -> 54 passed; 0 failed
     Running tests/json_key_order.rs     -> 14 passed; 0 failed
     Running tests/schema_layer.rs       -> 25 passed; 0 failed
     Running tests/server_config.rs      -> 18 passed; 0 failed
     Running tests/sort.rs               -> 25 passed; 0 failed
     Running tests/tracer_bullet.rs      -> 12 passed; 0 failed
   Doc-tests wayfinder                   -> 0 passed; 0 failed
```

Total: **186 passed, 0 failed** across 11 suites (8+18+12+54+14+25+18+25+12 = 186, plus the two
empty suites). This matches the number given in the handoff exactly.

`cargo fmt --check` — clean, exit 0, no output.

`cargo clippy --all-targets -- -D warnings` — CI's exact command. Output:

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.11s
```

Clean, zero warnings, exit 0.

Both gates match the handoff's expectation exactly; nothing to flag as a discrepancy there.

## Follow-ups to record

1. **Date sort-key precision.** Use `unix_timestamp_nanos()` rather than f64 seconds for an exact
   date sort key. The comment in `src/core_index.rs` currently claims the fix "removes the
   dependency on precision entirely" — that overclaim should be softened: at a 2026-era epoch
   second count, an f64 resolves to roughly 200ns, so two sufficiently close instants can still
   tie and fall back to Tantivy's own bucket order. Unreachable today because
   `DateOptions::default()` is `DateTimePrecision::Seconds`; already carries a `ponytail:` in
   code.
2. **Not recorded anywhere else, so it lands here.** Hoisting `facet_fields` into a `let` in
   `src/lib.rs::select` (needed so the warnings vector is known before `responseHeader` is built)
   changed error precedence: `facet_fields` is now evaluated before `facet_queries`, so a request
   carrying *both* an invalid `facet.query` and an unfacetable `facet.field` now reports a
   different error message than before the change — same HTTP status, same
   `wayfinder::FacetError` error class, different text. Unobserved by any current test or fixture:
   no fixture or test covers a double-error request, and Solr's own precedence for this case is
   uncaptured. Needs one captured fixture to settle Solr's order, then a regression test. This
   resonates with `docs/PRD.md` §8's warning that the differential harness normalises `error.msg`
   away entirely — a message-level change like this diffs to zero under that harness and would
   only be caught by a dedicated fixture/test, which does not yet exist.
3. **Float/double and sub-second date rendering are still open.** Capture `facet.field` on a
   `pfloat`/`pdouble` column to settle `"5"` vs `"5.0"` — Tantivy normalizes `F64` to `U64`/`I64`
   where possible, so Wayfinder currently emits `"5"` where Solr's `pdouble` would emit `"5.0"` —
   and capture a millisecond-precision date facet to pin date term rendering and ordering beyond
   what the seconds-precision corpus here can distinguish. Both already carry a `ponytail:` in
   `src/core_index.rs`.

## Review depth statement

Round 2 was the second and final round permitted by the pipeline's 2-round cap. It closed as an
approval with three follow-ups, not a bounce, but the work has now used its full allotment of
review passes — **this work could use more review passes** if anything else surfaces later,
since there is no built-in headroom left for a third round without escalating to the orchestrator.

## Pointers

- Order-key fix: `src/core_index.rs` (`CoreIndex::term_facet` return type, `FacetOrderKey`)
- Warnings + gate: `src/facet.rs` (`facet_counts`, `facet_fields`)
- Response envelope key ordering: `src/lib.rs` (`select`, `responseHeader` construction)
- Tests: `tests/faceting.rs` (new section 14, issue #24), `tests/json_key_order.rs`
  (`response_header_warnings_leads_not_trails`)
- New fixtures: `solr-ref/responses/facet_field_{numeric,date,string_control}_*.json` (11 files),
  captured against a self-contained container/core appended to `solr-ref/capture.sh`
- Capture rows: `solr-ref/manifest-errors.tsv` (11 new rows, `facets`-core GETs, base URL
  `http://localhost:8985/solr`)
- Findings: `docs/solr-ref-findings.md`, "Findings from the issue #24 `facet.field` numeric/date
  capture" (27-30); removed the falsified "known Wayfinder divergence" paragraph issue #3 had
  left under "Not yet captured"
- Envelope fact: `docs/PRD.md` §2, "Verified envelope facts" item 11 (not the ratified-divergences
  subsection — there is nothing to ratify)
