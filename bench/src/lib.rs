//! Library behind `wf-bench`, the Wayfinder vs Solr benchmark (issue #440).
//! Standalone crate with no dependency on the `wayfinder` crate itself -- see
//! `Cargo.toml` for why.

pub mod baseline;
pub mod check;
pub mod cold;
pub mod footprint;
pub mod freshness;
pub mod index_stats;
pub mod load;
pub mod machine;
pub mod memory;
pub mod score;
pub mod terms;
pub mod wiki;
