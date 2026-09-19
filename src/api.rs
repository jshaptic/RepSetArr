//! The HTTP surface: health, the list index, and one renderer per consumer.

use std::borrow::Cow;
use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{ListConfig, MediaTypeFilter};
use crate::lists::{self, EvalError, Evaluation};
use crate::model::{Item, MediaType};
use crate::state::SharedState;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/lists", get(index))
        .route("/api/lists/{name}", get(list))
        .route("/api/reload", post(reload))
        .with_state(state)
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<EvalError> for ApiError {
    fn from(error: EvalError) -> Self {
        let status = match error {
            EvalError::UnknownList(_) => StatusCode::NOT_FOUND,
            // The list is fine, the data behind it just is not here yet.
            EvalError::NoData(_) => StatusCode::SERVICE_UNAVAILABLE,
            EvalError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError {
            status,
            message: format!("{error}"),
        }
    }
}

fn bad_request(message: String) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        message,
    }
}

/// The encoding a consumer asks for with `?format=`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// The normalized items themselves - the default, and what a human reads.
    #[default]
    Json,
    Radarr,
    Sonarr,
    Kometa,
}

impl OutputFormat {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "json" => Some(OutputFormat::Json),
            "radarr" => Some(OutputFormat::Radarr),
            "sonarr" => Some(OutputFormat::Sonarr),
            "kometa" => Some(OutputFormat::Kometa),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Json => "json",
            OutputFormat::Radarr => "radarr",
            OutputFormat::Sonarr => "sonarr",
            OutputFormat::Kometa => "kometa",
        }
    }

    /// The one media type the format can represent, if it is limited to one:
    /// Radarr's import list parses movies and Sonarr's series, while Kometa and
    /// the raw dump carry both.
    fn media_type(self) -> Option<MediaType> {
        match self {
            OutputFormat::Radarr => Some(MediaType::Movie),
            OutputFormat::Sonarr => Some(MediaType::Show),
            OutputFormat::Json | OutputFormat::Kometa => None,
        }
    }
}

/// `?format=…&media_type=…`, parsed by hand so every rejection reads like the
/// rest of the API - a JSON `error`, not axum's plain-text `Query` rejection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ListQuery {
    format: OutputFormat,
    media_type: MediaTypeFilter,
}

impl ListQuery {
    fn from_params(params: &HashMap<String, String>) -> Result<Self, ApiError> {
        let mut query = ListQuery::default();
        // Kept verbatim so the compatibility error below quotes what was typed.
        let mut raw_media_type = "any";
        // Sorted so a request with two mistakes in it always names the same one.
        let mut entries: Vec<(&String, &String)> = params.iter().collect();
        entries.sort();
        for (key, value) in entries {
            match key.as_str() {
                "format" => {
                    query.format = OutputFormat::parse(value).ok_or_else(|| {
                        bad_request(format!(
                            "unknown format `{value}`, expected `json`, `radarr`, `sonarr` or `kometa`"
                        ))
                    })?;
                }
                "media_type" => {
                    query.media_type = MediaTypeFilter::parse(value).ok_or_else(|| {
                        bad_request(format!(
                            "unknown media_type `{value}`, expected `any`, `movies` or `shows`"
                        ))
                    })?;
                    raw_media_type = value;
                }
                other => {
                    return Err(bad_request(format!(
                        "unknown parameter `{other}`, expected `format` or `media_type`"
                    )));
                }
            }
        }

        // Asking Radarr's format for shows is a mis-wired URL, not an empty list.
        if let (Some(serves), Some(asked)) =
            (query.format.media_type(), query.media_type.as_media_type())
            && serves != asked
        {
            return Err(bad_request(format!(
                "format `{}` serves {}s only, but media_type `{raw_media_type}` was requested",
                query.format.as_str(),
                serves.as_str()
            )));
        }

        Ok(query)
    }
}

async fn health(State(state): State<SharedState>) -> Json<Value> {
    let runtime = state.runtime();
    let statuses = state.cache.statuses();

    let mut degraded = false;
    let mut sources = serde_json::Map::new();
    for (name, source) in &runtime.config.sources {
        let status = statuses.get(name).cloned().unwrap_or_default();
        let stale = status.is_stale(runtime.ttl_for(name));
        if status.items.is_none() || status.last_error.is_some() {
            degraded = true;
        }
        sources.insert(
            name.clone(),
            json!({
                "type": source.kind(),
                "items": status.item_count(),
                "fetched_at": status.fetched_at,
                "age_seconds": status.age().map(|age| age.as_secs()),
                "stale": stale,
                "last_attempt": status.last_attempt,
                "last_error": status.last_error,
            }),
        );
    }

    Json(json!({
        "status": if degraded { "degraded" } else { "ok" },
        "version": VERSION,
        "uptime_seconds": state.uptime_seconds(),
        "config": state.config_path.display().to_string(),
        "sources": Value::Object(sources),
        "lists": runtime.config.lists.keys().collect::<Vec<_>>(),
        "list_order": runtime.list_order,
    }))
}

async fn index(State(state): State<SharedState>) -> Json<Value> {
    let runtime = state.runtime();
    // One pass for the whole config: evaluating each list on its own would
    // re-run every dependency chain once per list.
    let mut evaluations = lists::evaluate_all(&runtime, &state.cache, &state.meta);
    let lists: Vec<Value> = runtime
        .config
        .lists
        .iter()
        .map(|(name, config)| {
            let compiled = runtime.list(name);
            let mut entry = json!({
                "name": name,
                "list_formula": config.list_formula,
                "filter": config.filter,
                "media_type": config.media_type.as_str(),
                "sources": compiled.map(|list| list.source_deps.clone()).unwrap_or_default(),
                "list_deps": compiled.map(|list| list.list_deps.clone()).unwrap_or_default(),
                "endpoints": {
                    "json": format!("/api/lists/{name}"),
                    "radarr": format!("/api/lists/{name}?format=radarr"),
                    "sonarr": format!("/api/lists/{name}?format=sonarr"),
                    "kometa": format!("/api/lists/{name}?format=kometa"),
                },
            });
            let evaluation = evaluations
                .remove(name)
                .unwrap_or_else(|| Err(EvalError::UnknownList(name.clone())));
            match evaluation {
                Ok(evaluation) => {
                    let movies = count(&evaluation.items, MediaType::Movie);
                    entry["items"] = json!(evaluation.items.len());
                    entry["movies"] = json!(movies);
                    entry["shows"] = json!(evaluation.items.len() - movies);
                    entry["stale_sources"] = json!(evaluation.stale_sources);
                    entry["unenriched"] = json!(evaluation.unenriched);
                    entry["evaluated_at"] = json!(evaluation.evaluated_at);
                }
                Err(error) => entry["error"] = json!(format!("{error}")),
            }
            entry
        })
        .collect();

    Json(json!(lists))
}

async fn reload(State(state): State<SharedState>) -> Result<Json<Value>, ApiError> {
    match state.reload() {
        Ok(runtime) => Ok(Json(json!({
            "status": "reloaded",
            "sources": runtime.config.sources.len(),
            "lists": runtime.config.lists.len(),
        }))),
        Err(error) => Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!("{error:#}"),
        }),
    }
}

/// One list, rendered the way the caller asked for it. The list is the
/// resource; `format` is only how it is written down.
async fn list(
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<SharedState>,
) -> Result<Response, ApiError> {
    let query = ListQuery::from_params(&params)?;

    // Kometa needs the list's own config, and reading it first keeps an unknown
    // list a 404 instead of whatever evaluating it would have said.
    let config = match query.format {
        OutputFormat::Kometa => Some(
            state
                .runtime()
                .config
                .lists
                .get(&name)
                .ok_or_else(|| ApiError::from(EvalError::UnknownList(name.clone())))?
                .clone(),
        ),
        _ => None,
    };

    let evaluation = evaluate(&state, &name)?;
    // Narrowing happens here rather than in `lists`, which evaluates the whole
    // config in one shared pass that a per-request option would invalidate.
    let items: Cow<[Item]> = match query.media_type.as_media_type() {
        None => Cow::Borrowed(&evaluation.items),
        Some(_) => Cow::Owned(
            evaluation
                .items
                .iter()
                .filter(|item| query.media_type.matches(item.media_type))
                .cloned()
                .collect(),
        ),
    };

    Ok(match query.format {
        OutputFormat::Json => (headers(&evaluation, items.len(), 0), Json(items)).into_response(),
        OutputFormat::Radarr => {
            let (payload, skipped) = radarr_payload(&items);
            (headers(&evaluation, payload.len(), skipped), Json(payload)).into_response()
        }
        OutputFormat::Sonarr => {
            let (payload, skipped) = sonarr_payload(&items);
            (headers(&evaluation, payload.len(), skipped), Json(payload)).into_response()
        }
        OutputFormat::Kometa => {
            let config = config.expect("the kometa branch read the config above");
            let body = kometa_document(&name, &config, &items);
            let mut response_headers = headers(&evaluation, items.len(), 0);
            response_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/yaml; charset=utf-8"),
            );
            (response_headers, body).into_response()
        }
    })
}

fn evaluate(state: &SharedState, name: &str) -> Result<Evaluation, ApiError> {
    let runtime = state.runtime();
    Ok(lists::evaluate(&runtime, &state.cache, &state.meta, name)?)
}

fn headers(evaluation: &Evaluation, returned: usize, skipped: usize) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Repsetarr-Count",
        HeaderValue::from_str(&returned.to_string()).expect("a number is a valid header"),
    );
    if skipped > 0 {
        headers.insert(
            "X-Repsetarr-Skipped",
            HeaderValue::from_str(&skipped.to_string()).expect("a number is a valid header"),
        );
    }
    if evaluation.unenriched > 0 {
        headers.insert(
            "X-Repsetarr-Unenriched",
            HeaderValue::from_str(&evaluation.unenriched.to_string())
                .expect("a number is a valid header"),
        );
    }
    if !evaluation.stale_sources.is_empty()
        && let Ok(value) = HeaderValue::from_str(&evaluation.stale_sources.join(","))
    {
        headers.insert("X-Repsetarr-Stale", value);
    }
    headers
}

fn count(items: &[Item], media_type: MediaType) -> usize {
    items
        .iter()
        .filter(|item| item.media_type == media_type)
        .count()
}

/// Radarr's "Custom Lists" import list parses TMDb search results and reads
/// nothing but `id`. The rest is there so a human can read the response.
#[derive(Debug, Serialize, PartialEq)]
pub struct RadarrMovie {
    pub id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
}

/// Returns the payload and the number of movies dropped for want of a TMDb id.
pub fn radarr_payload(items: &[Item]) -> (Vec<RadarrMovie>, usize) {
    let movies: Vec<&Item> = items
        .iter()
        .filter(|item| item.media_type == MediaType::Movie)
        .collect();
    let payload: Vec<RadarrMovie> = movies
        .iter()
        .filter_map(|item| {
            Some(RadarrMovie {
                id: item.ids.tmdb?,
                title: item.title.clone(),
                imdb_id: item.ids.imdb.clone(),
                year: item.effective_year(),
            })
        })
        .collect();
    let skipped = movies.len() - payload.len();
    (payload, skipped)
}

/// Sonarr's "Custom List" import list reads `title`, `tvdbId`, `tmdbId` and
/// `imdbId`; unknown ids are omitted rather than sent as zero.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SonarrSeries {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvdb_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmdb_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
}

/// Returns the payload and the number of shows with no TVDb id, which Sonarr
/// has to resolve by another id.
pub fn sonarr_payload(items: &[Item]) -> (Vec<SonarrSeries>, usize) {
    let payload: Vec<SonarrSeries> = items
        .iter()
        .filter(|item| item.media_type == MediaType::Show)
        .map(|item| SonarrSeries {
            title: item.title.clone(),
            tvdb_id: item.ids.tvdb,
            tmdb_id: item.ids.tmdb,
            imdb_id: item.ids.imdb.clone(),
        })
        .collect();
    let without_tvdb = payload
        .iter()
        .filter(|series| series.tvdb_id.is_none())
        .count();
    (payload, without_tvdb)
}

/// A Kometa collection file, to be loaded with `collection_files: - url: …`.
///
/// Multiple builders in one collection are unioned by Kometa, so shows that
/// have no TVDb id can still be carried by `tmdb_show`.
pub fn kometa_document(name: &str, config: &ListConfig, items: &[Item]) -> String {
    use serde_yaml_ng::{Mapping, Value as Yaml};

    let ids = |media_type: MediaType, pick: &dyn Fn(&Item) -> Option<u32>| -> String {
        items
            .iter()
            .filter(|item| item.media_type == media_type)
            .filter_map(pick)
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let movies = ids(MediaType::Movie, &|item| item.ids.tmdb);
    let shows_by_tvdb = ids(MediaType::Show, &|item| item.ids.tvdb);
    let shows_by_tmdb = ids(MediaType::Show, &|item| {
        item.ids.tvdb.is_none().then_some(item.ids.tmdb).flatten()
    });

    let mut block = Mapping::new();
    let mut insert = |key: &str, value: &str| {
        if !value.is_empty() {
            block.insert(Yaml::from(key), Yaml::from(value));
        }
    };
    insert("tmdb_movie", &movies);
    insert("tvdb_show", &shows_by_tvdb);
    insert("tmdb_show", &shows_by_tmdb);

    let mut document = Mapping::new();
    if block.is_empty() {
        // Every builder is empty; emitting a builderless collection would make
        // Kometa fail the run, so the file is deliberately left with none.
        document.insert(Yaml::from("collections"), Yaml::Mapping(Mapping::new()));
    } else {
        block.insert(
            Yaml::from("sync_mode"),
            Yaml::from(config.kometa.sync_mode.as_str()),
        );
        block.insert(
            Yaml::from("collection_order"),
            Yaml::from(config.kometa.collection_order.as_str()),
        );
        for (key, value) in &config.kometa.extra {
            block.insert(Yaml::from(key.as_str()), value.clone());
        }
        let collection = config
            .kometa
            .collection
            .clone()
            .unwrap_or_else(|| name.to_string());
        let mut collections = Mapping::new();
        collections.insert(Yaml::from(collection), Yaml::Mapping(block));
        document.insert(Yaml::from("collections"), Yaml::Mapping(collections));
    }

    let body = serde_yaml_ng::to_string(&document).expect("a mapping always serializes");
    format!(
        "# Generated by Repsetarr {VERSION} - list `{name}` - {} item(s)\n{body}",
        items.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MediaIds;

    fn movie(tmdb: Option<u32>, imdb: Option<&str>, title: &str, year: i32) -> Item {
        Item {
            media_type: MediaType::Movie,
            ids: MediaIds {
                tmdb,
                imdb: imdb.map(str::to_string),
                ..Default::default()
            },
            title: Some(title.to_string()),
            year: Some(year),
            ..Default::default()
        }
    }

    fn show(tvdb: Option<u32>, tmdb: Option<u32>, title: &str) -> Item {
        Item {
            media_type: MediaType::Show,
            ids: MediaIds {
                tmdb,
                tvdb,
                imdb: Some("tt0903747".into()),
                ..Default::default()
            },
            title: Some(title.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn the_radarr_payload_is_what_radarr_parses() {
        let items = vec![
            movie(Some(550), Some("tt0137523"), "Fight Club", 1999),
            movie(None, Some("tt0133093"), "The Matrix", 1999),
            show(Some(81189), Some(1396), "Breaking Bad"),
        ];
        let (payload, skipped) = radarr_payload(&items);
        assert_eq!(skipped, 1, "the movie with no TMDb id is dropped");
        assert_eq!(payload.len(), 1, "shows never appear in a Radarr list");
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!([{"id": 550, "title": "Fight Club", "imdb_id": "tt0137523", "year": 1999}])
        );
    }

    #[test]
    fn the_sonarr_payload_is_what_sonarr_parses() {
        let items = vec![
            show(Some(81189), Some(1396), "Breaking Bad"),
            show(None, Some(1399), "Game of Thrones"),
            movie(Some(550), None, "Fight Club", 1999),
        ];
        let (payload, without_tvdb) = sonarr_payload(&items);
        assert_eq!(without_tvdb, 1);
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!([
                {"title": "Breaking Bad", "tvdbId": 81189, "tmdbId": 1396, "imdbId": "tt0903747"},
                {"title": "Game of Thrones", "tmdbId": 1399, "imdbId": "tt0903747"}
            ]),
            "movies are excluded and unknown ids are omitted, not zero"
        );
    }

    fn list_config() -> ListConfig {
        serde_yaml_ng::from_str("list_formula: a").unwrap()
    }

    #[test]
    fn the_kometa_document_uses_id_builders() {
        let items = vec![
            movie(Some(550), None, "Fight Club", 1999),
            movie(Some(603), None, "The Matrix", 1999),
            show(Some(81189), Some(1396), "Breaking Bad"),
            show(None, Some(1399), "Game of Thrones"),
        ];
        let document = kometa_document("wanted", &list_config(), &items);
        assert_eq!(
            document.lines().skip(1).collect::<Vec<_>>().join("\n"),
            concat!(
                "collections:\n",
                "  wanted:\n",
                "    tmdb_movie: 550, 603\n",
                "    tvdb_show: '81189'\n",
                "    tmdb_show: '1399'\n",
                "    sync_mode: sync\n",
                "    collection_order: custom"
            )
        );
        assert!(document.starts_with("# Generated by Repsetarr"));
    }

    #[test]
    fn the_kometa_collection_name_and_extras_are_configurable() {
        let mut config = list_config();
        config.kometa.collection = Some("Wanted Movies".into());
        config.kometa.sync_mode = "append".into();
        config.kometa.extra.insert(
            "summary".into(),
            serde_yaml_ng::Value::from("Everything I still want"),
        );
        let document = kometa_document(
            "wanted",
            &config,
            &[movie(Some(550), None, "Fight Club", 1999)],
        );
        assert!(document.contains("  Wanted Movies:"), "{document}");
        assert!(document.contains("sync_mode: append"), "{document}");
        assert!(
            document.contains("summary: Everything I still want"),
            "{document}"
        );
    }

    #[test]
    fn an_empty_list_produces_no_builderless_collection() {
        let document = kometa_document("wanted", &list_config(), &[]);
        assert!(document.contains("collections: {}"), "{document}");
    }
}
