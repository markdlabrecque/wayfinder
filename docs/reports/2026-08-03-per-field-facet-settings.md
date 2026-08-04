# #296 — per-field facet settings (`f.<field>.facet.*` and local params)

**Date:** 2026-08-03. **Branch:** `296-per-field-facet-settings`.
**Spec:** findings 147-151 in `docs/solr-ref-findings.md` and the 23 `facet_perfield_*`
fixtures (`manifest.tsv`) plus the 6 `pf296_sort_*` fixtures (`manifest-errors.tsv`, dedicated
`pf296` core) — expected values come from those, never from what the implementation happens to
produce.

## What shipped

Per-facet resolution of `facet.limit`, `facet.mincount`, `facet.sort`, and `facet.missing` in
finding-152 precedence order: `f.<field>.facet.X` beats a local param on that `facet.field`,
which beats the global `facet.X`. Resolution is always addressed by field name — never by a
`{!key=}` label, matching finding 147.

- `PER_FIELD_PARAMS` in `src/lib.rs` gained the three new names (`facet.limit`,
  `facet.mincount`, `facet.sort`) in the same commit as the implementation (`3616f32`), per its
  own `ponytail:` contract that adding a name early is a bug.
- `src/facet.rs` grew `addressed_setting`/`addressed_number` to read a setting off both the
  `f.<field>.facet.X` param and a local param on `facet.field`, at both places the plan phase and
  the response-shaping phase read facet globals, so the two phases cannot disagree.
- `QueryBuilder::buildFacets()` (Drupal module) now emits each facet's settings as local params
  on that facet's own `facet.field`, instead of global `facet.*` params — dropping a last-wins
  ceiling. Two facets over one field now keep their own settings, the case the module has needed
  since #299 keyed facets by Search API delta (two facets on one field, told apart by `{!key=}`).
  `ex=` still precedes `key=`, and a delta failing `[A-Za-z0-9_:-]+` still drops only the `key`
  half, per #298's invariants; the setting values appended to the same local-param block got the
  same hostile-value guard the key half already had (`localParamValue()`).
- README's "Per-field facet settings" bullet corrected: it previously claimed there was no
  `f.<field>.facet.*` override at all, which was already wrong before this ticket (`facet.missing`
  has had one since #140).

## How the ticket changed under capture

#296 as written asked only for the `f.<field>.facet.*` half: implement the three missing
per-field params and emit them from the module. The fixtures captured for it (findings 147-150,
then 151) showed that half cannot express the module's actual case — two facets on one field can
only disagree via the local-param form, because `f.<field>.facet.*` addresses the field, which
both facets share (finding 149) — and that one of the four params, `facet.missing`, was already
implemented since #140. The ticket's premise was wrong on its own terms. This is the third or
fourth v1 ticket where captured Solr contradicted the ticket text; the correct move, per this
repo's convention, was to flag it and build both halves rather than silently ship only the letter
of the ticket.

## Evidence

23 `EXPECTED_DIVERGENCES` entries and the matching rows citing #296 in
`EXPECTED_DIVERGENCES_MANIFEST_ERRORS` were deleted from `tests/differential.rs`, each because
its fixture now matches Wayfinder's response bit-for-bit — the self-expiry guard on that list
fails in both directions (an entry that starts matching fails the build), so this deletion is
verification, not assertion.

Final gates, re-run independently for this report:

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact invocation).
- `cargo test` — 1176 passed, 61 suites.
- `cargo test --test differential` — 41 passed.
- `cd drupal/search_api_wayfinder && vendor/bin/phpunit --filter QueryBuilderTest` — 63/63,
  99 assertions.

A local full-suite PHPUnit run reports ~51 pre-existing errors in the *extraction* test classes
(`Drupal\file\FileInterface` missing from a stale local vendor install) — unrelated to this
change, not touched by this branch.

## Review, honestly

Two rounds, both bounced with must-fix findings.

- **Round 1 must-fix:** `f.<field>.facet.mincount=-1` (an addressed, negative mincount) returned
  HTTP 400, while the bare global `facet.mincount=-1` returned 200 on the same server. The
  global path's `u64::from_str().ok()` was silently swallowing the parse failure and defaulting
  to 0; the new addressed path's stricter behaviour was the actual outlier, not the global. Fixed
  in `62d1d50` by parsing as `i64` and clamping, rather than rejecting. Neither direction is
  fixtured, so matching the existing (lenient) global path was the conservative call — see
  follow-up 1.
- **Round 2 must-fix:** a new test (`facet_limit_unlimited`-adjacent) derived its expected bucket
  order from Wayfinder's own response and compared unordered, when two sibling tests
  (`facet_limit_unlimited`, `facet_perfield_overrides_global`) already pinned the same case's
  ordered array against the fixture. Corrected in `399e734` to assert the fixture directly. Worth
  recording as a process point, not just a fix: the test reached round 2 with a
  fixture-contradicting comment still in it, and it was the independent review pass — not the
  same-model author — that caught it.
- **What review cleared under mutation testing** (each check deliberately broken, confirmed a
  test failed, then reverted): precedence is not backwards (per-field beats local beats global,
  not the reverse); there is no fallback to the `{!key=}` label anywhere in the addressed lookup;
  there is no fourth site reading the facet globals that the per-field resolution missed;
  the PHP local-param value escaping has no block-breakout path for a hostile setting value; and
  post-exclusion ranking (finding 150) is structural — it falls out of the existing exclusion
  order, not an accident of the fixtures picked.

Two rounds is the reviewer's default cap; both rounds found a real must-fix, so this work could
still use further review passes rather than being treated as exhaustively checked.

## Open follow-ups

None of these are closed by review approval — they are genuinely open.

1. No fixture pins a negative `facet.mincount` (addressed or global) or a non-numeric addressed
   mincount; both behaviours (accept-and-clamp, reject) are currently guaranteed only
   structurally, not against captured Solr. Capturable as core-relative rows against the existing
   `pf296`/default cores.
2. The 400-message test for a bad addressed boolean (`facet.missing`) asserts Wayfinder's own
   wording, not a captured one. A real fixture is capturable: `{!facet.missing=nope}category`
   as a core-relative 400 GET, alongside `facet_perfield_err_bad_limit`.
3. `QueryBuilder.php:515` — `ex=facet:<field_id>` is still emitted unguarded, while the setting
   values beside it in the same local-param block now are guarded. `localParamValue()` would
   leave an ordinary field id byte-identical, so hardening this is free; it just hasn't been
   done.
4. No Rust-side test pins the `"`/`\`-inside-double-quotes half of the PHP/Rust local-param
   escape contract — exactly what `localParamValue()` emits on the PHP side. The PHP side is
   covered; the server side that has to parse it back is not.
5. `f.<field>.facet.limit` parses as `i64` on the server; Solr's own `getFieldInt` is 32-bit.
   Unfixtured; a value between `i32::MAX` and `i64::MAX` would diverge.
6. A `facet.sort` value that is neither `count` nor `index` is silently read as `count`, in every
   form (global, per-field, local-param) — pre-existing behaviour, unfixtured, not addressed by
   this ticket.
7. `search_api_solr`'s `f.<field>.facet.range.*` behaviour is unverifiable on this machine —
   Solarium is not vendored here and is not available to inspect. Moot for #296 itself (the
   module emits no `facet.range` at all), recorded as a named descope rather than a silent
   omission.

## Ceilings left as `ponytail:` comments

- `src/facet.rs:507` — only the *addressed* forms of `facet.limit`/`facet.mincount` validate a
  non-numeric value with a 400. A non-numeric **bare global** `facet.limit`/`facet.mincount` is
  still silently defaulted, as it was before this function existed; real Solr 400s on that too,
  but no fixture pins it and changing it would be a behaviour change outside #296.
- `drupal/search_api_wayfinder/src/QueryBuilder.php:472` — facet settings as local params on
  `facet.query` are out of scope: Solr does honour local params there too, but this module emits
  no `facet.query` at all, so only `facet.missing` would ever have a meaning on that block, and
  nothing captures it.

## Commits

`9a20598` (fixtures/findings), `1cf5525` (red tests), `3616f32` (implementation +
`PER_FIELD_PARAMS`), `62d1d50` (round-1 fix: addressed mincount clamping), `399e734` (round-2 fix:
assert fixture order instead of self-derived order).
