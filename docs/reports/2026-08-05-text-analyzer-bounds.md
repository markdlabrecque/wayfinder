# Issue #388 - text analyzer bounds implementation

## Approved contract

AMENDMENT 1 corrected the original scope: static `text_en` and `text_general`
align to Solr `_default`, retaining `RemoveLongFilter`'s 40-byte behavior and
switching only to simple lowercase. `_dynamic_text` aligns to
`search_api_solr`: character `LengthFilter` bounds `2..=100` plus simple
lowercase. Analyzer identities and contract are v3:
`wayfinder_text_en_v3`, `wayfinder_text_general_v3`,
`wayfinder_dynamic_text_v3`, and `text_en_solr_length_case_v3`.

The static 40-byte behavior intentionally remains, including its discrepancy
with character-based length semantics; #393 tracks that follow-up.

## Implementation and tests

- Added 12 captured `select_analyzer_*` fixture tests, covering one-character,
  dotted-I/simple-case, and 45/100/101-length behavior.
- Registered `text_general` as a Wayfinder-owned chain; updated static and
  dynamic chains and their migration/contract handling.
- Renamed the `tests/query_types.rs` probe `count_i` to `count_num` so its
  quoted-phrase rewrite coverage is not coupled to analyzer behavior.
- Legacy v1/v2-to-v3 migration permits raw-only adoption through retired
  tokenizer identity aliases. For analyzed `_dynamic_text`, it now scans the
  persisted term dictionary before rewriting the marker and refuses reindexing
  when any term exists.

There is deliberately **no differential manifest wiring**: no changes to
`solr-ref/manifest.tsv`, `manifest-errors.tsv`, or `tests/differential.rs`.

## Mutation evidence

From `HANDOFF-388.md`:

- Changing the dynamic minimum from 2 to 1 was caught by the one-character
  fixture tests.
- Replacing `SimpleLowerCaseFilter` with `LowerCaser` was caught by dotted-I
  fixtures.
- Changing maximum length from 100 to 1000 failed exactly the 101-character
  fixture.
- Initially deleting both retired-identity aliases was not caught. Tests were
  added to POST and commit after adoption; the alias deletion then produced the
  tokenizer-resolution failure. Two unrelated mutation-path tests remained
  green (fresh index and builtin `en_stem`) and were reported as coverage
  limits, not weakened.

## Review history

Review round 2 initially returned **REQUEST CHANGES**: persisted snapshots
could hide historical analyzed `_dynamic_text` postings. Red regression commit
`0ab45c9` proved an actual old one-character term. Production fix `f8cf16f`
scans persisted `_dynamic_text` term dictionaries before a v1/v2-to-v3 marker
rewrite and refuses reindexing if any term exists. Formatting follow-up:
`220ffe1`.

The default review cap was reached. The foreground accepted the resolved
escalation after the regression and production fix; this is the review outcome,
not an unqualified pre-cap approval.

## Verification

Latest full gate: PASS.

```text
cargo test --no-fail-fast                         1567 passed, 0 failed, 1 ignored (74 result groups)
cargo fmt --check                                 PASS
cargo clippy --all-targets -- -D warnings         PASS
```

The pre-work baseline was 1539 tests; rebasing added five upstream tests, making
the current base 1544. The branch remains +23: 22 original #388 tests plus the
review regression test.

## Substantive commits

- `9e4c8d6` test(select): capture the text_en/text_und analyzer bounds from real Solr (#388)
- `8a30891` test(schema): pin text_en/text_general/_dynamic_text length+case bounds (#388)
- `4afdfdc` test(schema): amend #388 static-preset assertions per AMENDMENT 1
- `d3892db` feat(schema): bound and simple-fold the analyzed text chains (#388)
- `ddee1a6` fix(schema): keep retired text tokenizer identities resolvable
- `3be9c65` test(schema): assert legacy _dynamic_text catch-all writes, not just opens
- `0ab45c9` test(schema): reject hidden legacy dynamic postings
- `f8cf16f` fix(schema): reject hidden legacy dynamic postings
- `220ffe1` test(schema): format dynamic postings regression

## Accepted deviations

- Static one-character tokens intentionally survive for `text_en` and
  `text_general`; the test expectation was inverted after AMENDMENT 1 and the
  `_default` fixture evidence. Their 45/100/101 ASCII-character tokens also
  intentionally remain dropped because the retained `RemoveLongFilter` is
  byte-based. This preserves the approved static contract; #393 records the
  character-versus-byte inconsistency.
- The legacy analyzed-dynamic migration test was renamed and inverted to
  require reindexing. This is a fail-closed decision supported by the red
  persisted-posting regression: relabeling a populated legacy analyzed index
  would silently change its analysis semantics.
- Raw-only migration tests received non-analyzed dynamic rules and the helper
  was generalized to reproduce the historical `_dynamic_text` identity. Three
  pre-existing synchronous tests became `tokio::test` solely to POST and commit
  through the adoption path; no dependency was added. These changes make the
  retired-alias guarantee executable rather than open-only.
- The quoted-phrase test probe was renamed from `count_i` to `count_num` so
  dynamic-field rewrite coverage does not depend on analyzer behavior;
  field-name-only cases retain `*_i`.
- No differential manifest was added, by the standing retired-harness
  instruction. The captured responses remain the expected-value source for the
  ordinary integration tests.

## Residual risk and follow-ups

The persisted-term dictionary check is deliberately conservative and
fail-closed: deleted terms can still force reindexing until segments merge.
This is accepted to prevent silently relabeling an index with historical
analyzed postings. The static 40-byte discrepancy remains tracked by #393.
There are no other unresolved review findings or deferred follow-ups in this
report.
