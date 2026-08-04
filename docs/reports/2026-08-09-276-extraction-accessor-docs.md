# #276 — mark `AppServer::extraction()` as test support and note its lifetime hazards

Follow-up 5 from #258 (`docs/reports/2026-08-02-extract-only-tracer.md`).

## Problem

`AppServer::extraction()` (`src/lib.rs`) was added in `6b88dcc` so the
concurrency test (`tests/extract_route.rs:88`) could hold the single extraction
permit and make saturation deterministic instead of racy. It is **public API**
today and hands back an `Arc<ExtractionRuntime>`, but the accessor's doc comment
hid two hazards the signature does not show:

1. The `Arc` can outlive the `AppState` it came from, deferring
   `ExtractionRuntime::drop` and keeping its `max_concurrency` dedicated OS
   threads alive as long as any clone is held.
2. A `mem::forget`-ed `ExtractionPermit` obtained through it permanently burns a
   concurrency slot — the same burnt-slot mechanism `ExtractionRuntime`
   documents for a wedged parser, but reachable by accident from outside the
   module.

Both are harmless in-tree (one test caller, which drops the permit on scope
exit); the issue asked only that they be discoverable from the accessor.

## Change

Rewrote the doc comment on `AppServer::extraction()` so that it:

- Leads with **Test support only.** and names the single in-tree caller.
- Points production code at
  `extract::ExtractionRuntime::spawn_extraction` instead of holding a permit
  directly.
- Keeps the existing accurate "additive" note (it hands back the *same* runtime
  the route uses, so a reserved slot is a slot the route can no longer hand out).
- Enumerates both lifetime hazards in a numbered list.

Production behaviour is untouched — this is a doc-comment-only change.

## Verification (local, hermetic — no network, no Docker)

- `cargo fmt --check` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean (CI's exact command).
- `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D
  rustdoc::private_intra_doc_links" cargo doc --no-deps --document-private-items`
  — clean. (`AppState` is private, so it is rendered as a plain code span rather
  than an intra-doc link to avoid a `private_intra_doc_links` warning; the
  `Arc`, `ExtractionRuntime`, `ExtractionPermit`, `try_acquire_permit`, and
  `spawn_extraction` links all resolve.)
- `cargo test --no-fail-fast` — **1189 passed / 0 failed**.

## Risk

None. Doc-comment only; no reachable code or test changed. Closes #276.
