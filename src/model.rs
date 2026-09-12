//! Normalized media items and the external ids used to identify them.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    #[default]
    Movie,
    Show,
}

impl MediaType {
    /// Lenient parsing of the many spellings upstream providers use.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "movie" | "movies" | "film" | "feature" => Some(MediaType::Movie),
            "show" | "shows" | "tv" | "series" | "season" | "episode" => Some(MediaType::Show),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::Movie => "movie",
            MediaType::Show => "show",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Tmdb,
    Imdb,
    Tvdb,
    Trakt,
}

impl Provider {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "tmdb" => Some(Provider::Tmdb),
            "imdb" => Some(Provider::Imdb),
            "tvdb" | "thetvdb" => Some(Provider::Tvdb),
            "trakt" => Some(Provider::Trakt),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Tmdb => "tmdb",
            Provider::Imdb => "imdb",
            Provider::Tvdb => "tvdb",
            Provider::Trakt => "trakt",
        }
    }
}

/// Every external id known for one item. All fields are optional; an item with
/// no ids at all cannot be matched against anything and is dropped on ingest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trakt: Option<u32>,
}

impl MediaIds {
    pub fn is_empty(&self) -> bool {
        self.tmdb.is_none() && self.imdb.is_none() && self.tvdb.is_none() && self.trakt.is_none()
    }

    /// The identity keys this item contributes, as `(provider, value)` pairs.
    pub fn keys(&self) -> Vec<(Provider, String)> {
        let mut out = Vec::with_capacity(4);
        if let Some(id) = self.tmdb {
            out.push((Provider::Tmdb, id.to_string()));
        }
        if let Some(id) = &self.imdb {
            out.push((Provider::Imdb, id.clone()));
        }
        if let Some(id) = self.tvdb {
            out.push((Provider::Tvdb, id.to_string()));
        }
        if let Some(id) = self.trakt {
            out.push((Provider::Trakt, id.to_string()));
        }
        out
    }

    /// Fill in whatever this set of ids is missing from `other`. Existing values win.
    pub fn fill_from(&mut self, other: &MediaIds) {
        if self.tmdb.is_none() {
            self.tmdb = other.tmdb;
        }
        if self.imdb.is_none() {
            self.imdb = other.imdb.clone();
        }
        if self.tvdb.is_none() {
            self.tvdb = other.tvdb;
        }
        if self.trakt.is_none() {
            self.trakt = other.trakt;
        }
    }

    pub fn set(&mut self, provider: Provider, value: &str) -> Result<(), String> {
        match provider {
            Provider::Imdb => {
                self.imdb = Some(
                    normalize_imdb(value).ok_or_else(|| format!("invalid imdb id `{value}`"))?,
                )
            }
            Provider::Tmdb | Provider::Tvdb | Provider::Trakt => {
                let n: u32 = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid {} id `{value}`", provider.as_str()))?;
                match provider {
                    Provider::Tmdb => self.tmdb = Some(n),
                    Provider::Tvdb => self.tvdb = Some(n),
                    Provider::Trakt => self.trakt = Some(n),
                    Provider::Imdb => unreachable!(),
                }
            }
        }
        Ok(())
    }
}

/// `tt0137523`, `0137523` and `137523` all denote the same IMDb title; store the
/// canonical `tt`-prefixed, zero-padded form so string comparison is meaningful.
pub fn normalize_imdb(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let digits = trimmed.strip_prefix("tt").unwrap_or(&trimmed);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("tt{digits:0>7}"))
}

/// Descriptive metadata used by filters. Every field is optional: sources
/// populate what their payload happens to carry, and the enricher fills gaps
/// later. Absent means "not known", never "not applicable".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attrs {
    /// ISO-3166-1 alpha-2, lowercased. Upstreams report a single country even
    /// for co-productions, so this is "a" country, not "the" country.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// ISO-639-1, lowercased.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken_language: Option<String>,
    /// Lowercased genre names, deduplicated, in the order the source gave them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    /// Minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<u32>,
    /// Certification as the upstream spells it, uppercased (`PG`, `R`, `TV-MA`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_rating: Option<String>,
    /// Release status, lowercased (`released`, `in production`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl Attrs {
    pub fn is_empty(&self) -> bool {
        self.country.is_none()
            && self.original_language.is_none()
            && self.spoken_language.is_none()
            && self.genres.is_empty()
            && self.runtime.is_none()
            && self.content_rating.is_none()
            && self.status.is_none()
    }

    /// Fill in whatever this set of attributes is missing from `other`.
    /// Existing values win, matching [`MediaIds::fill_from`].
    pub fn fill_from(&mut self, other: &Attrs) {
        if self.country.is_none() {
            self.country = other.country.clone();
        }
        if self.original_language.is_none() {
            self.original_language = other.original_language.clone();
        }
        if self.spoken_language.is_none() {
            self.spoken_language = other.spoken_language.clone();
        }
        if self.genres.is_empty() {
            self.genres = other.genres.clone();
        }
        if self.runtime.is_none() {
            self.runtime = other.runtime;
        }
        if self.content_rating.is_none() {
            self.content_rating = other.content_rating.clone();
        }
        if self.status.is_none() {
            self.status = other.status.clone();
        }
    }
}

/// Lowercase and trim a free-form code (country, language, genre); empty becomes
/// `None` so a source sending `""` does not look like knowledge we do not have.
pub fn normalize_code(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// One movie or show, normalized away from whatever the source called things.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub media_type: MediaType,
    #[serde(default)]
    pub ids: MediaIds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<NaiveDate>,
    /// Position within the source list, if the source has a meaningful order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    #[serde(default, skip_serializing_if = "Attrs::is_empty")]
    pub attrs: Attrs,
}

impl Item {
    pub fn new(media_type: MediaType) -> Self {
        Item {
            media_type,
            ..Default::default()
        }
    }

    /// Identity keys, scoped by media type so TMDb movie 550 never collides
    /// with TMDb show 550.
    pub fn keys(&self) -> Vec<ItemKey> {
        self.ids
            .keys()
            .into_iter()
            .map(|(provider, value)| ItemKey {
                media_type: self.media_type,
                provider,
                value,
            })
            .collect()
    }

    /// Absorb anything `other` knows that this item does not. Existing values win,
    /// so the leftmost source in an expression decides the title shown.
    pub fn merge_from(&mut self, other: &Item) {
        self.ids.fill_from(&other.ids);
        if self.title.is_none() {
            self.title = other.title.clone();
        }
        if self.year.is_none() {
            self.year = other.year;
        }
        if self.released.is_none() {
            self.released = other.released;
        }
        if self.rank.is_none() {
            self.rank = other.rank;
        }
        self.attrs.fill_from(&other.attrs);
    }

    /// Best-effort release year: the explicit year, else the year of the release date.
    pub fn effective_year(&self) -> Option<i32> {
        use chrono::Datelike;
        self.year.or_else(|| self.released.map(|d| d.year()))
    }

    pub fn sort_title(&self) -> String {
        self.title
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase()
            .trim_start_matches("the ")
            .trim_start_matches("a ")
            .trim_start_matches("an ")
            .to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemKey {
    pub media_type: MediaType,
    pub provider: Provider,
    pub value: String,
}

/// Parse a `provider:id` spec as written in a static source, e.g. `tmdb:550`.
pub fn parse_id_spec(spec: &str) -> Result<MediaIds, String> {
    let (provider, value) = spec
        .split_once(':')
        .ok_or_else(|| format!("`{spec}` is not a `provider:id` pair (e.g. `tmdb:550`)"))?;
    let provider = Provider::parse(provider)
        .ok_or_else(|| format!("unknown id provider `{provider}` in `{spec}`"))?;
    let mut ids = MediaIds::default();
    ids.set(provider, value)?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdb_ids_normalize_to_a_canonical_form() {
        assert_eq!(normalize_imdb("tt0137523").as_deref(), Some("tt0137523"));
        assert_eq!(normalize_imdb("TT0137523").as_deref(), Some("tt0137523"));
        assert_eq!(normalize_imdb("137523").as_deref(), Some("tt0137523"));
        assert_eq!(
            normalize_imdb(" tt12345678 ").as_deref(),
            Some("tt12345678")
        );
        assert_eq!(normalize_imdb("nope"), None);
        assert_eq!(normalize_imdb(""), None);
    }

    #[test]
    fn keys_are_scoped_by_media_type() {
        let mut movie = Item::new(MediaType::Movie);
        movie.ids.tmdb = Some(550);
        let mut show = Item::new(MediaType::Show);
        show.ids.tmdb = Some(550);
        assert_ne!(movie.keys(), show.keys());
    }

    #[test]
    fn merge_keeps_existing_values_and_fills_the_gaps() {
        let mut a = Item::new(MediaType::Movie);
        a.ids.tmdb = Some(550);
        a.title = Some("Fight Club".into());
        let mut b = Item::new(MediaType::Movie);
        b.ids.imdb = Some("tt0137523".into());
        b.title = Some("Le Cercle des combattants".into());
        b.year = Some(1999);
        a.merge_from(&b);
        assert_eq!(a.ids.tmdb, Some(550));
        assert_eq!(a.ids.imdb.as_deref(), Some("tt0137523"));
        assert_eq!(a.title.as_deref(), Some("Fight Club"));
        assert_eq!(a.year, Some(1999));
    }

    #[test]
    fn id_specs_parse_and_reject() {
        assert_eq!(parse_id_spec("tmdb:550").unwrap().tmdb, Some(550));
        assert_eq!(
            parse_id_spec("imdb:137523").unwrap().imdb.as_deref(),
            Some("tt0137523")
        );
        assert!(parse_id_spec("550").is_err());
        assert!(parse_id_spec("nope:550").is_err());
        assert!(parse_id_spec("tmdb:abc").is_err());
    }
}
