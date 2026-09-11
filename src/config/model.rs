//! The shape of `config.yml`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveDate;
use indexmap::IndexMap;
use serde::Deserialize;

use crate::model::MediaType;

/// Keys serde did not recognize. They are collected rather than ignored so that
/// a typo in a source block is reported instead of silently doing nothing.
pub type Extra = BTreeMap<String, serde_yaml_ng::Value>;

/// The `type:` discriminator is part of the flattened content for internally
/// tagged enums, so it is never an unknown key.
pub fn unknown_keys(extra: &Extra) -> Vec<&str> {
    extra
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "type")
        .collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub providers: Providers,
    #[serde(default)]
    pub sources: IndexMap<String, SourceConfig>,
    #[serde(default)]
    pub lists: IndexMap<String, ListConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    9797
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_cache_dir")]
    pub dir: PathBuf,
    /// Used by any source that does not set its own `ttl`.
    #[serde(default = "default_cache_ttl", with = "humantime_serde")]
    pub default_ttl: Duration,
    /// How often the refresher looks for sources whose data has aged out.
    #[serde(default = "default_check_interval", with = "humantime_serde")]
    pub check_interval: Duration,
    /// Write snapshots to disk so a restart does not refetch everything.
    #[serde(default = "yes")]
    pub persist: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            dir: default_cache_dir(),
            default_ttl: default_cache_ttl(),
            check_interval: default_check_interval(),
            persist: true,
        }
    }
}

fn default_cache_dir() -> PathBuf {
    PathBuf::from("/config/cache")
}

fn default_cache_ttl() -> Duration {
    Duration::from_secs(6 * 60 * 60)
}

fn default_check_interval() -> Duration {
    Duration::from_secs(60)
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    #[serde(default)]
    pub mdblist: Option<MdblistProvider>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdblistProvider {
    pub apikey: String,
    #[serde(default = "default_mdblist_base")]
    pub base_url: String,
}

fn default_mdblist_base() -> String {
    "https://api.mdblist.com".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    Mdblist(MdblistSource),
    Static(StaticSource),
    Json(JsonSource),
    Radarr(ArrSource),
    Sonarr(ArrSource),
}

impl SourceConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            SourceConfig::Mdblist(_) => "mdblist",
            SourceConfig::Static(_) => "static",
            SourceConfig::Json(_) => "json",
            SourceConfig::Radarr(_) => "radarr",
            SourceConfig::Sonarr(_) => "sonarr",
        }
    }

    pub fn ttl(&self) -> Option<Duration> {
        match self {
            SourceConfig::Mdblist(source) => source.ttl,
            SourceConfig::Static(_) => None,
            SourceConfig::Json(source) => source.ttl,
            SourceConfig::Radarr(source) | SourceConfig::Sonarr(source) => source.ttl,
        }
    }

    /// A static source is defined entirely by the config, so there is nothing
    /// worth keeping on disk or refreshing on a timer.
    pub fn is_remote(&self) -> bool {
        !matches!(self, SourceConfig::Static(_))
    }

    pub fn extra(&self) -> &Extra {
        match self {
            SourceConfig::Mdblist(source) => &source.extra,
            SourceConfig::Static(source) => &source.extra,
            SourceConfig::Json(source) => &source.extra,
            SourceConfig::Radarr(source) | SourceConfig::Sonarr(source) => &source.extra,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MdblistSource {
    /// `username/listname` as it appears in the mdblist.com URL.
    #[serde(default)]
    pub list: Option<String>,
    /// Numeric list id, for lists addressed by id.
    #[serde(default)]
    pub list_id: Option<u64>,
    /// A pasted `https://mdblist.com/lists/username/listname` URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Overrides `providers.mdblist.apikey`.
    #[serde(default)]
    pub apikey: Option<String>,
    /// Keep only one media type from a mixed list.
    #[serde(default)]
    pub media_type: MediaTypeFilter,
    /// Stop after this many items.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default, with = "humantime_serde")]
    pub ttl: Option<Duration>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StaticSource {
    /// `provider:id` specs, e.g. `tmdb:550`, `imdb:tt0133093`, `tvdb:81189`.
    #[serde(default)]
    pub items: Vec<String>,
    /// Media type of every entry in this block.
    #[serde(default)]
    pub media_type: MediaType,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonSource {
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Dotted path to the array inside the response, e.g. `data.items`.
    /// Omit when the response is an array already.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub fields: FieldMap,
    /// Media type for entries whose type field is missing or unmapped.
    #[serde(default)]
    pub media_type: MediaType,
    #[serde(default, with = "humantime_serde")]
    pub ttl: Option<Duration>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Which keys of a JSON object hold which id. Each entry is a list of
/// candidate key names tried in order; leaving one empty uses the defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldMap {
    #[serde(default, deserialize_with = "one_or_many")]
    pub tmdb: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub imdb: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub tvdb: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub trakt: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub title: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub year: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub media_type: Vec<String>,
}

impl FieldMap {
    pub fn tmdb_keys(&self) -> Vec<&str> {
        pick(&self.tmdb, &["tmdbId", "tmdb_id", "tmdb"])
    }
    pub fn imdb_keys(&self) -> Vec<&str> {
        pick(&self.imdb, &["imdbId", "imdb_id", "imdb"])
    }
    pub fn tvdb_keys(&self) -> Vec<&str> {
        pick(&self.tvdb, &["tvdbId", "tvdb_id", "tvdb"])
    }
    pub fn trakt_keys(&self) -> Vec<&str> {
        pick(&self.trakt, &["traktId", "trakt_id", "trakt"])
    }
    pub fn title_keys(&self) -> Vec<&str> {
        pick(&self.title, &["title", "name"])
    }
    pub fn year_keys(&self) -> Vec<&str> {
        pick(&self.year, &["year", "release_year", "releaseYear"])
    }
    pub fn media_type_keys(&self) -> Vec<&str> {
        pick(&self.media_type, &["mediatype", "media_type", "type"])
    }
}

/// Configured key names win; an unconfigured field falls back to the spellings
/// these feeds commonly use.
fn pick<'a>(configured: &'a [String], defaults: &[&'a str]) -> Vec<&'a str> {
    if configured.is_empty() {
        defaults.to_vec()
    } else {
        configured.iter().map(String::as_str).collect()
    }
}

fn one_or_many<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(value) => vec![value],
        OneOrMany::Many(values) => values,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArrSource {
    /// Base URL of the instance, e.g. `http://radarr:7878`.
    pub url: String,
    pub api_key: String,
    /// Only report items the instance is monitoring.
    #[serde(default)]
    pub monitored_only: bool,
    #[serde(default, with = "humantime_serde")]
    pub ttl: Option<Duration>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaTypeFilter {
    #[default]
    Any,
    Movie,
    Show,
}

impl MediaTypeFilter {
    pub fn matches(self, media_type: MediaType) -> bool {
        match self {
            MediaTypeFilter::Any => true,
            MediaTypeFilter::Movie => media_type == MediaType::Movie,
            MediaTypeFilter::Show => media_type == MediaType::Show,
        }
    }

    pub fn as_media_type(self) -> Option<MediaType> {
        match self {
            MediaTypeFilter::Any => None,
            MediaTypeFilter::Movie => Some(MediaType::Movie),
            MediaTypeFilter::Show => Some(MediaType::Show),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListConfig {
    /// The set expression, e.g. `(trending | top250) - my_radarr`.
    pub expr: String,
    #[serde(default)]
    pub media_type: MediaTypeFilter,
    #[serde(default)]
    pub min_year: Option<i32>,
    #[serde(default)]
    pub max_year: Option<i32>,
    #[serde(default)]
    pub released_after: Option<NaiveDate>,
    #[serde(default)]
    pub released_before: Option<NaiveDate>,
    #[serde(default)]
    pub sort: SortKey,
    #[serde(default)]
    pub order: SortOrder,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub kometa: KometaConfig,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// Keep the order the algebra produced.
    #[default]
    None,
    /// The rank the source gave the item, unranked items last.
    Rank,
    Title,
    Year,
    Released,
    Random,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KometaConfig {
    /// Collection name in the generated file; defaults to the list name.
    #[serde(default)]
    pub collection: Option<String>,
    #[serde(default = "default_sync_mode")]
    pub sync_mode: String,
    #[serde(default = "default_collection_order")]
    pub collection_order: String,
    /// Copied verbatim into the collection block.
    #[serde(default)]
    pub extra: BTreeMap<String, serde_yaml_ng::Value>,
}

impl Default for KometaConfig {
    fn default() -> Self {
        KometaConfig {
            collection: None,
            sync_mode: default_sync_mode(),
            collection_order: default_collection_order(),
            extra: BTreeMap::new(),
        }
    }
}

fn default_sync_mode() -> String {
    "sync".to_string()
}

fn default_collection_order() -> String {
    "custom".to_string()
}
