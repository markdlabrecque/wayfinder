//! Trivial binary wrapper around `wayfinder::app` (PRD §7 tracer bullet).
//!
//! Usage: `wayfinder <schema.toml> <data-dir> [bind-addr]`
//! `bind-addr` defaults to `127.0.0.1:8983` (Solr's default port).
//!
//! The server config (PRD §6) comes from `WAYFINDER_CONFIG`; unset means all
//! defaults, and so does a path that does not exist.

use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("coverage") {
        if args.next().as_deref() != Some("--format") || args.next().as_deref() != Some("json") {
            return Err(anyhow::anyhow!("usage: wayfinder coverage --format json"));
        }
        if args.next().is_some() {
            return Err(anyhow::anyhow!("usage: wayfinder coverage --format json"));
        }
        println!(
            "{}",
            serde_json::to_string(&wayfinder::coverage_report().await)?
        );
        return Ok(());
    }
    let schema_path =
        PathBuf::from(first.ok_or_else(|| {
            anyhow::anyhow!("usage: wayfinder <schema.toml> <data-dir> [bind-addr]")
        })?);
    let data_dir =
        PathBuf::from(args.next().ok_or_else(|| {
            anyhow::anyhow!("usage: wayfinder <schema.toml> <data-dir> [bind-addr]")
        })?);
    let bind_addr = args.next().unwrap_or_else(|| "127.0.0.1:8983".to_string());

    let app = match std::env::var_os("WAYFINDER_CONFIG") {
        Some(path) => wayfinder::app_with_config(&schema_path, &data_dir, Path::new(&path))?,
        None => wayfinder::app(&schema_path, &data_dir)?,
    };

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!("wayfinder listening on {bind_addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
