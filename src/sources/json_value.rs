//! Small helpers for digging ids out of loosely typed JSON.

use chrono::NaiveDate;
use serde_json::Value;

/// Numbers arrive as numbers, as strings, or not at all.
pub fn as_u32(value: Option<&Value>) -> Option<u32> {
    match value? {
        Value::Number(number) => number.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

pub fn as_i32(value: Option<&Value>) -> Option<i32> {
    match value? {
        Value::Number(number) => number.as_i64().and_then(|n| i32::try_from(n).ok()),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

pub fn as_str(value: Option<&Value>) -> Option<&str> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim()),
        _ => None,
    }
}

pub fn as_date(value: Option<&Value>) -> Option<NaiveDate> {
    let text = as_str(value)?;
    // Radarr and Sonarr send full timestamps, MDBList sends plain dates.
    NaiveDate::parse_from_str(&text[..text.len().min(10)], "%Y-%m-%d").ok()
}

/// Genres arrive either as plain strings (`["drama"]`, from the list endpoint)
/// or as objects (`[{"id":6,"title":"Drama"}]`, from the batch endpoint).
/// Normalized to lowercase names, deduplicated, order preserved.
pub fn as_genres(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(entries)) = value else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = match entry {
            Value::String(_) => as_str(Some(entry)),
            Value::Object(_) => as_str(first_present(entry, &["title", "name", "genre"])),
            _ => None,
        };
        if let Some(name) = name.and_then(crate::model::normalize_code)
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

/// Look up the first key that is present, so a feed can spell it `tmdbId`,
/// `tmdb_id` or `tmdb`.
pub fn first_present<'a>(object: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .filter_map(|key| object.get(key))
        .find(|value| !value.is_null())
}

/// Walk a dotted path such as `data.items`.
pub fn dig<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|segment| !segment.is_empty())
        .try_fold(value, |current, segment| current.get(segment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_parse_from_both_json_shapes() {
        assert_eq!(as_u32(Some(&json!(550))), Some(550));
        assert_eq!(as_u32(Some(&json!("550"))), Some(550));
        assert_eq!(as_u32(Some(&json!(null))), None);
        assert_eq!(as_u32(Some(&json!(-1))), None);
        assert_eq!(as_i32(Some(&json!(-1))), Some(-1));
    }

    #[test]
    fn dates_tolerate_trailing_timestamps() {
        assert_eq!(
            as_date(Some(&json!("1999-10-15T00:00:00Z"))),
            NaiveDate::from_ymd_opt(1999, 10, 15)
        );
        assert_eq!(as_date(Some(&json!("not a date"))), None);
    }

    #[test]
    fn genres_normalize_from_both_shapes() {
        assert_eq!(
            as_genres(Some(&json!(["Drama", "War"]))),
            vec!["drama".to_string(), "war".to_string()]
        );
        assert_eq!(
            as_genres(Some(
                &json!([{"id": 6, "title": "Drama"}, {"title": "drama"}])
            )),
            vec!["drama".to_string()]
        );
        assert!(as_genres(Some(&json!(null))).is_empty());
        assert!(as_genres(None).is_empty());
    }

    #[test]
    fn key_candidates_are_tried_in_order() {
        let object = json!({"tmdb_id": 550, "tmdb": 1});
        assert_eq!(
            as_u32(first_present(&object, &["tmdbId", "tmdb_id", "tmdb"])),
            Some(550)
        );
        assert!(first_present(&object, &["nope"]).is_none());
    }

    #[test]
    fn dotted_paths_navigate_nested_objects() {
        let object = json!({"data": {"items": [1, 2]}});
        assert_eq!(
            dig(&object, "data.items")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(dig(&object, "data.missing").is_none());
        assert!(dig(&object, "").is_some());
    }
}
