# Issue #412: Publish public multi-architecture image

## Spec

GitHub issue #412 is the approved specification.

## Implemented behavior

- `Dockerfile`: the final `scratch` stage includes the repository presets at `/presets`.
- `.github/workflows/publish.yml`: the release workflow builds native images on `ubuntu-latest` for `amd64` and `ubuntu-24.04-arm` for `arm64`, pushes digest-addressed images, and merges them into an immutable release-tag manifest at `ghcr.io/${owner}/wayfinder`.
- `tests/publish_public_multi_arch_contract.rs`: contract coverage for the preset copy and multi-architecture publish workflow.

## TDD and gate evidence

- Red: `cargo test --test publish_public_multi_arch_contract` — failed 2/2 tests for the expected missing behavior: no preset copy and no publish workflow.
- Green: `cargo test --test publish_public_multi_arch_contract` — passed 2/2 tests.
- Implementer handoff: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — passed.
- Reviewer round 1: the same full local gate — passed.
- Reviewer round 2: no gate rerun; it relied on the passing round 1 gate and approved the retained post-merge evidence plan.

The Cargo test suite has no Docker or network dependency.

## Review

- Round 1 requested registry visibility and runtime evidence.
- Round 2 approved the code because that evidence is explicitly retained as post-merge operational work.
- No code findings remain. No accepted implementation deviations were recorded.

## Pending post-merge evidence and risks

The following steps remain pending and are not claimed as passed:

1. Publish a release tag.
2. Set the GHCR package public once.
3. Verify an anonymous pull.
4. Verify the published manifest contains both `amd64` and `arm64`.
5. Run the image with `/presets/search-api.toml`, a data volume, and a successful ping.
6. Run the published image on actual arm64 hardware.

Until these steps complete, public registry visibility, manifest composition, packaged-preset startup, persistent-volume operation, health behavior, and actual arm64 runtime compatibility remain operational risks. There are no other deferred follow-ups.
