//! Shared application state and configuration reloading.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};

use crate::cache::CacheStore;
use crate::config::{self, Runtime};
use crate::meta::MetaStore;

pub type SharedState = Arc<AppState>;

pub struct AppState {
    runtime: ArcSwap<Runtime>,
    pub cache: Arc<CacheStore>,
    pub meta: Arc<MetaStore>,
    pub http: reqwest::Client,
    pub config_path: PathBuf,
    pub started_at: DateTime<Utc>,
}

impl AppState {
    pub fn new(
        runtime: Runtime,
        cache: Arc<CacheStore>,
        meta: Arc<MetaStore>,
        http: reqwest::Client,
    ) -> Self {
        AppState {
            config_path: runtime.path.clone(),
            runtime: ArcSwap::from_pointee(runtime),
            cache,
            meta,
            http,
            started_at: Utc::now(),
        }
    }

    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime.load_full()
    }

    /// Re-read the config file. The running configuration is only replaced once
    /// the new one has parsed and validated, so a broken edit keeps serving.
    pub fn reload(&self) -> Result<Arc<Runtime>> {
        let previous = self.runtime();
        let next = config::load(&self.config_path)?;

        if next.config.cache.dir != previous.config.cache.dir {
            tracing::warn!(
                "cache.dir changed to {} but the cache is already open at {}; restart to apply",
                next.config.cache.dir.display(),
                self.cache.dir().display()
            );
        }
        if next.config.server.port != previous.config.server.port
            || next.config.server.host != previous.config.server.host
        {
            tracing::warn!("server.host/port changed; restart to apply");
        }

        // Forget sources that no longer exist so they stop showing up in health.
        let keep: Vec<String> = next.config.sources.keys().cloned().collect();
        self.cache
            .retain(&|name: &str| keep.iter().any(|kept| kept == name));

        let next = Arc::new(next);
        self.runtime.store(next.clone());
        tracing::info!(
            sources = next.config.sources.len(),
            lists = next.config.lists.len(),
            "configuration reloaded"
        );
        Ok(next)
    }

    pub fn uptime_seconds(&self) -> i64 {
        (Utc::now() - self.started_at).num_seconds()
    }
}
