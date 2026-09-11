//! What a Radarr or Sonarr instance already has, as a set.
//!
//! This is what makes "everything trending that I do not already have" a
//! one-line expression.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::json_value::{as_date, as_i32, as_str, as_u32};
use crate::config::ArrSource;
use crate::model::{Item, MediaType, normalize_imdb};

pub async fn fetch(
    http: &reqwest::Client,
    source: &ArrSource,
    media_type: MediaType,
) -> Result<Vec<Item>> {
    let resource = match media_type {
        MediaType::Movie => "movie",
        MediaType::Show => "series",
    };
    let url = format!("{}/api/v3/{resource}", source.url.trim_end_matches('/'));

    let response = http
        .get(&url)
        .header("X-Api-Key", &source.api_key)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    let status = response.status();
    let body = response.text().await.context("reading response")?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("{url} rejected the API key");
    }
    if !status.is_success() {
        bail!("{url} returned {status}");
    }

    let document: Value =
        serde_json::from_str(&body).with_context(|| format!("parsing JSON from {url}"))?;
    parse(&document, source, media_type)
}

pub fn parse(document: &Value, source: &ArrSource, media_type: MediaType) -> Result<Vec<Item>> {
    let Some(entries) = document.as_array() else {
        bail!("expected a JSON array of {} resources", media_type.as_str());
    };

    let items = entries
        .iter()
        .filter(|entry| {
            !source.monitored_only || entry.get("monitored").and_then(Value::as_bool) == Some(true)
        })
        .filter_map(|entry| {
            let mut item = Item::new(media_type);
            item.ids.tmdb = as_u32(entry.get("tmdbId"));
            item.ids.imdb = as_str(entry.get("imdbId")).and_then(normalize_imdb);
            item.ids.tvdb = as_u32(entry.get("tvdbId"));
            if item.ids.is_empty() {
                return None;
            }
            item.title = as_str(entry.get("title")).map(str::to_string);
            item.year = as_i32(entry.get("year"));
            item.released =
                as_date(entry.get("inCinemas")).or_else(|| as_date(entry.get("firstAired")));
            Some(item)
        })
        .collect();
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(monitored_only: bool) -> ArrSource {
        ArrSource {
            url: "http://radarr:7878".into(),
            api_key: "key".into(),
            monitored_only,
            ttl: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn radarr_movies_parse() {
        let document = json!([
            {"title": "Fight Club", "year": 1999, "tmdbId": 550, "imdbId": "tt0137523",
             "monitored": true, "inCinemas": "1999-09-10T00:00:00Z"},
            {"title": "Untracked", "year": 2020, "tmdbId": 1, "monitored": false}
        ]);
        let items = parse(&document, &source(false), MediaType::Movie).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].ids.tmdb, Some(550));
        assert_eq!(items[0].ids.imdb.as_deref(), Some("tt0137523"));
        assert_eq!(items[0].released.unwrap().to_string(), "1999-09-10");
    }

    #[test]
    fn monitored_only_filters() {
        let document = json!([
            {"tmdbId": 550, "monitored": true},
            {"tmdbId": 1, "monitored": false}
        ]);
        let items = parse(&document, &source(true), MediaType::Movie).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].ids.tmdb, Some(550));
    }

    #[test]
    fn sonarr_series_carry_tvdb_ids() {
        let document = json!([
            {"title": "Breaking Bad", "year": 2008, "tvdbId": 81189, "tmdbId": 1396,
             "imdbId": "tt0903747", "monitored": true, "firstAired": "2008-01-20T00:00:00Z"}
        ]);
        let items = parse(&document, &source(false), MediaType::Show).unwrap();
        assert_eq!(items[0].media_type, MediaType::Show);
        assert_eq!(items[0].ids.tvdb, Some(81189));
        assert_eq!(items[0].released.unwrap().to_string(), "2008-01-20");
    }
}
