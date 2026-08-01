# Issue #184: `hl.fl=*` stored-string consistency

## Spec and evidence

A clean one-off `solr:9` capture against the tracer-bullet schema queried
`q=category:animals&hl=true&hl.fl=*`. Solr returned a `category` snippet for both
matching documents, identical to explicit `hl.fl=category`. The committed fixture
is `solr-ref/responses/hl_wildcard_stored_string.json`; finding 110 records the
result and capture provenance.

## Changed behavior

- `hl.fl=*` now includes stored `string`/`keyword` fields, while still excluding
  unstored, numeric, and date fields.
- Raw multi-valued strings fall back to exact stored-value highlighting when
  Tantivy's space-joined raw-token snippet path misses a matching member.
- Wildcard and explicit `hl.fl=category` now match the captured Solr response.

## Verification

The regression test was first confirmed red:

```text
cargo test --test highlighting hl_wildcard_fl_matches_stored_string_fixture_and_explicit_field
FAILED: wildcard returned {"doc1":{},"doc4":{}} instead of category snippets
```

Final gates:

```text
cargo fmt --check                                      PASS
cargo clippy --all-targets -- -D warnings              PASS
cargo test                                             PASS
```

Independent review round 1: **APPROVE**. The reviewer reran the complete chained
gate, confirmed the capture/manifest row is suitable for differential replay, and
found no blocking issue in the raw-string fallback, escaping, snippet cap, or
wildcard field filters.

## Follow-ups

None.
