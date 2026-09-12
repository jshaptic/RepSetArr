//! Any JSON endpoint, mapped to items by configurable field names.
//!
//! This covers StevenLu-shaped feeds, another Repsetarr instance, and anything
//! homemade, without needing a dedicated provider for each.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::json_value::{as_date, as_genres, as_i32, as_str, as_u32, dig, first_present};
use crate::config::{FieldMap, JsonSource};
use crate::model::{Attrs, Item, MediaType, normalize_code, normalize_imdb};

pub async fn fetch(http: &reqwest::Client, source: &JsonSource) -> Result<Vec<Item>> {
    let mut request = http.get(&source.url);
    for (name, value) in &source.headers {
        request = request.header(name, value);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("requesting {}", source.url))?;
    let status = response.status();
    let body = response.text().await.context("reading response")?;
    if !status.is_success() {
        bail!("{} returned {status}", source.url);
    }

    let document: Value =
        serde_json::from_str(&body).with_context(|| format!("parsing JSON from {}", source.url))?;
    parse(&document, source)
}

pub fn parse(document: &Value, source: &JsonSource) -> Result<Vec<Item>> {
    let array = match &source.path {
        Some(path) => dig(document, path)
            .with_context(|| format!("`{path}` is not present in the response"))?,
        None => document,
    };
    let Some(entries) = array.as_array() else {
        bail!(
            "expected a JSON array{}, found {}",
            source
                .path
                .as_ref()
                .map(|path| format!(" at `{path}`"))
                .unwrap_or_default(),
            kind_of(array)
        );
    };

    let fields = &source.fields;
    let items = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let media_type = as_str(first_present(entry, &fields.media_type_keys()))
                .and_then(MediaType::parse)
                .unwrap_or(source.media_type);

            let mut item = Item::new(media_type);
            item.ids.tmdb = as_u32(first_present(entry, &fields.tmdb_keys()));
            item.ids.imdb =
                as_str(first_present(entry, &fields.imdb_keys())).and_then(normalize_imdb);
            item.ids.tvdb = as_u32(first_present(entry, &fields.tvdb_keys()));
            item.ids.trakt = as_u32(first_present(entry, &fields.trakt_keys()));
            if item.ids.is_empty() {
                return None;
            }

            item.title = as_str(first_present(entry, &fields.title_keys())).map(str::to_string);
            item.year = as_i32(first_present(entry, &fields.year_keys()));
            item.released = as_date(first_present(entry, &fields.released_keys()));
            item.rank = Some(index as u32 + 1);
            item.attrs = attrs(entry, fields);
            Some(item)
        })
        .collect();
    Ok(items)
}

/// Descriptive fields, if the feed happens to carry any. A feed that does not
/// leaves the gaps for the enricher.
fn attrs(entry: &Value, fields: &FieldMap) -> Attrs {
    let code = |keys: &[&str]| as_str(first_present(entry, keys)).and_then(normalize_code);
    Attrs {
        country: code(&fields.country_keys()),
        original_language: code(&fields.original_language_keys()),
        spoken_language: code(&fields.spoken_language_keys()),
        genres: as_genres(first_present(entry, &fields.genres_keys())),
        runtime: as_u32(first_present(entry, &fields.runtime_keys())).filter(|m| *m > 0),
        content_rating: as_str(first_present(entry, &fields.content_rating_keys()))
            .map(|text| text.to_ascii_uppercase()),
        status: code(&fields.status_keys()),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> JsonSource {
        JsonSource {
            url: "http://example.test/list.json".into(),
            headers: Default::default(),
            path: None,
            fields: Default::default(),
            media_type: MediaType::Movie,
            ttl: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_stevenlu_style_feed_parses_with_the_default_field_names() {
        let document = json!([
            {"title": "Fight Club", "imdb_id": "tt0137523"},
            {"title": "The Matrix", "imdb_id": "tt0133093"}
        ]);
        let items = parse(&document, &source()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].ids.imdb.as_deref(), Some("tt0137523"));
        assert_eq!(items[0].media_type, MediaType::Movie);
        assert_eq!(items[1].rank, Some(2));
    }

    #[test]
    fn a_nested_array_is_reachable_by_path() {
        let document = json!({"data": {"items": [{"tmdbId": 550}]}});
        let source = JsonSource {
            path: Some("data.items".into()),
            ..source()
        };
        assert_eq!(parse(&document, &source).unwrap()[0].ids.tmdb, Some(550));
    }

    #[test]
    fn field_names_can_be_remapped() {
        let document = json!([{"movie_db_id": 550, "label": "Fight Club", "kind": "show"}]);
        let mut source = source();
        source.fields.tmdb = vec!["movie_db_id".into()];
        source.fields.title = vec!["label".into()];
        source.fields.media_type = vec!["kind".into()];
        let items = parse(&document, &source).unwrap();
        assert_eq!(items[0].ids.tmdb, Some(550));
        assert_eq!(items[0].title.as_deref(), Some("Fight Club"));
        assert_eq!(items[0].media_type, MediaType::Show);
    }

    #[test]
    fn entries_without_ids_are_skipped_and_bad_shapes_error() {
        let document = json!([{"title": "Nameless"}, {"tmdb": 1}]);
        assert_eq!(parse(&document, &source()).unwrap().len(), 1);

        let error = parse(&json!({"not": "an array"}), &source())
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected a JSON array"), "{error}");

        let source = JsonSource {
            path: Some("missing".into()),
            ..source()
        };
        assert!(parse(&json!({}), &source).is_err());
    }
}
