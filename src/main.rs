//! Trivial binary wrapper around `wayfinder::app` (PRD §7 tracer bullet).
//!
//! Usage: `wayfinder <schema.toml> <data-dir> [bind-addr]`
//! `bind-addr` defaults to `127.0.0.1:8983` (Solr's default port).

use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let schema_path =
        PathBuf::from(args.next().ok_or_else(|| {
            anyhow::anyhow!("usage: wayfinder <schema.toml> <data-dir> [bind-addr]")
        })?);
    let data_dir =
        PathBuf::from(args.next().ok_or_else(|| {
            anyhow::anyhow!("usage: wayfinder <schema.toml> <data-dir> [bind-addr]")
        })?);
    let bind_addr = args.next().unwrap_or_else(|| "127.0.0.1:8983".to_string());

    let app = wayfinder::app(&schema_path, &data_dir)?;

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!("wayfinder listening on {bind_addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
