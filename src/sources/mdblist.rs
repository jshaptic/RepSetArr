//! MDBList lists, via `GET /lists/{…}/items`.
//!
//! The API pages with `limit`/`offset` and reports `X-Has-More`. Items come back
//! split into `movies` and `shows` (a flat array when `unified=true` is used),
//! and carry TMDb, IMDb and TVDb ids in both a flat and a nested form.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use super::json_value::{as_date, as_i32, as_str, as_u32};
use crate::config::{MdblistSource, MediaTypeFilter};
use crate::model::{Item, MediaType, normalize_imdb};

const PAGE_SIZE: usize = 1000;
const MAX_PAGES: usize = 100;

/// Where a list lives, resolved from `list`, `list_id` or a pasted URL.
#[derive(Debug, PartialEq, Eq)]
enum Address {
    Named { user: String, list: String },
    Id(u64),
}

impl Address {
    fn path(&self) -> String {
        match self {
            Address::Named { user, list } => format!("lists/{user}/{list}/items"),
            Address::Id(id) => format!("lists/{id}/items"),
        }
    }
}

fn address(source: &MdblistSource) -> Result<Address> {
    if let Some(id) = source.list_id {
        return Ok(Address::Id(id));
    }
    if let Some(list) = &source.list {
        let (user, name) = list
            .trim_matches('/')
            .split_once('/')
            .with_context(|| format!("`{list}` is not `username/listname`"))?;
        return Ok(Address::Named {
            user: user.to_string(),
            list: name.to_string(),
        });
    }
    if let Some(url) = &source.url {
        return address_from_url(url);
    }
    bail!("no `list`, `list_id` or `url` configured")
}

/// Accepts `https://mdblist.com/lists/user/name`, with or without a trailing
/// `/json`, and the numeric `?list=` form.
fn address_from_url(url: &str) -> Result<Address> {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let segments: Vec<&str> = without_query
        .trim_end_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let position = segments
        .iter()
        .position(|segment| *segment == "lists")
        .with_context(|| format!("`{url}` does not look like an MDBList list URL"))?;
    let rest: Vec<&str> = segments[position + 1..]
        .iter()
        .copied()
        .filter(|segment| !matches!(*segment, "json" | "items"))
        .collect();
    match rest.as_slice() {
        [id] => id.parse::<u64>().map(Address::Id).with_context(|| {
            format!("`{url}` has no username/listname and `{id}` is not a list id")
        }),
        [user, list, ..] => Ok(Address::Named {
            user: (*user).to_string(),
            list: (*list).to_string(),
        }),
        _ => bail!("`{url}` does not name a list"),
    }
}

pub async fn fetch(
    http: &reqwest::Client,
    base_url: &str,
    apikey: &str,
    source: &MdblistSource,
) -> Result<Vec<Item>> {
    let address = address(source)?;
    let url = format!("{}/{}", base_url.trim_end_matches('/'), address.path());
    let wanted = source.limit.unwrap_or(usize::MAX);

    let mut items: Vec<Item> = Vec::new();
    let mut offset = 0usize;

    for _ in 0..MAX_PAGES {
        let page_size = PAGE_SIZE.min(wanted.saturating_sub(items.len()));
        if page_size == 0 {
            break;
        }

        let response = http
            .get(&url)
            .query(&[
                ("apikey", apikey.to_string()),
                ("limit", page_size.to_string()),
                ("offset", offset.to_string()),
            ])
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?;

        let status = response.status();
        let has_more = response
            .headers()
            .get("X-Has-More")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("true"));
        let body = response.text().await.context("reading MDBList response")?;

        if !status.is_success() {
            bail!(
                "MDBList returned {status} for {url}: {}",
                truncate(&body, 300)
            );
        }

        let page: Page = serde_json::from_str(&body)
            .with_context(|| format!("parsing MDBList response: {}", truncate(&body, 300)))?;
        let raw = page.into_items()?;
        let count = raw.len();
        let base = items.len();
        let converted: Vec<Item> = raw
            .iter()
            .enumerate()
            .filter_map(|(index, (media_type, value))| convert(*media_type, value, base + index))
            .collect();
        items.extend(converted);

        if count < page_size || has_more == Some(false) {
            break;
        }
        offset += count;
    }

    if source.media_type != MediaTypeFilter::Any {
        items.retain(|item| source.media_type.matches(item.media_type));
    }
    Ok(items)
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Page {
    /// MDBList reports failures as a JSON body with a 200 in some cases.
    Error {
        error: String,
    },
    Split {
        #[serde(default)]
        movies: Vec<Value>,
        #[serde(default)]
        shows: Vec<Value>,
    },
    Flat(Vec<Value>),
}

impl Page {
    /// Flattens to `(media type hint, raw item)` pairs; the hint is the array
    /// the item came from, which an explicit `mediatype` field overrides.
    fn into_items(self) -> Result<Vec<(Option<MediaType>, Value)>> {
        match self {
            Page::Error { error } => bail!("MDBList returned an error: {error}"),
            Page::Split { movies, shows } => Ok(movies
                .into_iter()
                .map(|value| (Some(MediaType::Movie), value))
                .chain(
                    shows
                        .into_iter()
                        .map(|value| (Some(MediaType::Show), value)),
                )
                .collect()),
            Page::Flat(values) => Ok(values.into_iter().map(|value| (None, value)).collect()),
        }
    }
}

fn convert(hint: Option<MediaType>, raw: &Value, position: usize) -> Option<Item> {
    let media_type = as_str(raw.get("mediatype"))
        .and_then(MediaType::parse)
        .or(hint)?;

    let nested = raw.get("ids");
    let mut item = Item::new(media_type);
    item.ids.tmdb =
        as_u32(nested.and_then(|ids| ids.get("tmdb"))).or_else(|| as_u32(raw.get("id")));
    item.ids.imdb = as_str(nested.and_then(|ids| ids.get("imdb")))
        .or_else(|| as_str(raw.get("imdb_id")))
        .and_then(normalize_imdb);
    item.ids.tvdb =
        as_u32(nested.and_then(|ids| ids.get("tvdb"))).or_else(|| as_u32(raw.get("tvdb_id")));
    item.ids.trakt =
        as_u32(nested.and_then(|ids| ids.get("trakt"))).or_else(|| as_u32(raw.get("traktid")));

    if item.ids.is_empty() {
        return None;
    }

    item.title = as_str(raw.get("title")).map(str::to_string);
    item.year = as_i32(raw.get("release_year")).or_else(|| as_i32(raw.get("year")));
    item.released = as_date(raw.get("released"));
    item.rank = as_u32(raw.get("rank")).or(Some(position as u32 + 1));
    Some(item)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> MdblistSource {
        MdblistSource {
            list: None,
            list_id: None,
            url: None,
            apikey: None,
            media_type: MediaTypeFilter::Any,
            limit: None,
            ttl: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn lists_are_addressed_by_name_id_or_url() {
        let named = MdblistSource {
            list: Some("garycrawfordgc/latest-tv-shows".into()),
            ..source()
        };
        assert_eq!(
            address(&named).unwrap().path(),
            "lists/garycrawfordgc/latest-tv-shows/items"
        );

        let by_id = MdblistSource {
            list_id: Some(4321),
            ..source()
        };
        assert_eq!(address(&by_id).unwrap().path(), "lists/4321/items");

        for url in [
            "https://mdblist.com/lists/user/my-list",
            "https://mdblist.com/lists/user/my-list/",
            "https://mdblist.com/lists/user/my-list/json",
            "https://mdblist.com/lists/user/my-list?sort=rank",
        ] {
            let from_url = MdblistSource {
                url: Some(url.into()),
                ..source()
            };
            assert_eq!(
                address(&from_url).unwrap().path(),
                "lists/user/my-list/items",
                "{url}"
            );
        }

        let bad = MdblistSource {
            url: Some("https://example.com/nope".into()),
            ..source()
        };
        assert!(address(&bad).is_err());
    }

    #[test]
    fn items_prefer_the_nested_id_block() {
        let raw = json!({
            "id": 999,
            "title": "Fight Club",
            "imdb_id": "tt0137523",
            "release_year": 1999,
            "released": "1999-10-15",
            "rank": 7,
            "ids": {"tmdb": 550, "imdb": "tt0137523", "tvdb": null}
        });
        let item = convert(Some(MediaType::Movie), &raw, 0).unwrap();
        assert_eq!(item.ids.tmdb, Some(550));
        assert_eq!(item.ids.imdb.as_deref(), Some("tt0137523"));
        assert_eq!(item.ids.tvdb, None);
        assert_eq!(item.year, Some(1999));
        assert_eq!(item.rank, Some(7));
        assert_eq!(item.media_type, MediaType::Movie);
    }

    #[test]
    fn a_flat_id_is_used_when_there_is_no_nested_block() {
        let raw = json!({"id": 550, "title": "Fight Club"});
        assert_eq!(
            convert(Some(MediaType::Movie), &raw, 0).unwrap().ids.tmdb,
            Some(550)
        );
    }

    #[test]
    fn an_explicit_mediatype_overrides_the_array_it_came_from() {
        let raw = json!({"id": 1396, "mediatype": "show"});
        let item = convert(Some(MediaType::Movie), &raw, 0).unwrap();
        assert_eq!(item.media_type, MediaType::Show);
    }

    #[test]
    fn items_without_any_id_are_dropped() {
        assert!(convert(Some(MediaType::Movie), &json!({"title": "Nameless"}), 0).is_none());
    }

    #[test]
    fn both_response_shapes_parse_and_errors_surface() {
        let split: Page = serde_json::from_value(json!({
            "movies": [{"id": 1}],
            "shows": [{"id": 2}]
        }))
        .unwrap();
        let items = split.into_items().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, Some(MediaType::Movie));
        assert_eq!(items[1].0, Some(MediaType::Show));

        let flat: Page = serde_json::from_value(json!([{"id": 1}])).unwrap();
        assert_eq!(flat.into_items().unwrap().len(), 1);

        let error: Page = serde_json::from_value(json!({"error": "API Key Not Valid"})).unwrap();
        let message = error.into_items().unwrap_err().to_string();
        assert!(message.contains("API Key Not Valid"), "{message}");
    }
}
