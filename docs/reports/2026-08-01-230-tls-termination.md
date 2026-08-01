# Issue #230 — TLS termination policy

## Decision and scope

Wayfinder remains an HTTP-only service and does not terminate TLS natively. Non-colocated clients
connect through an established reverse proxy such as Caddy, nginx, or Traefik; the
proxy-to-Wayfinder hop stays on loopback where possible, or on a trusted private network or
encrypted tunnel otherwise.

This follows the PRD's operational-simplicity goal. Adding `rustls` would still leave Wayfinder
responsible for certificate issuance, renewal, reload, and protocol policy, duplicating lifecycle
management already handled by deployment infrastructure. No dependency, server configuration, or
`src/` change was made.

The decision was posted to issue #230 before implementation, as required by the ticket.

## Changed behavior and files

Runtime behavior is unchanged.

- `docs/deployment.md`: records the HTTP-only trust boundary and gives a Caddy HTTPS reverse-proxy
  example. It keeps Wayfinder on `127.0.0.1:8983`, keeps external credential transport behind
  HTTPS, blocks both unauthenticated health endpoints at the public proxy by default, and covers
  remote-proxy and container-network constraints.
- `README.md`: links the deployment guide and makes the HTTP-only TLS policy explicit in the
  Basic-auth warning.
- `docs/PRD.md`: records reverse-proxy TLS termination under resource tuning and excludes
  certificate lifecycle management from the search process.

## Evidence and verification

Initial executable documentation check failed as expected because `docs/deployment.md` did not
exist. After implementation, the same check passed and confirmed the guide contains the Caddy
configuration, loopback upstream, reverse proxy, and both public health paths.

Full local and independent-review gates passed:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo fmt --check --manifest-path bench/Cargo.toml`
- `cargo clippy --manifest-path bench/Cargo.toml --all-targets -- -D warnings`
- `cargo test --manifest-path bench/Cargo.toml`
- `git diff --check`

The first reviewer attempt ended on a harness command-batching constraint before producing a
verdict. A fresh read-only reviewer then examined the named suspected weakness — invalid or
misleading Caddy, trust-boundary, or health-route guidance — ran one sanitized full gate, and
approved with no findings.

## Unresolved risks and follow-ups

Operators can still expose plaintext HTTP by binding Wayfinder publicly or publishing its
container port; documentation cannot enforce the network boundary. The guide therefore requires
keeping port 8983 private and makes Basic credentials over an untrusted plaintext hop explicitly
unsupported.

No native TLS implementation, accepted deviation, deferred code change, or other follow-up is
known.
