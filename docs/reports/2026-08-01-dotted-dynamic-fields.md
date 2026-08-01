# Issue #164 — dotted dynamic field names never matched what was indexed

- Branch: `164-dotted-dynamic-fields`
- Worktree: `/Users/mark/Projects/wayfinder-164`
- PR: #180, `Closes #164`
- Commits: `20b4924` (fix), `ba09583` (round-2 review follow-ups)

## The bug

A dynamic field name containing a `.` (e.g. `tm_X3b_en_a.b`, matched by the shipped
`tm_X3b_en_*` glob in `presets/search-api.toml`) resolved as a valid field on both `/select`
and `/terms`, but never matched anything indexed under it: `select?q=tm_X3b_en_a.b:gamma`
returned `numFound: 0`, and `terms?terms.fl=tm_X3b_en_a.b` returned an empty list even
immediately after indexing a document with that exact key.

Root cause, confirmed by reading the tantivy source rather than inferred from the ticket: the
read and write paths for a dynamic field's JSON path disagreed on how many segments a dotted
name is.

- **Read**: `CoreIndex::term_for_target` builds the term with
  `Term::from_field_json_path(container, path, expand_dots)`. Tantivy's
  `Term::from_field_json_path` (`tantivy-0.26.1/src/schema/term.rs:78`) unconditionally calls
  `split_json_path`, which splits on every unescaped `.` **regardless of the `expand_dots`
  argument** — so `"tm_X3b_en_a.b"` always becomes two segments, `["tm_X3b_en_a", "b"]`, on the
  read side no matter how the schema is configured.
- **Write**: indexing walks the JSON object key-by-key and calls
  `JsonPathWriter::push(segment)` once per already-distinct JSON key. For the catch-all
  container, the whole dynamic field name is one key, pushed in one call.
  `JsonPathWriter::push` (`tantivy-common-0.11.0/src/json_path_writer.rs:53`) only replaces `.`
  with the `0x01` segment separator *inside that single push* when `expand_dots` is enabled.
  Pre-fix, the catch-all `JsonObjectOptions` never enabled it, so the write side produced one
  segment containing a literal `.` byte.

Written and queried encodings never matched — dotted dynamic fields were 100% non-functional,
regardless of glob or field-type.

## The fix and why this side

One line, in `schema::parse`'s catch-all-container construction site
(`src/schema.rs`): `.set_expand_dots_enabled()` added to the catch-all `JsonObjectOptions`.
With it, `JsonPathWriter::push` does the same byte-for-byte `.` -> `\x01` swap on the write
side that the read side already performs unconditionally, so the two sides produce identical
bytes.

The maintainer's call was to fix the **write** path rather than the read path — Solr treats
dots in field names as ordinary characters, so round-trip is the compatible direction, not
rejection. Fixing the read path instead (stopping it from splitting on `.`) was considered and
rejected, since `Term::from_field_json_path`'s unconditional split is tantivy's own general
JSON-path behaviour, not something specific to this container.

Consequence, named explicitly rather than hidden: this changes the on-disk encoding of dotted
dynamic field names, so **an existing index holding documents under a dotted dynamic name needs
a reindex** to benefit. Non-dotted names (the overwhelming majority of fields) are unaffected —
`push` on a dot-free segment is a no-op either way.

## The edge-case derivation

`a..b`, `.leading`, and `trailing.` all round-trip because the write and read encodings
coincide byte-for-byte once both sides expand dots, verified against
`tantivy-common-0.11.0/src/json_path_writer.rs` and `tantivy-0.26.1/src/core/json_utils.rs`
directly, not inferred from the option's name:

| Input | Write-side encoding (`JsonPathWriter::push`, one call, in-place `.`→`\x01` swap) | Read-side encoding (`split_json_path` + per-segment pushes, unconditional) |
|---|---|---|
| `a..b` | `push("a..b")` swaps both dots in place → `a\x01\x01b` | `split_json_path("a..b")` → `["a", "", "b"]`; `JsonPathWriter` inserts a separator between each push regardless of segment content → `a\x01\x01b` |
| `.leading` | `push(".leading")` → `\x01leading` | `split_json_path(".leading")` → `["", "leading"]` → `\x01leading` |
| `trailing.` | `push("trailing.")` → `trailing\x01` | `split_json_path("trailing.")` → `["trailing", ""]` → `trailing\x01` |

Both the implementor and the reviewer traced this independently from source. The fix needs no
special-casing for these cases: once both sides expand dots the same way, leading/trailing/
consecutive dots round-trip through an empty-named segment rather than erroring or silently
dropping data — the surprising case a naive or partial fix (e.g. one that tries to reject or
collapse empty segments) would get wrong. `tests/dotted_dynamic_fields.rs` pins this
explicitly.

One consequence noted but not filed as its own issue: with `expand_dots` on, an escaped dot
(`a\.b`) now encodes identically to an unescaped one, so escaping is no longer meaningful. The
reviewer judged this a non-loss — both spellings now match the write encoding, whereas pre-fix
only the escaped spelling matched anything at all — and no fixture or `search_api_solr` naming
scheme produces a raw dot in that position.

## The ceiling (`ponytail:` comment at the call site)

An existing index reopens via `CoreIndex::open`'s `create_in_dir(...).or_else(Index::open_in_dir)`
pair, which reads back the schema **persisted in that index's own `meta.json`** — where
`expand_dots` is still `false` for an index created before this fix. `term_for_target` reads
the `expand_dots` flag off `self.index.schema()` (the opened schema), and the writer is built
from that same `index`, not from `wf_schema.tantivy_schema` (the freshly-parsed schema with the
new flag on). So on a pre-existing index, both the read and write sides agree with each other
— just on the old, broken encoding. The fix is therefore **inert** on such an index, not
corrupting: it keeps the old broken (non-matching) behaviour until a reindex into a fresh data
directory. There is no migration and no detection that an opened index predates the fix.

This comment's first version stated the wrong mechanism — it blamed `check_compatible`
comparing schema TOML, implying an existing index might get silently adopted mis-encoded. The
real mechanism (the persisted-schema reopen described above) is safer than that first
description claimed: no mixed-encoding or partially-corrupt state is possible, only the
inertness above. Round 2 corrected the comment text.

## Process, candidly

Stage 1 (test-writer) reported a test failing "pre-existing" in its worktree, for a test that
does not exist anywhere on this branch — it belongs to issue #168. This is the **second
fabricated finding of this batch** (the first was a fabricated "finding 93" on #155). Because
of it, both the implementor and the reviewer were explicitly instructed to re-derive every
stage-1 claim from source rather than trust it. Both did so independently (see the source
citations above: `term.rs:78`, `json_path_writer.rs:53`, `json_utils.rs::index_json_object`),
and both found the substantive root-cause and edge-case claims sound — the fabrication was
isolated to that one false test-failure report, not the technical analysis. The cost was two
rounds of re-derivation; the mitigation that worked was naming the specific suspect claim
explicitly in the downstream prompts rather than asking for a generic re-check.

## Review outcome

Approved round 1, no must-fix. The reviewer went beyond the implementor's blast-radius check
and independently traced the columnar/fast-field path (`encode_column_name` and
`FastFieldWriter` use the same push ordering as the JSON-document path, so the fix's effect is
consistent there too) and the stored-document rendering path (`render_doc` reads original JSON
keys off the stored document rather than the indexed term encoding, so `fl` output is
untouched by this change).

Two five-minute items came back out of round 1 and were closed in round 2 (`ba09583`):

1. `distinct_dotted_dynamic_names_do_not_collide` — pins that two distinct dotted names in the
   same core (`a.b` encoding to `a\x01b`, `a..b` encoding to `a\x01\x01b`) each return only
   their own document and neither matches the other's token. Not a suspected bug, but the
   collision class a reader will ask about given tantivy's own documentation warning that
   `expand_dots` "can lead to ambiguity."
2. The `ponytail:` comment's mechanism correction described above.

Stale `src/schema.rs:705`-style line-number citations in the test module doc were replaced
with symbol references (`schema::parse`'s `catch_all_fields` loop, `CoreIndex::term_for_target`,
etc.) rather than re-pinned line numbers, since they had already gone stale once during this
same review.

## Evidence

Re-run for this report on the current HEAD (`ba09583`), not copied from an earlier commit's
run:

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test` — 738 passed, 39 suites, 0 failed.
- Coverage: 57/75, endpoints 9/9 (unchanged by this fix — it corrects existing-endpoint
  behaviour, adds no new endpoint surface).
- Mutation test: reverting the one line (`.set_expand_dots_enabled()`) made exactly the three
  dotted-round-trip tests in `tests/dotted_dynamic_fields.rs` fail, and no others — confirming
  those tests, and only those tests, depend on the fix.

Per CLAUDE.md's default two-round cap for the reviewer stage: this review closed in round 1
substantively (round 2 was two five-minute polish items, not new findings), so the cap was not
exhausted and there is no standing need flagged for further passes on this specific diff.

## Follow-ups filed, not yet actioned

- **#176** — no fast/columnar path coverage for a dotted dynamic name. The `expand_dots` flag
  changes column-key encoding too (`encode_column_name` / `FastFieldWriter`); the match with
  the JSON-document path is derived from reading the source, not exercised by a test.
- **#177** — no Solr fixture backs the dotted-dynamic-field behaviour at all. The edge cases
  (`a..b`, `.leading`, `trailing.`) are exactly where an unfixtured assumption should be
  confirmed against real `solr:9`, per this repo's compatibility contract.
- **#178** — `dotted_dynamic_field_edge_cases_round_trip` (or its equivalent in
  `tests/dotted_dynamic_fields.rs`) checks `/select` only, not `/terms`, even though the bug
  report and root-cause analysis cover both endpoints.

All three are open, filed by the reviewer, and linked here rather than retyped.

## Bottom line

A dynamic field name with a dot was completely non-functional because the write path encoded
it as one JSON-path segment while the read path — unconditionally, regardless of any flag —
split it into two; the fix is a single `.set_expand_dots_enabled()` call on the catch-all
container, chosen over a read-path fix because Solr treats dots as ordinary field-name
characters. The fix is inert (not corrupting) on any index created before it landed, and that
is now correctly documented at the call site after a round-2 correction. All local gates are
green (738/39, fmt and clippy clean) and the one-line fix is mutation-confirmed. Three
follow-ups are open (#176 fast/columnar coverage, #177 no Solr fixture, #178 `/terms` coverage
gap) and one process risk is on record: this is the second fabricated stage-1 finding in this
batch, caught only because both downstream stages were told to re-derive rather than trust it.
