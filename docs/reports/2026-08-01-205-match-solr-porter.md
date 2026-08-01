# Issue 205: Match Solr Porter behavior

## Approved spec

Canonical differential `text_en` must map `day` to `dai`; migration is explicit; remove the `terms_body` waiver.

## Delivered behavior

- Static built-in `text_en` v2 adds a narrow classic-Porter terminal-`y` compatibility filter before Tantivy Snowball.
- Shared `_dynamic_text` deliberately remains the v1 Snowball pipeline and identity, so captured Drupal Search API singular `day` remains `day`.
- This is analyzer compatibility, not complete Solr configset compatibility.

## Migration matrix

| Existing index | Outcome |
| --- | --- |
| Normal v1 static `text_en` | Refuses upgrade; reindex required. |
| Normal v1 dynamic-only | Safely upgrades: pipeline and identity are unchanged. |
| Pre-v1 or legacy `en_stem` dynamic | Fails before analyzed use. |
| Raw-only | Adoptable. |

## Changed coverage and documentation

Changed analyzer/schema migration coverage in `tests/schema_fieldtypes.rs` and `tests/schema_layer.rs`; differential and terms coverage in `tests/differential.rs` and `tests/terms.rs`. Updated `docs/PRD.md`, `docs/schema.md`, and `docs/solr-ref-findings.md`.

Fixture provenance: expected singular `day` behavior for dynamic fields is from the captured Drupal Search API Solr response; fixtures remain the compatibility ground truth. The canonical `text_en` expectation is `day -> dai`.

## Verification

- Focused analyzer, migration, differential, and terms test commands: passed.
- Mutation evidence: all three mutations were killed—removing the terminal-`y` filter, disabling the v1 static migration guard, and switching the dynamic catch-all to v2.
- Round-2 hermetic full gate passed: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.
- Final wording quick-fix targeted `fmt` and terms tests: passed.

## Review history

- Round 1 found a Drupal Search API regression. Architecture was revised so dynamic fields retain v1 behavior.
- Round 2 approved behavior and requested a wording-only quick fix; it was applied and verified.

## Residual risk and follow-up

Residual risk: terminal-`y` is a narrow compatibility shim, not a full Porter implementation. The v1 migration test uses a handcrafted empty index, though identity, filters, and production terms are covered.

No unresolved blocking follow-up.
