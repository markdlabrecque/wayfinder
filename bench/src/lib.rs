//! Benchmark tooling for issue #13: a deterministic corpus generator plus a
//! results-table renderer used to produce `bench/RESULTS.md`. Standalone
//! crate, `std`-only, no dependency on the `wayfinder` crate itself -- see
//! `Cargo.toml` for why.

pub mod check;
pub mod cold;
pub mod corpus;
pub mod results;
pub mod terms;
pub mod wiki;
