# Issue #56: compute the Search API coverage denominator

## Goal

Turn the frozen `search_api_solr` contract from issue #55 into a hermetic, recomputable coverage report showing which captured endpoints, request-parameter semantics, and client-consumed response fields Wayfinder serves today.

## Parallel ownership and shared contracts

Issue #57-M1 is concurrently owned by another harness.

- #56 owns its coverage extractor/model, coverage tests or command, this plan, and `docs/reports/2026-07-30-56-coverage-denominator.md`.
- #56 must not modify `drupal/`.
- #56 must not modify `.github/workflows/ci.yml`; `cargo test` integration is sufficient for this issue and avoids the #57-M1 hot file.
- Wayfinder's existing route and strict-parameter surface in `src/lib.rs` is the shared contract. Prefer reading or minimally exposing the existing constants rather than creating a second manually maintained capability list. Any necessary `src/lib.rs` edit belongs to #56 and must preserve runtime behavior.
- Frozen `solr-ref/search-api/` artifacts are inputs. Do not recapture or rewrite them.

## Plan

1. Define and test a deterministic contract model derived from all 28 frozen trace files plus `manifest.tsv`: normalized endpoint shapes, request parameters, and meaningful syntax/semantics variants where one parameter name represents materially different behavior.
2. Pin the client-consumed response-field denominator with explicit provenance to the frozen trace and the captured `search_api_solr` 4.4.0 behavior. Do not count volatile or unread host-introspection fields merely because Solr emitted them. Keep any checked-in derived contract reproducible and guarded against trace drift.
3. Derive Wayfinder's numerator mechanically:
   - endpoint support from actual routed behavior or the router's source-of-truth declarations;
   - parameter support from the same `SELECT_PARAMS`, `UPDATE_PARAMS`, `MLT_PARAMS`, and endpoint parameter declarations used by strict-parameter validation;
   - response-field support from actual rendered fixture/handler behavior, not a manually asserted "supported" list.
4. Produce a stable report with covered/uncovered items and separate endpoint, request-semantic, and response-field subtotals plus an overall fraction. Unsupported captured Solr features such as `/terms`, `/schema/fieldtypes`, `/admin/luke`, `/admin/mbeans`, spellcheck, and unsupported highlighting/local-param variants must remain visible rather than being normalized away.
5. Make the report runnable hermetically through `cargo test` with no network or Docker. Add guards proving denominator changes when trace inputs change and numerator changes when Wayfinder capability sources change; mutation-test the critical unsupported/covered classification path.
6. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
7. Record the initial fraction, full uncovered list, derivation method, commands/results, review verdict, and v2 backlog implications in `docs/reports/2026-07-30-56-coverage-denominator.md`.

## Acceptance criteria

- The denominator is reproducibly derived from the complete #55 frozen contract, with client-consumed response fields carrying auditable provenance.
- The numerator is mechanically coupled to Wayfinder's real route, strict-param, and rendered-response behavior rather than a hand-maintained support list.
- A hermetic `cargo test` command prints deterministic covered/uncovered items and fractions.
- Unsupported items remain explicit and the report does not claim semantic coverage from endpoint/parameter-name presence alone.
- The initial number and uncovered backlog are committed in the issue report.
- All repository gates pass, and no #57-owned or shared CI file is changed.
