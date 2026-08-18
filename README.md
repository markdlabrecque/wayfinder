# Wayfinder

A search backend in Rust, built on Tantivy: one static binary, one schema file, and one data
directory. It retains a Solr-shaped JSON wire for supported existing-client routes; it is not a
general Solr replacement or an ongoing parity project.

## Documentation

- [Configuration](docs/CONFIGURATION.md) — schema, analyzers, server settings, defaults, and live knobs
- [Compatibility](docs/COMPATIBILITY.md) — supported wire, capabilities, divergences, and boundaries
- [Deployment](docs/DEPLOYMENT.md) — systemd, containers, TLS, backup, restore, reindex, and upgrades
- [Development](docs/DEVELOPMENT.md) — architecture, tests, benchmarks, and contribution workflow

## Running

```sh
wayfinder <schema.toml> <data-dir> [bind-addr]     # defaults to 127.0.0.1:8983
wayfinder snapshot <live-data-dir> <fresh-destination-dir>
```

Wayfinder takes two deliberately separate TOML files:

| File | Scope | Input |
|---|---|---|
| `schema.toml` | One core: fields, types, and unique key | First CLI argument |
| `wayfinder.toml` | Process tuning and security | `WAYFINDER_CONFIG` environment variable |

Both are optional beyond the required schema path: a missing server configuration selects all
defaults. Unknown server-config keys are errors; unknown request parameters are ignored unless
`strict_params` is enabled. See [CONFIGURATION.md](docs/CONFIGURATION.md) for the complete reference.

## Security

Wayfinder serves HTTP only. Bind it to loopback or a trusted private network and terminate TLS at
an established reverse proxy for remote clients. HTTP Basic authentication is plaintext-equivalent
without TLS. The configured core's `/admin/ping` and `/ui/ping` remain unauthenticated health
checks. See [DEPLOYMENT.md](docs/DEPLOYMENT.md) for the supported proxy model.

## Tests

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests are hermetic: no network and no Docker. `solr-ref/responses/` is the frozen regression
baseline for the retained wire; fixture-derived expected values never come from implementation
output.
