# Issue #198 — repo-wide `finding N` citation sweep

**Branch:** `198-finding-citation-sweep`
**Status:** implemented, reviewed (2 rounds, approved), ready to merge.

## What the issue asked, and what was actually true

Issue #198 reported that `tests/mlt.rs` had a systematic drift where finding citations were
"consistently nine low", and asked for a repo-wide sweep on the grounds that the drift was
"unlikely to be confined to one file". Both halves held up, but the shape was worse than the
ticket's model:

- The `-9` band is real and is confined to the MLT citations (`tests/mlt.rs`, `src/lib.rs`'s
  `/mlt` block). Every number the issue listed was correct.
- It is **not** one drift. Five independent bands exist, because each came from a different
  renumbering event: `-14` (query-types 42-45 -> 56-59), `-9` (MLT 51-58 -> 60-67), `-7`
  (facet-error 43 -> 50), `-4` (`mm` 85 -> 89), `-3` (`qf`-under-`*:*` 85 -> 88), `-2`
  (`mlt.match.offset` 97 -> 99), and `-1` (the whole highlighting block, 51-54 -> 52-55).
- **53 citation instances pointed at findings that do not exist at all.** The numbers 32, 33,
  43, 44, 45, 85 and 86 were never committed to `docs/solr-ref-findings.md` in any state. This
  is the worst class in the issue's own terms — a reader following a citation to 45 lands
  nowhere.

### Root cause, confirmed in the history

`7a7809e` ("fix(select): lead responseHeader with warnings, and renumber findings") says it
outright: *"renumber this branch's findings from 21-24 to 27-30 ... since #25 landed on main
first and claimed 21-26."* Concurrent branches each claim a number range in a hot file, a
collision forces a renumber at merge time, and nobody sweeps the citations that were written
against the pre-merge numbering. The vacated numbers come from the same mechanism in reverse:
`git show 3981f62:docs/solr-ref-findings.md` (#114) jumps `84.` straight to `87.`, having
reserved 85/86 and then not used them.

This is a direct instance of the hazard `CLAUDE.md` already names for `src/lib.rs`,
`tests/common/mod.rs` and `capture.sh` — `docs/solr-ref-findings.md`'s *numbering* is a hot
shared resource in exactly the same way, and it has no merge conflict to warn you.

## What changed

122 citation corrections across 19 files, comment/prose only — no production logic, no test
assertions, no fixtures touched.

| Area | Sites fixed |
|---|---|
| `src/` (`core_index`, `highlight`, `lib`, `query`, `error`, `schema`, `coverage`) | 50 |
| `tests/` (`mlt`, `query_types`, `edismax`, `highlighting`, `common/diff`) | 40 |
| `docs/reports/*.md` | 24 |
| `docs/solr-ref-findings.md` (internal cross-references) | 7 |
| `solr-ref/capture.sh` | 1 |

Three judgment calls worth naming, since none is a mechanical renumber:

1. **`tests/mlt.rs:297` now reads "finding 64/65"**, as the issue suspected it should. The
   comment makes two claims and 64 only supports one: the `mlt.mintf`/`mlt.mindf` loosening is
   64, but the specific count it asserts (4 matches for `mlt11`) appears only in 65.

2. **Two citations had no correct target rather than a wrong one**, and were reworded to stop
   pointing at a finding that does not back them. `docs/solr-ref-findings.md:523` cited finding
   21 for where `warnings` sits in `responseHeader`, but 21's table is the no-warnings case
   (`status, QTime, params`) — the ordering comes from this finding's own fixture. And the `pf`
   unknown-field-leniency sentence (`:1400`, plus the same citation in `src/core_index.rs`)
   cited finding 8 — "unknown request *parameters* are silently ignored" — for a fact about
   field names inside `pf` that no fixture captures at all. It is a Wayfinder choice, and now
   says so.

3. **`docs/reports/*.md` were included.** They are dated records, but a wrong citation misleads
   a reader there exactly as much as in a comment. Where a report narrated *adding* a finding
   ("the reporter added the finding as 85"), the number was corrected and a parenthetical
   records what really happened, so the history is not falsified.

While fixing the `finding 8` citation in `src/core_index.rs:1176`, its neighbouring sentence
turned out to be false as well: it claimed an unknown field name in `qf` "is silently dropped
rather than erroring", which #111 and #112 made a 400. Corrected in the same comment.

## The guard: `tests/finding_citations.rs`

Two assertions, both mutation-tested:

- `every_citation_resolves_to_a_real_finding` — scans `src/`, `tests/`, `docs/`, `capture.sh`
  and `CLAUDE.md` for citations and fails on any that names a finding the doc does not contain.
  Planting a citation to a number the doc does not contain in `src/params.rs` fails it;
  reverted.
- `findings_are_numbered_uniquely` — pins the doc's duplicate numbers at exactly {16, 17, 18}.
  Renumbering `101.` to `100.` fails it; reverted.

The parser is hand-rolled (no `regex` dev-dependency, and adding one for this is not worth it).
It flattens newlines first, because citations wrap across comment lines, and handles the forms
actually in use: `finding 12`, `findings 27-30` (expanded), `finding 36/37`, `findings 90, 91
and 92`, en dashes, `finding #93`, `finding-54`, `finding(90)`. The reviewer verified it is not
under-matching — 828 extractions against an independent regex sweep of 580 citation starts, with
negative controls (`docs/solr-ref-findings.md`, `Findings from the issue #3`, `## Finding from
issue #110`) correctly extracting nothing, so issue numbers are not read as finding numbers.

**What the guard deliberately does not do**, disclosed in its own `ponytail:` comment: it checks
existence, not support. It would not have caught the drift that motivated it — every number in
the `tests/mlt.rs` 51-58 band resolves to a real finding, just the wrong one. Whether finding 63
supports the sentence citing it stays a human review job.

`INTENTIONAL_VACANT_REFERENCES` allowlists two references in
`docs/reports/2026-07-28-harness-debt.md` that are deliberately *about* 32/33 not existing —
a true sentence that would otherwise read as two dangling citations.

## Review outcome

Two rounds, independent reviewer (Opus), approved in round 2. The prime suspect named in the
brief was that the ~120 edits came from four parallel audit subagents and were applied without
being independently re-derived. That was the right suspicion to plant — round 1 bounced two
must-fix items:

1. `tests/highlighting.rs:395`, a **line-wrapped** citation the sweep missed (`finding` ending
   one comment line, `51's` starting the next). Its identical twin in `src/highlight.rs:199` was
   caught and corrected to 52. Worth noting the new guard cannot catch this class: 51 exists.
2. My own parenthetical in the two report files claimed #198 renumbered 85 to 88/89. The
   pickaxe disproves it — `git log -S "88. **"` -> `59bda91`, `-S "89. **"` -> `d05ad3c`,
   `-S "85. **"` -> nothing. Each finding landed at its final number in its own merge commit;
   the reports had cited an in-flight number that was never committed. Round 2 narrowed it
   further: #111 never went near 85/86 and #114 skipped them, so only #112's and #113's own
   reports ever said 85.

No false positives: every changed number was either vacated or pointed at a demonstrably
unrelated finding (42 = `maxScore` key position, 51 = `stats.field` floats, 97 =
`f.<field>.facet.missing`). The reviewer re-derived all 122 topic-by-topic.

## Gates

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — no issues found.
- `cargo test` — 851 passed (44 suites).

## Follow-ups

1. **The doc's duplicate 16/17/18 (filed separately).** Each number is used twice — once by
   issue #3's faceting capture, once by issue #2's sort capture — so a citation to one is
   ambiguous about which it means. Six citations currently rely on context to disambiguate.
   Deliberately **not** fixed here: renumbering would invalidate citations that resolve
   correctly today, which is a bigger change than this comment-only issue. `findings_are_
   numbered_uniquely` pins the set, so the fix deletes its own expectation when it lands.
2. **A support-checking guard, not just an existence one.** Cross-check a file's citations
   against the "Claiming findings X-Y" range of the section its fixtures belong to — that
   *would* have caught all five drift bands mechanically.
3. **Prevention beats sweeping.** The real fix is for the numbering to stop being a contended
   resource: either a branch appends at `max+1` at merge time rather than claiming a range up
   front, or findings get stable slugs and the numbers become presentation. Worth a decision
   before the next parallel batch, since this is the second time the numbering has cost real
   work.
4. `docs/reports/` historical-attribution prose is the one class of claim in this diff that git
   history can contradict and no test guards. The reviewer flagged it as deserving another pass;
   both instances found were corrected, but the class is unguarded by construction.
