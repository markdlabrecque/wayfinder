> **Historical implementation record.** This completed spec does not define current requirements or future work.

# #353 — five `hl.*` params the client sends are missing from `SELECT_PARAMS`

Branch: `353-highlight-params`. **Group B — land this before #350**, which also
edits `SELECT_PARAMS`. Rebase #350 onto this.

## The defect

`SearchApiSolrBackend::setHighlighting()` (`:4230-4275` in the vendored 4.4.0
source) emits five `hl.*` params absent from `SELECT_PARAMS`
(`src/lib.rs:198`). Under `strict_params = true` each one 400s a request the
client legitimately sends — exactly the failure the `SELECT_PARAMS` rule in
`CLAUDE.md` exists to prevent.

Wayfinder currently admits nine: `hl`, `hl.fl`, `hl.fragsize`,
`hl.mergeContiguous`, `hl.method`, `hl.requireFieldMatch`, `hl.simple.post`,
`hl.simple.pre`, `hl.snippets`. None of the five below.

The module sends these **only when non-default**, deliberately, "to have shorter
request strings" (its own comment at `:4243`):

| Param | Sent when | Source line |
|---|---|---|
| `hl.maxAnalyzedChars` | `!= 51200` | 4245-4247 |
| `hl.fragmenter` | `!= 'gap'` | 4248-4249 |
| `hl.usePhraseHighlighter=false` | setting is off | 4260-4262 |
| `hl.highlightMultiTerm=false` | setting is off | 4263-4265 |
| `hl.preserveMulti=true` | setting is on | 4266-4268 |

Because they are only sent when non-default, they are invisible to a capture of
a default-configured site — which is how they were missed.

## Two different kinds of work here

Do not treat all five the same.

**Admission** — all five must be in `SELECT_PARAMS` so `strict_params` stops
rejecting them. That alone fixes the 400s and is the minimum bar.

**Behaviour** — `hl.preserveMulti` is the one with real semantics: it changes how
multi-valued fields are highlighted. Implement it. The others are admission-plus-
behaviour in descending order of consequence; implement what the fixtures
support and mark the rest with a `ponytail:` naming the ceiling. **An admitted
param that is silently ignored is worse than a 400 if nothing says so** — a
`ponytail:` is what makes it honest.

## The upstream bug — scope around it, do not reproduce it

At `:4249-4251`:

```php
if ('gap' !== $highlighter['fragmenter']) {
  $hl->setFragmenter($highlighter['fragmenter']);
  if ('regex' !== $highlighter['fragmenter']) {   // <- inverted
    $hl->setRegexPattern(...);
```

The fragmenter options are `gap` and `regex`. Reaching the inner test means the
value *is* `regex`, so the condition is always false. **`hl.regex.pattern`,
`hl.regex.slop` and `hl.regex.maxAnalyzedChars` are never emitted by 4.4.0.**

So: implement `hl.fragmenter`, do **not** build `hl.regex.*`, and leave a
**self-expiring guard** asserting the inversion is still present in the vendored
source. The day upstream fixes it, the guard fails and names itself for removal,
and `hl.regex.*` becomes real work. Follow the pattern in
`tests/version_descope_guard.rs` / `tests/edismax_descope_guard.rs`.

## Verify before implementing

1. Re-read `:4230-4275` in the **now-vendored full source** and confirm all five
   params, their emission conditions, and the inverted regex branch. Report the
   real line numbers — the table above comes from the issue, not from a read of
   the tree.
2. Check whether the base corpus has a **multi-valued highlighted field**.
   `hl.preserveMulti` means nothing without one. If there is none, a new capture
   block is needed — do not fake it with a single-valued field.

## Fixtures

Capture what you implement. Append the block at the **end** of
`solr-ref/capture.sh`, run with `capture.sh --only <prefix>`, add core-relative
GET rows to `solr-ref/manifest.tsv`, and **commit the new fixtures before doing
anything else** — untracked fixtures are not restored by `git checkout -- solr-ref/`.

Never re-run the whole `capture.sh`; it rewrites every fixture and the
`QTime`/`_version_`/`rid` churn dirties every branch in the batch.

## Testing

Tests first, red, from fixtures. Cover:

- each of the five params is accepted under `strict_params = true` (the
  regression that motivates the issue)
- `hl.preserveMulti=true` changes multi-valued highlighting output, matching the
  fixture
- `hl.fragmenter` behaviour per fixture
- the self-expiring guard for the upstream regex inversion

`strict_params` rejection is compatibility-guard code: mutation-test it. Remove
a param from `SELECT_PARAMS`, confirm a test goes red, revert.

## Files

**You own:** `src/lib.rs` (`SELECT_PARAMS` — **add entries only, never
reorder**), `src/highlight.rs`, `tests/highlighting.rs`, `solr-ref/capture.sh`
(append at end), `solr-ref/manifest.tsv`, and the new guard test.

**Sibling:** #350 also edits `SELECT_PARAMS`. Land this first.

## Definition of done

- All five admitted; `hl.preserveMulti` and `hl.fragmenter` implemented against
  fixtures; anything inert carries a `ponytail:` naming the ceiling.
- Self-expiring guard for the `hl.regex.*` upstream inversion.
- Mutation test performed and reported.
- If a listed entry in `EXPECTED_DIVERGENCES` starts matching, **delete it** —
  that file fails if a listed divergence starts passing.
- `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
  clean.
