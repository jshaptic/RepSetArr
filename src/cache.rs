//! Per-source snapshots, in memory and on disk.
//!
//! Radarr and Kometa poll on their own schedule and expect an answer straight
//! away, while MDBList has a request quota and can be down. So nothing is ever
//! fetched during a request: a background refresher keeps snapshots warm, and a
//! failed refresh leaves the last good snapshot in place, marked stale.

use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::Item;

/// What is persisted per source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub fetched_at: DateTime<Utc>,
    /// Identifies the source configuration that produced this snapshot, so a
    /// changed config never serves data fetched under the old one.
    pub fingerprint: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default)]
pub struct SourceStatus {
    pub items: Option<Arc<Vec<Item>>>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub fingerprint: String,
}

impl SourceStatus {
    pub fn item_count(&self) -> Option<usize> {
        self.items.as_ref().map(|items| items.len())
    }

    pub fn age(&self) -> Option<Duration> {
        let fetched_at = self.fetched_at?;
        (Utc::now() - fetched_at).to_std().ok()
    }

    /// Older than its TTL - still served, but the refresher wants another go.
    pub fn is_stale(&self, ttl: Duration) -> bool {
        match self.age() {
            Some(age) => age > ttl,
            None => true,
        }
    }
}

#[derive(Debug)]
pub struct CacheStore {
    dir: PathBuf,
    persist: bool,
    entries: RwLock<HashMap<String, SourceStatus>>,
}

impl CacheStore {
    pub fn new(dir: impl Into<PathBuf>, persist: bool) -> Self {
        CacheStore {
            dir: dir.into(),
            persist,
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub fn items(&self, name: &str) -> Option<Arc<Vec<Item>>> {
        self.entries
            .read()
            .expect("cache lock poisoned")
            .get(name)
            .and_then(|status| status.items.clone())
    }

    pub fn status(&self, name: &str) -> SourceStatus {
        self.entries
            .read()
            .expect("cache lock poisoned")
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    pub fn statuses(&self) -> BTreeMap<String, SourceStatus> {
        self.entries
            .read()
            .expect("cache lock poisoned")
            .iter()
            .map(|(name, status)| (name.clone(), status.clone()))
            .collect()
    }

    /// Record a successful fetch, and persist it if persistence is enabled.
    pub async fn store(&self, name: &str, fingerprint: &str, items: Vec<Item>) {
        let now = Utc::now();
        let items = Arc::new(items);
        {
            let mut entries = self.entries.write().expect("cache lock poisoned");
            let entry = entries.entry(name.to_string()).or_default();
            entry.items = Some(items.clone());
            entry.fetched_at = Some(now);
            entry.last_attempt = Some(now);
            entry.last_error = None;
            entry.fingerprint = fingerprint.to_string();
        }

        if self.persist
            && let Err(error) = self
                .write_snapshot(
                    name,
                    &Snapshot {
                        fetched_at: now,
                        fingerprint: fingerprint.to_string(),
                        items: items.as_ref().clone(),
                    },
                )
                .await
        {
            tracing::warn!(
                source = name,
                error = format!("{error:#}"),
                "could not persist snapshot"
            );
        }
    }

    /// Record a failed fetch. Any previous snapshot stays in place.
    pub fn record_failure(&self, name: &str, error: &str) {
        let mut entries = self.entries.write().expect("cache lock poisoned");
        let entry = entries.entry(name.to_string()).or_default();
        entry.last_attempt = Some(Utc::now());
        entry.last_error = Some(error.to_string());
    }

    /// Seed a source from its persisted snapshot. A snapshot whose fingerprint
    /// no longer matches the configuration is ignored.
    pub async fn restore(&self, name: &str, fingerprint: &str) -> bool {
        if !self.persist {
            return false;
        }
        let path = self.path_for(name);
        let Ok(raw) = tokio::fs::read(&path).await else {
            return false;
        };
        let snapshot: Snapshot = match serde_json::from_slice(&raw) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(source = name, %error, "ignoring unreadable snapshot");
                return false;
            }
        };
        if snapshot.fingerprint != fingerprint {
            tracing::info!(
                source = name,
                "configuration changed, ignoring stored snapshot"
            );
            return false;
        }

        let mut entries = self.entries.write().expect("cache lock poisoned");
        let entry = entries.entry(name.to_string()).or_default();
        entry.items = Some(Arc::new(snapshot.items));
        entry.fetched_at = Some(snapshot.fetched_at);
        entry.fingerprint = snapshot.fingerprint;
        true
    }

    /// Drop in-memory entries for sources that no longer exist, after a reload.
    pub fn retain(&self, keep: &dyn Fn(&str) -> bool) {
        self.entries
            .write()
            .expect("cache lock poisoned")
            .retain(|name, _| keep(name));
    }

    async fn write_snapshot(&self, name: &str, snapshot: &Snapshot) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| format!("creating cache directory {}", self.dir.display()))?;
        let path = self.path_for(name);
        let temporary = path.with_extension("json.tmp");
        let encoded = serde_json::to_vec(snapshot).context("encoding snapshot")?;
        tokio::fs::write(&temporary, &encoded)
            .await
            .with_context(|| format!("writing {}", temporary.display()))?;
        // Rename is atomic, so a crash mid-write cannot leave a half file behind.
        tokio::fs::rename(&temporary, &path)
            .await
            .with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    pub fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", file_stem(name)))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Source names may contain anything; the hash suffix keeps two names that
/// sanitize to the same string apart.
fn file_stem(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{sanitized}-{}", short_hash(name))
}

pub fn short_hash(value: impl Hash) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

/// A source's configuration reduced to a comparable string.
pub fn fingerprint(source: &crate::config::SourceConfig) -> String {
    short_hash(format!("{source:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaIds, MediaType};

    fn item(tmdb: u32) -> Item {
        Item {
            media_type: MediaType::Movie,
            ids: MediaIds {
                tmdb: Some(tmdb),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn snapshots_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::new(dir.path(), true);
        store.store("trending", "fp1", vec![item(550)]).await;
        assert_eq!(store.items("trending").unwrap().len(), 1);

        let reopened = CacheStore::new(dir.path(), true);
        assert!(reopened.restore("trending", "fp1").await);
        assert_eq!(reopened.items("trending").unwrap()[0].ids.tmdb, Some(550));
    }

    #[tokio::test]
    async fn a_changed_configuration_invalidates_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::new(dir.path(), true);
        store.store("trending", "fp1", vec![item(550)]).await;

        let reopened = CacheStore::new(dir.path(), true);
        assert!(!reopened.restore("trending", "fp2").await);
        assert!(reopened.items("trending").is_none());
    }

    #[tokio::test]
    async fn a_failure_keeps_the_previous_data_and_records_the_error() {
        let store = CacheStore::new("/nonexistent", false);
        store.store("trending", "fp1", vec![item(550)]).await;
        store.record_failure("trending", "connection refused");

        let status = store.status("trending");
        assert_eq!(status.item_count(), Some(1));
        assert_eq!(status.last_error.as_deref(), Some("connection refused"));
        assert!(status.fetched_at.is_some());
    }

    #[test]
    fn an_unfetched_source_is_stale_and_empty() {
        let store = CacheStore::new("/nonexistent", false);
        let status = store.status("never-fetched");
        assert!(status.is_stale(Duration::from_secs(3600)));
        assert_eq!(status.item_count(), None);
    }

    #[test]
    fn file_names_are_safe_and_unique() {
        assert_ne!(file_stem("a/b"), file_stem("a_b"));
        assert!(file_stem("a/b").starts_with("a_b-"));
    }
}
