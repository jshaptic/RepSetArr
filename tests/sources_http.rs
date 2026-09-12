//! Tests for the sources that talk HTTP, against a mock server.

mod support;

use serde_json::json;
use support::{get, state_from};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn mdblist_lists_are_paged_until_the_server_says_stop() {
    let server = MockServer::start().await;

    // A full page means there may be more, so a second request must follow.
    let first: Vec<serde_json::Value> = (0..1000)
        .map(|index| json!({"id": index + 1, "title": format!("Movie {index}")}))
        .collect();
    Mock::given(method("GET"))
        .and(path("/lists/someuser/trending/items"))
        .and(query_param("apikey", "test-key"))
        .and(query_param("offset", "0"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Has-More", "true")
                .set_body_json(json!({"movies": first, "shows": []})),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/lists/someuser/trending/items"))
        .and(query_param("offset", "1000"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Has-More", "false")
                .set_body_json(json!({
                    "movies": [{"id": 1001, "ids": {"tmdb": 1001}}],
                    "shows": [{"id": 1002, "mediatype": "show", "ids": {"tvdb": 81189}}]
                })),
        )
        .mount(&server)
        .await;

    let state = state_from(&format!(
        r#"
cache:
  persist: false
providers:
  mdblist:
    apikey: test-key
    base_url: {uri}
sources:
  trending:
    type: mdblist
    list: someuser/trending
lists:
  all:
    list_formula: "trending"
  movies:
    list_formula: "trending"
    media_type: movie
"#,
        uri = server.uri()
    ))
    .await;

    let health = get(&state, "/api/health").await.json();
    assert_eq!(health["sources"]["trending"]["items"], 1002);
    assert_eq!(health["status"], "ok");

    let movies = get(&state, "/api/lists/movies/radarr").await.json();
    assert_eq!(movies.as_array().unwrap().len(), 1001);

    let shows = get(&state, "/api/lists/all/sonarr").await.json();
    assert_eq!(shows, json!([{"tvdbId": 81189, "tmdbId": 1002}]));
}

#[tokio::test]
async fn an_mdblist_error_body_is_reported_rather_than_read_as_an_empty_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": "API Key Not Valid"})),
        )
        .mount(&server)
        .await;

    let state = state_from(&format!(
        r#"
cache:
  persist: false
providers:
  mdblist:
    apikey: wrong
    base_url: {uri}
sources:
  trending:
    type: mdblist
    list_id: 42
lists:
  all:
    list_formula: "trending"
"#,
        uri = server.uri()
    ))
    .await;

    let health = get(&state, "/api/health").await.json();
    assert_eq!(health["status"], "degraded");
    assert!(
        health["sources"]["trending"]["last_error"]
            .as_str()
            .unwrap()
            .contains("API Key Not Valid"),
        "{health}"
    );
}

#[tokio::test]
async fn a_fetched_item_carrying_two_ids_bridges_two_id_only_sources() {
    let server = MockServer::start().await;
    // This feed knows that tmdb:550 and tt0137523 are the same film.
    Mock::given(method("GET"))
        .and(path("/bridge.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"tmdbId": 550, "imdbId": "tt0137523", "title": "Fight Club"}
        ])))
        .mount(&server)
        .await;

    let state = state_from(&format!(
        r#"
cache:
  persist: false
sources:
  bridge:
    type: json
    url: {uri}/bridge.json
    media_type: movie
  wanted:
    type: static
    media_type: movie
    items: ["tmdb:550", "tmdb:603"]
  seen:
    type: static
    media_type: movie
    items: ["imdb:tt0137523"]
lists:
  unseen:
    list_formula: "(wanted | bridge) - seen"
"#,
        uri = server.uri()
    ))
    .await;

    let reply = get(&state, "/api/lists/unseen/radarr").await;
    assert_eq!(
        reply.json(),
        json!([{"id": 603}]),
        "the bridging item links tmdb:550 to tt0137523, so `seen` removes it"
    );
}

#[tokio::test]
async fn a_radarr_library_can_be_subtracted_from_a_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .and(header("X-Api-Key", "radarr-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"title": "The Matrix", "year": 1999, "tmdbId": 603, "monitored": true},
            {"title": "Alien", "year": 1979, "tmdbId": 348, "monitored": false}
        ])))
        .mount(&server)
        .await;

    let state = state_from(&format!(
        r#"
cache:
  persist: false
sources:
  my_radarr:
    type: radarr
    url: {uri}
    api_key: radarr-key
    monitored_only: true
  shortlist:
    type: static
    media_type: movie
    items: ["tmdb:550", "tmdb:603", "tmdb:348"]
lists:
  missing:
    list_formula: "shortlist - my_radarr"
"#,
        uri = server.uri()
    ))
    .await;

    let reply = get(&state, "/api/lists/missing/radarr").await;
    assert_eq!(
        reply.json(),
        json!([{"id": 550}, {"id": 348}]),
        "603 is monitored in Radarr; 348 is present but unmonitored, so it stays wanted"
    );
}

#[tokio::test]
async fn a_json_feed_with_remapped_fields_is_usable_as_a_source() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/movies.json"))
        .and(header("X-Token", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"items": [
                {"ident": "tt0137523", "label": "Fight Club"},
                {"ident": "tt0133093", "label": "The Matrix"}
            ]}
        })))
        .mount(&server)
        .await;

    let state = state_from(&format!(
        r#"
cache:
  persist: false
sources:
  feed:
    type: json
    url: {uri}/movies.json
    media_type: movie
    path: data.items
    headers:
      X-Token: abc
    fields:
      imdb: ident
      title: label
lists:
  all:
    list_formula: "feed"
"#,
        uri = server.uri()
    ))
    .await;

    let health = get(&state, "/api/health").await.json();
    assert_eq!(health["sources"]["feed"]["items"], 2);

    // Radarr's list needs TMDb ids, and this feed has none of them.
    let reply = get(&state, "/api/lists/all/radarr").await;
    assert_eq!(reply.json(), json!([]));
    assert_eq!(reply.header("X-Repsetarr-Skipped"), Some("2"));
}
