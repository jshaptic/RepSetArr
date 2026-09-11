//! Keeping source snapshots warm.
//!
//! Nothing is fetched while serving a request. One supervisor task ticks on
//! `cache.check_interval`, reads whatever configuration is current, and
//! refreshes the sources whose data has aged out or whose configuration
//! changed. A failure is recorded and the previous snapshot keeps serving.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;

use crate::cache::{self, CacheStore};
use crate::config::Runtime;
use crate::sources;
use crate::state::SharedState;

/// Fetch one source and store the result. Errors are recorded, not propagated.
pub async fn refresh_one(
    runtime: &Runtime,
    cache: &CacheStore,
    http: &reqwest::Client,
    name: &str,
) {
    let Some(source) = runtime.source(name) else {
        return;
    };
    let fingerprint = cache::fingerprint(source);

    match sources::fetch(runtime, name, http).await {
        Ok(items) => {
            tracing::info!(source = name, items = items.len(), "refreshed");
            cache.store(name, &fingerprint, items).await;
        }
        Err(error) => {
            let message = format!("{error:#}");
            tracing::warn!(source = name, error = %message, "refresh failed");
            cache.record_failure(name, &message);
        }
    }
}

fn due(runtime: &Runtime, cache: &CacheStore, name: &str) -> bool {
    let Some(source) = runtime.source(name) else {
        return false;
    };
    let status = cache.status(name);
    if status.items.is_none() {
        return true;
    }
    if status.fingerprint != cache::fingerprint(source) {
        return true;
    }
    status.is_stale(runtime.ttl_for(name))
}

/// Seed everything from disk, then fetch whatever is missing or stale.
pub async fn prime(state: &SharedState) {
    let runtime = state.runtime();
    for (name, source) in &runtime.config.sources {
        if source.is_remote() {
            state.cache.restore(name, &cache::fingerprint(source)).await;
        }
    }
    refresh_due(state).await;
}

/// Refresh every source that is due, a few at a time.
pub async fn refresh_due(state: &SharedState) {
    let runtime = state.runtime();
    let names: Vec<String> = runtime
        .config
        .sources
        .keys()
        .filter(|name| due(&runtime, &state.cache, name))
        .cloned()
        .collect();
    if names.is_empty() {
        return;
    }

    const CONCURRENCY: usize = 4;
    let mut tasks = JoinSet::new();
    let mut queue = names.into_iter();
    let mut running = 0;

    loop {
        while running < CONCURRENCY
            && let Some(name) = queue.next()
        {
            let runtime = Arc::clone(&runtime);
            let cache = Arc::clone(&state.cache);
            let http = state.http.clone();
            tasks.spawn(async move {
                refresh_one(&runtime, &cache, &http, &name).await;
            });
            running += 1;
        }
        if tasks.join_next().await.is_none() {
            break;
        }
        running -= 1;
    }
}

/// The supervisor loop. Runs until the process exits.
pub async fn run(state: SharedState) {
    loop {
        let interval = state.runtime().config.cache.check_interval;
        tokio::time::sleep(interval.max(Duration::from_secs(1))).await;
        refresh_due(&state).await;
    }
}
