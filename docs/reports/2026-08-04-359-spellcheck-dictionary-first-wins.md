# #359 — `spellcheck.dictionary` is first-wins in Solr; no merge to build

**Date:** 2026-08-04. **Branch:** `markdlabrecque/issue-359-server-consumes-only`.
**Status:** **closed as a wrong premise** — Wayfinder already matches Solr; the
merge the issue asks for would itself be a divergence.

Follow-up from #342 (language-aware text field naming and spellcheck wiring).

## The behaviour under examination

#359 claims a client may send `spellcheck.dictionary` several times to "consult
multiple dictionaries", and that Wayfinder reading only the first value (via
`params.get(...)` in `fn spellcheck`, `src/lib.rs`) silently collapses a
multilingual query to one dictionary's suggestions.

## The evidence: verified live against real Solr 9

Same setup as the #223 spellcheck capture — the `spellcheck_223` core with the
`search-api` configset, which registers **two** named spellcheckers
(`solrconfig_extra.xml`: `en` over `field=spellcheck_en`, `und` over
`field=spellcheck_und`). The corpus is built to make them disagree:
`spellcheck_en` = {quick, rocket, brown, fox}, `spellcheck_und` = {quack,
garden}; for the misspelling `qwick`, `en`'s nearest is `quick` (1 edit) and
`und`'s nearest is `quack` (2 edits).

Re-ran against `solr:9` (port 9099, `solr-precreate`):

| Request | `suggestions` |
|---|---|
| `spellcheck.dictionary=en&spellcheck.dictionary=und` | `qwick → quick` (en) |
| `spellcheck.dictionary=und&spellcheck.dictionary=en` | `qwick → quack` (und) |
| `spellcheck.dictionary=en` (alone) | `qwick → quick` |
| `spellcheck.dictionary=und` (alone) | `qwick → quack` |

The multi-dictionary response is **byte-identical** to sending just the first
dictionary alone. Solr consumes only the first value and ignores the rest.

This is unambiguous about there being no merge:

- A "merge and pick the closest candidate" reading would return `quick` in
  **both** orders (1 edit beats 2); und-first returned `quack`.
- A "merge all, one suggestion per dictionary" reading would return two
  suggestions; only one came back.
- Confirmed under `json.nl=map` too: same first-wins, single suggestion.

This is already captured as ground truth — `solr-ref/responses/spellcheck_
dictionary_en_first.json` / `_und_first.json`, captured by the #223 block of
`capture.sh` precisely because "repeated dictionary precedence is observable
rather than inferred" — and asserted by `tests/spellcheck.rs`
(`first_repeated_spellcheck_dictionary_wins_when_en_is_first` /
`_when_und_is_first`), which pass against the current `params.get` read.

## Decision: wrong premise — no Solr-compatible change

Implementing the merge #359 asks for would make Wayfinder diverge from Solr,
which is a bug under the compatibility contract ("Divergence from captured
Solr behaviour is a bug"), not a fix. The correct move is to record the
behaviour and close the issue, not build to the wrong spec.

`spellcheck.dictionary` stays in `SELECT_PARAMS` as a repeatable param —
trace 00021 sends it twice — so the echo and `strict_params` paths still accept
the repeat. Only the read-path consumes the first value, exactly as Solr does.

## Changes

- **`docs/solr-ref-findings.md`** — finding 193, recording first-wins with the
  live-Solr evidence above and citing the existing fixtures/tests that pin it.

No `src/` production code touched — this is a docs-only change. There is
nothing to guard: the behaviour the issue wanted changed is already the
Solr-compatible behaviour, and the existing `tests/spellcheck.rs` tests already
assert it.

## Verification

```
cargo test --test finding_citations          # 193 resolves; no dup; no dangling cite
cargo test --test spellcheck                 # the two first-wins tests pass
cargo fmt --check                            # clean (no .rs changes)
cargo clippy --all-targets -- -D warnings    # clean (no .rs changes)
```
