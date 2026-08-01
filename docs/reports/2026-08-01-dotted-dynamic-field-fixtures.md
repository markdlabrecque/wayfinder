# Issue #177 — Solr fixtures for dotted dynamic field names

- Branch: `177-dotted-dynamic-field-fixtures`
- Issue: #177
- Scope: capture and test evidence only; no production-code change

## Behavior captured

A clean one-off `solr:9` container (`wayfinder-solr-177`, port 8999, removed after capture) used the tracer-bullet five-document corpus plus an unstored `tm_X3b_en_*` dynamic-field rule.

Real Solr accepted and queried all four field names:

| Fixture | Field | Result |
|---|---|---|
| `dotted_dynamic_basic.json` | `tm_X3b_en_a.b` | `doc1`, `numFound=1` |
| `dotted_dynamic_leading.json` | `tm_X3b_en_.leading` | `doc2`, `numFound=1` |
| `dotted_dynamic_trailing.json` | `tm_X3b_en_trailing.` | `doc3`, `numFound=1` |
| `dotted_dynamic_consecutive.json` | `tm_X3b_en_a..b` | `doc4`, `numFound=1` |

This confirms issue #164's round-trip choice against Solr wire behavior: leading, trailing, and consecutive dots are accepted rather than rejected or collapsed.

## Changes

- Added four committed Solr response fixtures and core-relative GET rows to `solr-ref/manifest.tsv`.
- Appended the four capture calls to `solr-ref/capture.sh`.
- Seeded the unstored dynamic rule and values in the script's initial schema/corpus, avoiding late overwrites that could change old live-differential ordering, scoring, or deleted-term statistics.
- Mirrored that schema/corpus in `tests/common/mod.rs` for hermetic differential replay.
- Re-derived `/select` assertions in `tests/dotted_dynamic_fields.rs` from the committed fixtures.
- Recorded finding 111 in `docs/solr-ref-findings.md`.

## Verification

Initial red proof:

- `cargo test --test dotted_dynamic_fields dotted_dynamic_field_round_trips_through_select -- --exact` — failed because `dotted_dynamic_basic.json` did not yet exist.

Final gates:

- `bash -n solr-ref/capture.sh` — passed.
- `cargo fmt --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test` — passed.
- Independent review round 1 found and rejected late canonical-document overwrites.
- Independent review round 2 approved the corrected initial-corpus seeding and passed the full gate.
