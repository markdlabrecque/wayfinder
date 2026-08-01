# Issue #138 — `{!key=X}` local-params on `facet.field`

- Branch: `138-facet-local-params-key`
- Worktree: `/Users/mark/Projects/wayfinder-wt/138`
- Commits: `b4cf4b0`, `1a7e47d` (ground-truth fixtures), `9e7f2b1` (implementation,
  `Closes #138`)

## What was built

`search_api_solr` never sends a bare field name to `facet.field` — every captured request wraps
it as `{!key=X}field` — so Wayfinder took the whole prefixed string as a literal field name and
faceted browsing did not work at all against that module. `facet.field={!key=mylabel}category`
now facets on `category` and labels the response bucket `mylabel`.

`split_facet_key` (`src/facet.rs:223`) reuses #137's `local_params::parse_block` rather than
adding a second parser. `extract_nested_queries` is deliberately left untouched — it hard-errors
on the type-less block this shape uses (no `!edismax`/`!lucene` type token, just `{!key=...}`).
The key reaches only the response bucket label; the field name is what drives
`check_facetable`, `resolved_fast_column`, `resolved_value_kind`, the Points-field warning, and
every error message. The key is never resolved as a field.

## Test evidence (re-run for this report, not copied)

- `cargo test` — 653 passed, 34 suites, 0 failed.
- `cargo test --test differential` — 27 passed, 1 suite; no `facet_local_params*` entry present
  in `EXPECTED_DIVERGENCES` (`tests/differential.rs`) — the six new `manifest.tsv` rows genuinely
  match captured Solr, not a documented gap.
- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `git status --short solr-ref/` — empty; the two fixture-capture commits are insertion-only
  (176 + 98 lines, all new files plus append-only edits to `capture.sh`/`manifest.tsv`), the
  implementation commit touches no file under `solr-ref/`.
- Coverage: `select.facet.local-key` probe (`src/coverage.rs:883`, GET
  `facet.field=%7B!key=kind%7Dcategory` asserting `/facet_counts/facet_fields/kind`) now passes.
  45/75 → 46/75, its expected-uncovered entry removed from `tests/search_api_coverage.rs`. The
  ledger is strictly stronger, not just relabeled: `assert_bucket` there ends in exact set
  equality, so the test fails outright if the probe ever regresses, and the denominator (75) is
  unchanged.

## The four open questions — settled by capture, not reasoning

Eight fixtures total, captured against a one-off real `solr:9` in two passes: five before
implementation (`b4cf4b0`), three more mid-pipeline (`1a7e47d`) when stage 1 flagged assertions
with no ground truth behind them. The append-only block in `solr-ref/capture.sh` documents how
they were taken.

- **The key is the response label even when it names a different declared field.**
  `{!key=body}category` returns the *category* counts under the JSON key `"body"`. The key is
  never resolved as a field — settled by `facet_local_params_key_as_other_field.json`.
- **`f.<field>.facet.*` overrides key off the field name, not the local key.**
  `f.category.facet.missing=true` fires; `f.mylabel.facet.missing=true` does nothing. This is
  evidence handed to **#140** (the `f.<field>.*` wildcard-allowlist question), which is why
  `facet_local_params_key_f_field.json` / `facet_local_params_key_f_key.json` deliberately carry
  no `manifest.tsv` row — a row would only buy an `EXPECTED_DIVERGENCES` entry in a file #140 is
  about to touch.
- **No captured module request puts a `{!...}` prefix on `facet.query`, `facet.pivot`, or `fq`.**
  Descoped explicitly rather than generalised to those params speculatively.
- **Error shapes**: an unterminated block is a 400 SyntaxError, not a verbatim field name
  (`facet_local_params_key_unterminated.json`); an empty remainder is `undefined field: ""`
  (`facet_local_params_key_empty_remainder.json`).

**Asymmetry worth flagging**, found by stage 1 and not mentioned in the issue text: un-prefixed
`facet.field=nosuchfield` is 200-with-empty-array in real Solr (a documented, pre-existing
Wayfinder divergence per finding 105/issue #26), but the prefixed case `{!key=k}nosuchfield` is a
real 400 in Solr too. Wayfinder 400s in both cases — so this change moves the prefixed case
*toward* Solr's actual behaviour, and leaves the pre-existing un-prefixed divergence untouched.

## Process findings worth recording honestly

- **A `split_once('}')` + `rsplit_once("key=")` prefix strip survived the entire 648-test suite
  intact before mutation testing caught it.** The implementor found it deliberately, then closed
  it with unit tests beside `split_facet_key` (`src/facet.rs:795` onward). The reviewer verified
  those tests genuinely kill the mutant by extracting `parse_block`/`find_block_end`/
  `split_pairs`/`read_value` verbatim into a scratch harness. The discriminating inputs are
  `cat}egory` and `{!key='a} b'}category` — both exercise `parse_block`'s documented contract
  (a `}` or a quoted `}` inside the value) that a naive `split_once`/`rsplit_once` gets wrong.
  Record why the request-level suite missed it originally: no request-level test sends a `}`
  outside a well-formed block.
- **A docstring in stage 1's suite claimed a test already blocked that strip; it did not.**
  `an_unterminated_block_is_a_400_like_the_fixture` covers a value with no `}` at all, where the
  mutant returns byte-identical output to the correct implementation — it proves nothing about
  the mutant. Corrected in review round 1. Worth recording as a pattern: a comment asserting
  coverage is not coverage.
- **Two message-shape divergences judged inside the existing contract, no PRD entry needed.**
  Wayfinder says `can not facet on undefined field: nosuchfield` where Solr says
  `undefined field: "nosuchfield"`. `normalize_envelope` drops `error.msg` and
  `tests/error_shapes.rs` already treats it as free text, so this is pre-existing tolerance, not
  a new gap. The trailing space on the empty-remainder message is reachable through this feature
  but was already reachable pre-existing on `main` via plain `facet.field=`.
- **The differential harness cannot prove the error-message half of this feature.**
  `facet_local_params_key_unknown` showed 0 diffs in the differential run while an earlier build
  of Wayfinder still leaked the raw `{!key=...}` value into the message, because the harness
  tolerates `error.msg` differences. That is why the message text is pinned directly in
  `tests/facet_local_params_key.rs` rather than relied on via the differential suite. Standing
  limitation, worth naming for future facet/local-params work: green differential output does not
  certify error-message content.
- Review was one round; the bounce was comment/docstring-only (the mislabeled-coverage claim
  above) — no logic changed between rounds.

## Review outcome

One round. Per CLAUDE.md the pipeline's default cap is two rounds, used here as a floor of one
substantive pass rather than the cap being exhausted — the reviewer independently re-ran
`cargo test` (653/34), `cargo test --test differential` (27/27, confirming no
`facet_local_params*` entry appears in `EXPECTED_DIVERGENCES`), `cargo fmt --check`, and
`cargo clippy --all-targets -- -D warnings`, and confirmed `solr-ref/` was untouched by the
implementation commit and insertion-only in the two fixture commits, rather than trusting the
diff's own claims. It also independently extracted the parser functions into a scratch harness to
verify the mutation-testing claim rather than accepting it as stated.

## Follow-ups already filed — link, do not re-file

- **#149** — colliding facet labels collapse into one bucket in an `IndexMap`, last write wins,
  one facet vanishes from the response silently. Pre-existing on `facet.query`
  (`src/facet.rs:178`); this change only makes the same collapse reachable on `facet.field` too.
  Needs a capture first: Solr's `NamedList` can emit duplicate JSON keys, which an `IndexMap`
  structurally cannot hold, so the fix shape is not obvious without seeing what Solr actually does.
- **#150** — two coverage gaps: (1) no test distinguishes `resolved_value_kind(field)` from
  `resolved_value_kind(label)` because the only two fields exercised, `body` and `category`, are
  both `Text` — needs a numeric fast field added to the shared schema to be testable; (2)
  `{!key=a key=b}` (a repeated `key` local param) is assumed to take the first occurrence in
  Wayfinder's parser, where Solr likely takes the last — explicitly unverified, capture before
  acting.

Both ceilings are named in `ponytail:` comments directly on `split_facet_key`
(`src/facet.rs:209`), pointing at #149 and #150 respectively.

**One assertion in the diff is derived from reasoning, not a fixture**:
`a_block_without_a_key_labels_with_the_field_name` — `{!ex=tagname}category` (a local param other
than `key`) labels the bucket `category`, matching Solr's documented key-defaults-to-remainder
rule. Its docstring says explicitly that it is unfixtured (`src/facet.rs:847`). Flag this as the
entry to re-check if #140's work ends up capturing `tag`/`ex` local-param shapes — if that capture
contradicts the reasoned assumption, this test is the one to revisit.

## Bottom line

`{!key=X}` on `facet.field` lands with the field driving all schema/error-message behaviour and
the key relabeling only the response bucket, all four of the issue's open questions settled by
real `solr:9` capture rather than assumption, one review round with a genuine mutation-testing
catch (a prefix-strip bug that passed the full pre-existing suite) closed with targeted unit
tests, and two ceilings named and filed (#149, #150) rather than silently accepted. All local
gates green; `solr-ref/` untouched by the implementation. Coverage 45/75 → 46/75, ledger strictly
stronger.
