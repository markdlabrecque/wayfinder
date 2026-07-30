# Report: issue #64 — configurable request-body limit

## What was built

Axum's `Bytes`/`Json` extractors enforce a bare, hardcoded 2MB request-body cap by
default (`axum::extract::DefaultBodyLimit`'s built-in ceiling), applied unconditionally
to Wayfinder's `/update` endpoint with no override and no config knob. This is too small
for a realistic bulk `search_api_solr` `/update` batch.

Fix: a new `resources.max_body_size` field (`usize`, default `10_000_000` bytes / 10MB)
on `ServerConfig`, wired to an explicit `DefaultBodyLimit::max(...)` layer in
`build()` (`src/lib.rs`), replacing axum's implicit 2MB default with an operator-
configurable one, consistent with every other knob under `[resources]`.

Files touched:
- `src/config.rs` — new `Resources::max_body_size: usize` field, `Default` impl value
  `10_000_000`, doc comment explaining the axum default it replaces and why 10MB
  could not be hermetically verified against a Solr equivalent, plus a config-default
  test asserting `10_000_000`.
- `src/lib.rs` — imports `axum::extract::DefaultBodyLimit`; `build()` now layers
  `DefaultBodyLimit::max(max_body_size)` (read from `config.resources.max_body_size`)
  onto the router alongside the existing `CatchPanicLayer`.
- `tests/body_limit.rs` (new, 184 lines) — exercises the configured cap end-to-end
  against `/update`.
- `README.md`, `docs/PRD.md` — document the new `[resources] max_body_size` knob and
  its rationale.
- `docs/solr-ref-findings.md` — new finding 79 (see below).

## Why the default is 10MB, not fully verified hermetically

Finding 79 (`docs/solr-ref-findings.md`) records that the captured `search_api_solr`
`solrconfig.xml`'s `requestParsers` attributes (`formdataUploadLimitInKB`,
`multipartUploadLimitInKB`) govern `multipart/form-data` and
`application/x-www-form-urlencoded` uploads specifically — neither content type a bulk
JSON `/update` uses. Worse, the block that names them in the exported configset is
itself inside an HTML comment; the actually-active `<requestDispatcher>` (via entity
include) sets neither attribute at all. None of the repo's captured fixtures exceed
~7KB. So there is no in-repo, hermetic signal for Solr's own effective raw-JSON-body
ceiling, and no live-Solr probe was run to establish one (that would require
`WAYFINDER_DIFF_SOLR=1`, out of scope here).

**10MB is therefore a deliberate, round headroom figure over the largest known
captured fixture (~7KB)** — not a value derived from or matching a verified Solr
default. This is stated explicitly in the `Resources::max_body_size` doc comment,
the PRD entry, and finding 79, so it doesn't read as a false compatibility claim.

## Review rounds

**Round 1** bounced the implementor on:
- an uncommitted test file (`tests/body_limit.rs` existed on disk but wasn't staged)
- a non-load-bearing config test (asserted a value that would pass even if the field
  were wired to nothing)
- a wrong Solr-precedent justification for the 10MB default (an earlier draft of the
  doc comment implied a Solr-side equivalent existed; it doesn't — see finding 79)
- missing README/PRD updates and a missing config-default test for the new field
- a misleadingly-named test that didn't test what its name claimed

All of the above were fixed in commit `57f1eaf feat(config): make request body limit
configurable (issue #64)`, which is the sole commit on this branch relative to
`origin/main`.

**Round 2**: **APPROVED**. Reviewer confirmed the fixes and raised two additional,
non-blocking observations, logged as follow-ups below rather than fixed in this PR
(the round cap is 2; per repo convention on capped review rounds, this note records
that the work could still use further review passes rather than treating round 2 as
exhaustive).

## Test evidence

```
cargo test: 488 passed (23 suites, 25.60s)
cargo fmt --check: clean (exit 0)
cargo clippy --all-targets -- -D warnings: clean, no warnings
```

All hermetic — no network, no Docker, `WAYFINDER_DIFF_SOLR` unset.

## Follow-ups (deferred, not fixed in this PR)

1. **A 413 rejection from `DefaultBodyLimit` bypasses Wayfinder's JSON error
   envelope.** Every other `/update` error path returns Wayfinder's standard
   enveloped JSON error; an oversized body instead gets axum-core's bare
   `LengthLimitError` rejection, a plain-text (non-JSON) 413 response with no
   envelope. No fixture exists yet for what real Solr returns on an oversized raw
   body either, so it's unknown whether Solr's shape differs. Suggested next step:
   capture real-Solr behaviour on an oversized `/update` body behind
   `WAYFINDER_DIFF_SOLR=1`, then either (a) envelope the 413 to match Wayfinder's own
   error convention, or (b) if Solr's own oversized-body response is also
   unenveloped/divergent, record it as a documented divergence entry in
   `tests/differential.rs`'s `EXPECTED_DIVERGENCES` rather than leaving it silently
   unmatched.

2. **`resources.max_body_size == 0` is unvalidated in `ServerConfig::validate()`.**
   A config with `max_body_size = 0` would silently 413-reject every non-empty
   `/update` request with no startup-time warning. `resources.writer_threads == 0`
   already has a validation guard in the same struct as precedent for the pattern
   this field is missing. Suggested next step: add a `validate()` check rejecting
   (or defaulting) `max_body_size == 0`, mirroring the `writer_threads` guard, with
   a test asserting the guard fires.

## Commit / diff summary

Single commit on this branch vs. `origin/main`:

```
57f1eaf feat(config): make request body limit configurable (issue #64)

 README.md                 |  7 ++++++-
 docs/PRD.md               |  7 +++++++
 docs/solr-ref-findings.md | 16 ++++++++++++++++
 src/config.rs             | 18 ++++++++++++++++++
 src/lib.rs                |  9 +++++++--
 tests/body_limit.rs       | 184 +++++++++++++++++++++++++++++++++++++
 6 files changed, 238 insertions(+), 3 deletions(-)
```
