# Issue #222 — spellcheck empty envelope

## Spec and trace

Captured trace `00021` requires strict `/select` acceptance of `spellcheck`,
`spellcheck.q`, repeated `spellcheck.dictionary=en&spellcheck.dictionary=und`, and
`spellcheck.collate`. With `spellcheck=true`, it returns `spellcheck` after
`highlighting`, with ordered empty `suggestions: []` and `collations: []`.
The trace explicitly uses `json.nl=flat`.

## Implementation

- `src/lib.rs`: allowlists the four params; strictly gates emission on
  `spellcheck=true`; emits the empty ordered envelope after highlighting.
- `tests/spellcheck.rs`: pins the captured request, strict-mode acceptance, ordering, and
  absence when `spellcheck` is absent or false. The expiring
  `delete_this_empty_ceiling_guard_when_real_spellcheck_suggestions_land` test must be
  deleted when real generation lands.
- `src/coverage.rs`, `tests/search_api_coverage.rs`: coverage moves **69/75 -> 75/75**,
  including a repeated-dictionary probe and explicit-`json.nl=flat` array probes.

This is **envelope compatibility only**, not misspelling correction. Injecting a `quick`
suggestion made the empty-ceiling guard fail. Forcing unconditional emission made the
absent/false gate checks fail.

## Accepted deviation and risk

`json.nl=flat` empty arrays are evidenced by trace `00021`. `json.nl=map` rendering and all
non-empty suggestion/collation shapes are deliberately deferred: the current empty envelope
does not model those shapes. **#223** owns real suggestion generation and is waiting on this
merge; it must replace the expiring ceiling guard and settle those shapes.

## Verification

- Targeted spellcheck and search-API coverage tests — passed.
- `cargo test` — passed.
- Final gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
  — passed.

## Review

- Round 1 requested quick fixes: coverage now exercises repeated dictionaries, and
  explicit-flat probes/comments document the captured shape and deferred map/non-empty scope.
- Round 2 — approved; no findings or additional follow-ups.
