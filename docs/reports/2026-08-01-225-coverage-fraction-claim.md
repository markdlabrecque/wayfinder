# Issue #225 — define the 75/75 coverage claim

## Spec and decision

Document what `wayfinder coverage`'s 75/75 means without lowering the number: it is a recomputable wire-contract score over the fixture-derived `search_api_solr` 4.4.0 denominator, not a claim that every Solr component is feature-complete.

The PRD now separates the evidence layers. The fraction comes from item-specific runtime probes; differential and feature-specific tests separately carry fixture-derived semantic and explicit key-order assertions. The project claim is that captured client request shapes receive the expected parameter handling and client-consumed envelope, including required key order—not byte-for-byte parity for every trace.

## Spellcheck scope

The v3 placement now records both halves explicitly:

- #222 delivered the tracer slice for the four captured spellcheck parameters and the captured empty `suggestions`/`collations` envelope.
- #223 still owns real suggestions and collations; the separate `suggest` path also remains v3.

The 75/75 paragraph names spellcheck as the current envelope-only case.

## Corrected ticket premise

Issue #225 conditionally described `mlt.maxntp` as a second current gap. That premise became stale before this work started: #189 landed in PR #221 and now implements and probes the token cap. The PRD records it as the historical case that exposed the distinction, not as unimplemented behavior.

## Verification

- `cargo run --quiet -- coverage --format json` — overall **75/75**; endpoints 9/9, request semantics 51/51, response fields 15/15.
- `git diff --check` — passed.
- `cargo fmt --check` — passed.
- Focused PRD content assertions for the claim, spellcheck split, and `mlt.maxntp` correction — passed.
- Full Rust tests were not run because the issue is deliberately docs-only and changes no source or test code.

## Review

Round 1 found that the first draft attributed envelope and key-order proof directly to the coverage probes and placed completed spellcheck work in a non-phase row. Both were corrected by distinguishing the evidence layers, retaining the delivered tracer slice in v3, and naming the four captured parameters.

Round 2 confirmed the spellcheck correction but found the phrase “exact fixture parity” still too broad because the differential suite does not replay every Search API trace and some admin fixtures have ratified divergences. After the two-round cap, the foreground fix replaced that claim with the narrower fixture-derived semantic and explicit key-order evidence statement. Final documentation checks passed; no unresolved follow-up remains.
