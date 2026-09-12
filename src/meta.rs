//! Descriptive metadata for titles whose own source did not carry it.
//!
//! Sources hand back the ids and whatever else their payload happened to
//! include. MDBList lists carry country, language and genre; Radarr, Sonarr,
//! static entries and most JSON feeds do not. This store is where the gaps get
//! filled, and it exists so that a gap is filled *once*.
//!
//! The TTL is deliberately long. A film's country of origin, its original
//! language and its genres do not change, so re-asking is pure waste; the only
//! reason there is a TTL at all is that providers do correct their own data.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Attrs, Item, ItemKey, MediaType, Provider};

const FILE_NAME: &str = "metadata.json";

/// What is known about one id at one point in time. `attrs: None` records that
/// the provider had never heard of the id - a negative result worth remembering,
/// because otherwise every pass asks about it again.
#[derive(Debug, Clone)]
pub struct MetaEntry {
    pub attrs: Option<Attrs>,
    pub checked_at: DateTime<Utc>,
}

impl MetaEntry {
    fn is_fresh(&self, ttl: Duration, miss_ttl: Duration) -> bool {
        let limit = if self.attrs.is_some() { ttl } else { miss_ttl };
        match (Utc::now() - self.checked_at).to_std() {
            Ok(age) => age <= limit,
            // A timestamp in the future: treat as fresh rather than thrash.
            Err(_) => true,
        }
    }
}

/// The on-disk shape: a flat list, because JSON object keys cannot be structs.
#[derive(Debug, Serialize, Deserialize)]
struct MetaFile {
    version: u32,
    records: Vec<MetaRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MetaRecord {
    media_type: MediaType,
    provider: Provider,
    id: String,
    checked_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attrs: Option<Attrs>,
}

#[derive(Debug)]
pub struct MetaStore {
    dir: PathBuf,
    persist: bool,
    entries: RwLock<HashMap<ItemKey, MetaEntry>>,
}

impl MetaStore {
    pub fn new(dir: impl Into<PathBuf>, persist: bool) -> Self {
        MetaStore {
            dir: dir.into(),
            persist,
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    pub fn len(&self) -> usize {
        self.entries.read().expect("metadata lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The best attributes known for an item, tried across every id it carries
    /// so a title looked up by TMDb id is still found by its IMDb id later.
    pub fn lookup(&self, item: &Item) -> Option<Attrs> {
        let entries = self.entries.read().expect("metadata lock poisoned");
        let mut merged: Option<Attrs> = None;
        for key in item.keys() {
            if let Some(Some(attrs)) = entries.get(&key).map(|entry| entry.attrs.as_ref()) {
                match &mut merged {
                    Some(merged) => merged.fill_from(attrs),
                    none => *none = Some(attrs.clone()),
                }
            }
        }
        merged
    }

    /// Whether this id has been asked about recently enough to skip.
    pub fn is_fresh(&self, key: &ItemKey, ttl: Duration, miss_ttl: Duration) -> bool {
        self.entries
            .read()
            .expect("metadata lock poisoned")
            .get(key)
            .is_some_and(|entry| entry.is_fresh(ttl, miss_ttl))
    }

    /// Record what a provider said about a title, under every id it gave back,
    /// so any of them finds it next time.
    pub fn record(&self, keys: &[ItemKey], attrs: Option<Attrs>) {
        let checked_at = Utc::now();
        let mut entries = self.entries.write().expect("metadata lock poisoned");
        for key in keys {
            entries.insert(
                key.clone(),
                MetaEntry {
                    attrs: attrs.clone(),
                    checked_at,
                },
            );
        }
    }

    pub async fn restore(&self, ttl: Duration, miss_ttl: Duration) -> Result<usize> {
        let path = self.path();
        let raw = match tokio::fs::read(&path).await {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };

        let file: MetaFile = match serde_json::from_slice(&raw) {
            Ok(file) => file,
            Err(error) => {
                // A metadata file is a cache, never a source of truth. A
                // corrupt one costs a refetch, not a failed start.
                tracing::warn!(path = %path.display(), %error, "discarding unreadable metadata cache");
                return Ok(0);
            }
        };

        let mut entries = self.entries.write().expect("metadata lock poisoned");
        for record in file.records {
            let entry = MetaEntry {
                attrs: record.attrs,
                checked_at: record.checked_at,
            };
            // Expired entries would be refetched anyway; dropping them here is
            // what keeps the file from growing without bound.
            if !entry.is_fresh(ttl, miss_ttl) {
                continue;
            }
            entries.insert(
                ItemKey {
                    media_type: record.media_type,
                    provider: record.provider,
                    value: record.id,
                },
                entry,
            );
        }
        Ok(entries.len())
    }

    pub async fn save(&self) -> Result<()> {
        if !self.persist {
            return Ok(());
        }
        let records: Vec<MetaRecord> = {
            let entries = self.entries.read().expect("metadata lock poisoned");
            entries
                .iter()
                .map(|(key, entry)| MetaRecord {
                    media_type: key.media_type,
                    provider: key.provider,
                    id: key.value.clone(),
                    checked_at: entry.checked_at,
                    attrs: entry.attrs.clone(),
                })
                .collect()
        };

        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| format!("creating cache directory {}", self.dir.display()))?;
        let path = self.path();
        let temporary = path.with_extension("json.tmp");
        let encoded = serde_json::to_vec(&MetaFile {
            version: 1,
            records,
        })
        .context("encoding metadata cache")?;
        tokio::fs::write(&temporary, &encoded)
            .await
            .with_context(|| format!("writing {}", temporary.display()))?;
        // Rename is atomic, so a crash mid-write cannot leave a half file behind.
        tokio::fs::rename(&temporary, &path)
            .await
            .with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaIds, MediaType};

    fn key(id: &str) -> ItemKey {
        ItemKey {
            media_type: MediaType::Movie,
            provider: Provider::Tmdb,
            value: id.to_string(),
        }
    }

    fn russian() -> Attrs {
        Attrs {
            country: Some("su".into()),
            original_language: Some("ru".into()),
            ..Attrs::default()
        }
    }

    const LONG: Duration = Duration::from_secs(30 * 24 * 60 * 60);
    const SHORT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    #[tokio::test]
    async fn entries_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path(), true);
        store.record(&[key("25237")], Some(russian()));
        store.record(&[key("999")], None);
        store.save().await.unwrap();

        let reopened = MetaStore::new(dir.path(), true);
        assert_eq!(reopened.restore(LONG, SHORT).await.unwrap(), 2);

        let mut item = Item::new(MediaType::Movie);
        item.ids = MediaIds {
            tmdb: Some(25237),
            ..MediaIds::default()
        };
        assert_eq!(
            reopened.lookup(&item).unwrap().country.as_deref(),
            Some("su")
        );
        // A negative entry is remembered, so the id is not asked about again.
        assert!(reopened.is_fresh(&key("999"), LONG, SHORT));
        assert!(!reopened.is_fresh(&key("nobody"), LONG, SHORT));
    }

    #[tokio::test]
    async fn expired_entries_are_dropped_on_restore() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path(), true);
        store.record(&[key("1")], Some(russian()));
        store.save().await.unwrap();

        let reopened = MetaStore::new(dir.path(), true);
        let expired = Duration::from_secs(0);
        assert_eq!(reopened.restore(expired, expired).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_missing_or_corrupt_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path(), true);
        assert_eq!(store.restore(LONG, SHORT).await.unwrap(), 0);

        tokio::fs::write(store.path(), b"{ not json").await.unwrap();
        assert_eq!(store.restore(LONG, SHORT).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn lookup_finds_a_title_by_any_of_its_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path(), false);
        let imdb = ItemKey {
            media_type: MediaType::Movie,
            provider: Provider::Imdb,
            value: "tt0091251".into(),
        };
        store.record(&[key("25237"), imdb], Some(russian()));

        let mut by_imdb = Item::new(MediaType::Movie);
        by_imdb.ids = MediaIds {
            imdb: Some("tt0091251".into()),
            ..MediaIds::default()
        };
        assert_eq!(
            store.lookup(&by_imdb).unwrap().original_language.as_deref(),
            Some("ru")
        );

        // Media type is part of the key: a show with the same TMDb number is a
        // different title.
        let mut as_show = Item::new(MediaType::Show);
        as_show.ids = MediaIds {
            tmdb: Some(25237),
            ..MediaIds::default()
        };
        assert!(store.lookup(&as_show).is_none());
    }
}
