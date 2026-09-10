use std::{future::IntoFuture, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use cliproxyapi_rs::{AppState, admin_router, api_router, config::Config};
use tokio::{net::TcpListener, sync::watch};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "cliproxyapi-rs", version, about)]
struct Cli {
    #[arg(short, long, default_value = "config.yaml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("cliproxyapi_rs=info")),
        )
        .compact()
        .init();
    let cli = Cli::parse();
    let config = Config::load(&cli.config).await?;
    let public_addr = config.public_addr();
    let admin_addr = config.admin_addr();
    let state = AppState::new(config).await?;
    let public_listener = TcpListener::bind(public_addr)
        .await
        .with_context(|| format!("bind public listener {public_addr}"))?;
    let admin_listener = TcpListener::bind(admin_addr)
        .await
        .with_context(|| format!("bind admin listener {admin_addr}"))?;
    tracing::info!(%public_addr, "public API listener ready (management routes absent)");
    tracing::info!(%admin_addr, "private API + management listener ready");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let public = axum::serve(public_listener, api_router(state.clone()))
        .with_graceful_shutdown(shutdown(shutdown_rx.clone()))
        .into_future();
    let admin = axum::serve(admin_listener, admin_router(state))
        .with_graceful_shutdown(shutdown(shutdown_rx))
        .into_future();
    tokio::pin!(public, admin);
    tokio::select! {
        result = &mut public => result.context("public server failed")?,
        result = &mut admin => result.context("admin server failed")?,
        signal = tokio::signal::ctrl_c() => signal.context("install shutdown handler")?,
    }
    let _ = shutdown_tx.send(true);
    Ok(())
}

async fn shutdown(mut receiver: watch::Receiver<bool>) {
    if !*receiver.borrow() {
        let _ = receiver.changed().await;
    }
}
