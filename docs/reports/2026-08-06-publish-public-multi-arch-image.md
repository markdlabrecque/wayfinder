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

## Post-merge operational evidence

- PR [#414](https://github.com/markdlabrecque/wayfinder/pull/414) merged as `59d83daa`.
- Main CI [run 31078404628](https://github.com/markdlabrecque/wayfinder/actions/runs/31078404628) passed all required jobs.
- Release [`v0.1.0`](https://github.com/markdlabrecque/wayfinder/releases/tag/v0.1.0) was published.
- Publish [run 31078672486](https://github.com/markdlabrecque/wayfinder/actions/runs/31078672486) passed the native `arm64` job on `ubuntu-24.04-arm`, the native `amd64` job on `ubuntu-latest`, and the manifest merge job.
- The [Wayfinder container package](https://github.com/markdlabrecque/wayfinder/pkgs/container/wayfinder) reports **Public**. Premise correction: it was already public on first inspection, so no manual private-to-public transition was necessary or possible. An anonymous clean-context pull with an empty temporary `DOCKER_CONFIG` succeeded. The release tag digest is `sha256:7d653a8b6f57ee669966512a466989cb9cc9d724b0b46b623fb7de47cce00687`.
- `docker buildx imagetools inspect` showed `linux/amd64` digest `sha256:aaf135158ab0608b03d23cb951dc257e833b5ad7f7a963dd4ffc0fbcb7bf8e8d` and `linux/arm64` digest `sha256:25c5f7301f2637bc105a7361761f8caa058416641c377733fe4732ff6ed64025`, plus the expected `unknown/unknown` attestations.
- On an actual Apple Silicon host, `uname -m` returned `arm64`; the pulled image ran as architecture `arm64`. The container started with `/presets/search-api.toml /data 0.0.0.0:8983`, mounted a temporary data volume, and `/wayfinder/content/admin/ping?wt=json` returned status `0` / `OK`. The mounted volume contained `wayfinder-schema.toml`, Tantivy locks and metadata, the analyzer contract, and `.managed.json`.
- One initial harness check incorrectly expected `schema.snapshot.json` and exited `1` after the ping had already passed. The corrected check verified the actual nonempty persisted volume, and the full rerun passed.

No unresolved risks or deferred follow-ups remain.
