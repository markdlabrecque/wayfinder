# Issue #169 — `/terms` and `/admin/mbeans` differential coverage

## Spec

Resolve the missing `solr-ref/manifest.tsv` coverage for `GET /terms` and decide whether
`GET /admin/mbeans` belongs in the whole-response differential harness.

## Changed behavior

- Added `terms_body`, captured from Solr 9.10.1 against the canonical differential schema and
  five-document corpus, to `solr-ref/manifest.tsv` and the hermetic/live differential sweep.
- Added an executable manifest-policy guard: `/terms` must remain covered, while
  `/admin/mbeans` must remain excluded. The latter deliberately serves an honest subset of a
  48 KB Java/JVM response, so a whole-response row would only create a permanent waiver and
  contradict PRD section 5 v2.75.
- The capture settled a real analyzer divergence: Solr stems `day` to `dai`; Tantivy leaves
  `day`. A self-expiring exact-diff waiver under follow-up #205 allows only that one array value;
  every other term, frequency, ordering position, and envelope field remains enforced.

## Evidence

- Initial red:
  `cargo test --test differential manifest_covers_terms_but_deliberately_excludes_admin_mbeans -- --exact`
  failed because `terms_body` was absent.
- Capture: one-off `solr:9.10.1` container on port 8998, clean `content` core, canonical schema
  and corpus; container removed after capture.
- Targeted green:
  - `cargo test --test differential manifest_covers_terms_but_deliberately_excludes_admin_mbeans -- --exact`
  - `cargo test --test differential hermetic_whole_query_set_matches_committed_fixtures -- --exact`

## Follow-up

- #205 owns the `text_en` `day`/`dai` analyzer mismatch and the associated analyzer-contract /
  reindex decision. Removing the mismatch makes the differential harness fail until its narrow
  waiver is removed.

## Review

Round 1 returned must-fix: a whole-entry `EXPECTED_DIVERGENCES` waiver hid unrelated regressions.
It was replaced by an exact single-diff check plus a mutation-style guard proving an extra or
changed diff fails. Round 2 confirmed that weakness resolved and the PRD-backed `/admin/mbeans`
policy truthful, then caught one clippy `cloned-ref-to-slice-refs` error in the new guard. The
foreground fixed it with `std::slice::from_ref`; final full gate result is recorded below.

## Final gates

After the round-2 clippy fix, all passed:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
