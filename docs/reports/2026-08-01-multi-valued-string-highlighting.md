# Issue #185: multi-valued string highlighting

## Spec

An explicitly named multi-valued `string` field must highlight each stored value independently. For `q=category:animals&hl.fl=category`, both `doc1` (`["animals", "classic"]`) and `doc4` (`["animals"]`) must return `category:["<em>animals</em>"]`, matching Solr.

## Resolution

The production defect was already fixed on `main` by #213 while resolving #184. Its raw-string fallback reads stored values separately instead of relying on Tantivy's space-joined raw-token snippet path. The same PR added `hl_wildcard_fl_matches_stored_string_fixture_and_explicit_field`, which exercises the explicit `hl.fl=category` request and asserts the multi-valued `doc1` result against the captured Solr highlighting block.

No additional production or test change is appropriate for #185: duplicating the existing fallback or regression would weaken rather than clarify ownership. This report records the overlap so the ticket can close through a PR as required by the repository workflow.

## Evidence

Historical red, recorded in `docs/reports/2026-08-01-hl-string-field-consistency.md` before #213's implementation:

```text
cargo test --test highlighting hl_wildcard_fl_matches_stored_string_fixture_and_explicit_field
FAILED: wildcard returned {"doc1":{},"doc4":{}} instead of category snippets
```

Current verification:

```text
cargo test --test highlighting hl_wildcard_fl_matches_stored_string_fixture_and_explicit_field -- --exact
PASS: 1 passed, 0 failed

cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
PASS
```

Independent review round 1: **APPROVE**. The reviewer confirmed that the existing schema/corpus, explicit request, fixture, fallback, and report cover the exact issue, then reran the full gate successfully.

The fixture `solr-ref/responses/hl_wildcard_stored_string.json` contains the captured expected snippets for both documents; finding 110 in `docs/solr-ref-findings.md` records that explicit and wildcard `hl.fl` produce the same highlighting block.

## Follow-ups

None.
