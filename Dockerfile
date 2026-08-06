# Wayfinder release/benchmark image (issue #13, PRD §8: < 30 MB target).
#
# Multi-stage: build a statically linked (musl) release binary in a full
# Rust toolchain, then copy just that binary into a `scratch` final stage --
# nothing else, so the shipped image has no shell, no package manager, no
# dynamic libc dependency.
#
# The musl target is resolved from the builder's own architecture rather
# than hardcoded to x86_64: cross-architecture musl-gcc (e.g. targeting
# x86_64 from an arm64 build host) rejects arch-specific flags a native
# toolchain accepts (`-m64`), so a hardcoded target breaks on Apple Silicon
# Docker hosts building without an explicit `--platform`.

FROM rust:1-slim-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*

RUN set -eu; \
    arch="$(uname -m)"; \
    case "$arch" in \
      x86_64)  target=x86_64-unknown-linux-musl ;; \
      aarch64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported build architecture: $arch" >&2; exit 1 ;; \
    esac; \
    echo "$target" > /tmp/rust_target; \
    rustup target add "$target"

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY coverage ./coverage

RUN cargo build --release --target "$(cat /tmp/rust_target)" \
    && cp "target/$(cat /tmp/rust_target)/release/wayfinder" /wayfinder-bin

# An empty directory copied into the scratch final stage as /tmp. scratch has
# no filesystem at all, and extraction (#257/#258) streams an uploaded file to
# a tempfile under /tmp before parsing -- without this, /update/extract fails
# in the release image with "No such file or directory ... /tmp/...". Surfaced
# by the #262 end-to-end tracer, the first thing to exercise extraction against
# the scratch image (the differential harness runs against a separate Solr
# container, so this latent gap never surfaced there).
RUN mkdir /scratch-tmp

FROM scratch

COPY --from=builder /wayfinder-bin /wayfinder
# See the /scratch-tmp note above: provide the writable /tmp extraction needs.
COPY --from=builder /scratch-tmp /tmp
COPY presets /presets

ENTRYPOINT ["/wayfinder"]
