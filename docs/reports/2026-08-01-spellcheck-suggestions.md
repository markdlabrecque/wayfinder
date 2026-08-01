# Issue #223 — real spellcheck suggestions

## Goal, scope, and dependencies

Issue [#223](https://github.com/markdlabrecque/wayfinder/issues/223) replaces #222's deliberately empty spellcheck envelope with configured-Solr-compatible real suggestions. It depends on [#222](https://github.com/markdlabrecque/wayfinder/issues/222) for strict parameter acceptance and envelope gating, and [#226](https://github.com/markdlabrecque/wayfinder/issues/226) for `/select` routing/acceptance of that envelope.

Configured Solr was captured as the contract: five committed fixtures cover first-dictionary selection (`en` and `und`), `json.nl=flat`, `json.nl=map`, collation, and Unicode offsets. The Unicode fixture specifically establishes the UTF-16 offset bounce required by Solr wire output.

## Implementation

- The first requested dictionary resolves to `spellcheck_<name>`.
- A real term dictionary is searched in O(dictionary) time using Damerau distance <= 2 and emits one candidate.
- Suggestions render in both flat and map forms, with collation; the prior empty-envelope ceiling guard was replaced by real-behaviour coverage.
- Offset conversion now reports Solr-compatible UTF-16 positions rather than UTF-8 byte positions.
- A narrow `ponytail:` simplification remains: analyzer handling, ranking, and performance are intentionally limited to the captured contract.

Changed categories: select/spellcheck production code (`src/lib.rs`); spellcheck and differential tests (`tests/`); configured-Solr captures and capture/manifest metadata (`solr-ref/`); Solr findings documentation; and this report. The five new response fixtures are `spellcheck_dictionary_{en,und}_first.json`, `spellcheck_{flat,map}.json`, and `spellcheck_unicode_offsets.json`.

## Evidence and review

- **Initial red:** four fixture tests failed with the #222 empty `suggestions`/`collations` envelope, proving the missing real-generation behaviour.
- **Targeted verification:** spellcheck fixture, flat/map, dictionary-selection, collation, and coverage tests passed after implementation.
- **First full gate:** failed because the four new manifest rows lacked a dedicated hermetic app; routing those rows to the captured spellcheck schema/corpus fixed the harness, then the gate passed.
- **Review round 1:** bounced on Unicode offsets being UTF-8 based rather than Solr's UTF-16 contract.
- **Capture-backed fix:** added/used the Unicode-offset fixture and converted offsets to UTF-16.
- **Review round 2:** approved; no findings or follow-ups remain from review.
- **Latest post-rebase full gate:** `cargo fmt --check` — clean; `cargo clippy --all-targets -- -D warnings` — clean (CI-exact invocation); `cargo test` — all green.

## Residual ceiling

The narrow implementation deliberately does not generalize analyzer semantics, candidate ranking, or performance beyond the five captured configured-Solr cases. Reason: #223's approved contract is the captured behaviour, not a complete Solr spellcheck implementation. Decision: retain the explicit narrow `ponytail:` ceiling. Risk: unfixtured analyzers, ranking ties, and large dictionaries may diverge or scale poorly. Evidence: all five fixtures and the post-rebase full gate are green.

No unresolved risk exists beyond that documented analyzer/ranking/performance ceiling. Coverage remains truthful to the captured contract; no fixture-derived assertion was relaxed or hidden. No deferred follow-ups or deliberate skips remain.
