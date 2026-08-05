//! Production binary entry point for the Wayfinder HTTP server.
//!
//! Usage:
//! - Server: `wayfinder <schema.toml> <data-dir> [bind-addr]`
//! - Online snapshot: `wayfinder snapshot <live-data-dir> <fresh-destination-dir>`
//!
//! `bind-addr` defaults to `127.0.0.1:8983` (Solr's default port).
//!
//! The server config (PRD §6) comes from `WAYFINDER_CONFIG`; unset means all
//! defaults, and so does a path that does not exist.

use std::path::{Path, PathBuf};

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    sigterm.recv().await;
                }
                Err(error) => {
                    tracing::error!(%error, "failed to install SIGTERM handler");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => match result {
                Ok(()) => tracing::info!("received Ctrl-C; starting graceful shutdown"),
                Err(error) => tracing::error!(%error, "failed to receive Ctrl-C; starting graceful shutdown"),
            },
            _ = terminate => tracing::info!("received SIGTERM; starting graceful shutdown"),
        }
    }
    #[cfg(not(unix))]
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("received Ctrl-C; starting graceful shutdown"),
        Err(error) => {
            tracing::error!(%error, "failed to receive Ctrl-C; starting graceful shutdown")
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("snapshot") {
        let source = PathBuf::from(args.next().ok_or_else(|| {
            anyhow::anyhow!("usage: wayfinder snapshot <live-data-dir> <fresh-destination-dir>")
        })?);
        let destination = PathBuf::from(args.next().ok_or_else(|| {
            anyhow::anyhow!("usage: wayfinder snapshot <live-data-dir> <fresh-destination-dir>")
        })?);
        if args.next().is_some() {
            return Err(anyhow::anyhow!(
                "usage: wayfinder snapshot <live-data-dir> <fresh-destination-dir>"
            ));
        }
        wayfinder::snapshot::create(&source, &destination)?;
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

    let server = match std::env::var_os("WAYFINDER_CONFIG") {
        Some(path) => wayfinder::app_server_with_config(&schema_path, &data_dir, Path::new(&path))?,
        None => wayfinder::app_server(&schema_path, &data_dir)?,
    };
    let shutdown = server.shutdown_handle();

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(%bind_addr, "wayfinder listening");
    axum::serve(listener, server.into_router())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    shutdown.flush()?;
    tracing::info!("wayfinder shutdown complete");

    Ok(())
}
