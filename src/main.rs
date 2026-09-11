//! Repsetarr entry point: load the config, keep sources warm, serve the API.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use repsetarr::cache::CacheStore;
use repsetarr::state::{AppState, SharedState};
use repsetarr::{api, config, refresh};
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = "/config/config.yml";
const STARTER_CONFIG: &str = include_str!("../config.example.yml");

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config_path = config_path();
    ensure_config_exists(&config_path)?;

    let runtime = match config::load(&config_path) {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!("{error:#}");
            std::process::exit(1);
        }
    };

    let address = format!(
        "{}:{}",
        runtime.config.server.host, runtime.config.server.port
    );
    let cache = Arc::new(CacheStore::new(
        runtime.config.cache.dir.clone(),
        runtime.config.cache.persist,
    ));
    let http = reqwest::Client::builder()
        .user_agent(concat!("Repsetarr/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .context("building the HTTP client")?;

    tracing::info!(
        version = api::VERSION,
        config = %config_path.display(),
        cache = %cache.dir().display(),
        sources = runtime.config.sources.len(),
        lists = runtime.config.lists.len(),
        "starting"
    );

    let state: SharedState = Arc::new(AppState::new(runtime, cache, http));

    // Seed from disk and fetch anything missing before answering requests.
    refresh::prime(&state).await;
    tokio::spawn(refresh::run(Arc::clone(&state)));

    // Held for as long as the process runs; dropping it stops the watch.
    let _watcher = watch_config(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("binding {address}"))?;
    tracing::info!("listening on http://{address}");

    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")?;
    Ok(())
}

fn init_tracing() {
    // LOG_LEVEL is the documented knob; RUST_LOG still wins for fine control.
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("repsetarr={level},tower_http=warn")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn config_path() -> PathBuf {
    std::env::var_os("REPSETARR_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
}

/// First boot in a fresh container has no config; write a commented starter so
/// there is something to edit instead of an error to decipher.
fn ensure_config_exists(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, STARTER_CONFIG)
        .with_context(|| format!("writing a starter config to {}", path.display()))?;
    tracing::warn!("no config found, wrote a starter one to {}", path.display());
    Ok(())
}

/// Reload when the config file changes on disk. Editors write configs by
/// replacing the file, so the containing directory is watched, not the file.
fn watch_config(state: SharedState) -> Option<RecommendedWatcher> {
    let path = state.config_path.clone();
    let directory = path.parent()?.to_path_buf();
    let filename = path.file_name()?.to_os_string();

    let (sender, mut receiver) = tokio::sync::mpsc::channel::<()>(8);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if event.kind.is_access() {
            return;
        }
        if event
            .paths
            .iter()
            .any(|changed| changed.file_name() == Some(&filename))
        {
            // Called from notify's own thread, never from a runtime worker.
            let _ = sender.blocking_send(());
        }
    })
    .map_err(|error| tracing::warn!(%error, "config watching is unavailable"))
    .ok()?;

    if let Err(error) = watcher.watch(&directory, RecursiveMode::NonRecursive) {
        tracing::warn!(%error, "could not watch {}", directory.display());
        return None;
    }

    tokio::spawn(async move {
        while receiver.recv().await.is_some() {
            // A single save can produce several events; wait for quiet.
            while tokio::time::timeout(Duration::from_millis(400), receiver.recv())
                .await
                .is_ok()
            {}
            match state.reload() {
                Ok(_) => refresh::refresh_due(&state).await,
                Err(error) => tracing::error!("config not reloaded: {error:#}"),
            }
        }
    });

    tracing::info!("watching {} for changes", path.display());
    Some(watcher)
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
}
