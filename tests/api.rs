//! End-to-end tests over the real router, using static sources so no network
//! is involved and the expected payloads can be asserted exactly.

mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{get, post, state_at, state_from};

const CONFIG: &str = r#"
cache:
  persist: false
sources:
  trending:
    type: static
    media_type: movie
    items: ["tmdb:550", "tmdb:603", "tmdb:13"]
  owned:
    type: static
    media_type: movie
    items: ["tmdb:603"]
  shows:
    type: static
    media_type: show
    items: ["tvdb:81189", "tmdb:1399"]
lists:
  wanted_movies:
    list_formula: "trending - owned"
    media_type: movie
  wanted_shows:
    list_formula: "shows"
    media_type: show
  everything:
    list_formula: "wanted_movies | shows"
"#;

#[tokio::test]
async fn the_radarr_format_serves_the_result_of_the_algebra() {
    let state = state_from(CONFIG).await;
    let reply = get(&state, "/api/lists/wanted_movies?format=radarr").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json(),
        json!([{"id": 550}, {"id": 13}]),
        "603 is in `owned`, so it is subtracted"
    );
    assert_eq!(reply.header("X-Repsetarr-Count"), Some("2"));
}

#[tokio::test]
async fn the_sonarr_format_serves_shows_only() {
    let state = state_from(CONFIG).await;
    let reply = get(&state, "/api/lists/everything?format=sonarr").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json(),
        json!([{"tvdbId": 81189}, {"tmdbId": 1399}]),
        "the two movies in the same list are not offered to Sonarr"
    );
    assert_eq!(
        reply.header("X-Repsetarr-Skipped"),
        Some("1"),
        "one show has no TVDb id"
    );
}

#[tokio::test]
async fn the_kometa_format_serves_a_collection_file() {
    let state = state_from(CONFIG).await;
    let reply = get(&state, "/api/lists/everything?format=kometa").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("application/yaml; charset=utf-8")
    );
    assert!(reply.body.contains("tmdb_movie: 550, 13"), "{}", reply.body);
    assert!(reply.body.contains("tvdb_show: '81189'"), "{}", reply.body);
    assert!(reply.body.contains("tmdb_show: '1399'"), "{}", reply.body);
}

#[tokio::test]
async fn the_bare_list_url_serves_the_normalized_items() {
    let state = state_from(CONFIG).await;
    let reply = get(&state, "/api/lists/wanted_shows").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json(),
        json!([
            {"media_type": "show", "ids": {"tvdb": 81189}, "rank": 1},
            {"media_type": "show", "ids": {"tmdb": 1399}, "rank": 2}
        ]),
        "no format means the items themselves, with only the ids we know"
    );
    assert_eq!(reply.header("X-Repsetarr-Count"), Some("2"));
}

#[tokio::test]
async fn media_type_narrows_a_mixed_list() {
    let state = state_from(CONFIG).await;

    let movies = get(
        &state,
        "/api/lists/everything?format=kometa&media_type=movies",
    )
    .await;
    assert_eq!(movies.status, StatusCode::OK);
    assert!(
        movies.body.contains("tmdb_movie: 550, 13"),
        "{}",
        movies.body
    );
    assert!(!movies.body.contains("_show"), "{}", movies.body);
    assert_eq!(movies.header("X-Repsetarr-Count"), Some("2"));

    let shows = get(
        &state,
        "/api/lists/everything?format=kometa&media_type=shows",
    )
    .await;
    assert!(shows.body.contains("tvdb_show: '81189'"), "{}", shows.body);
    assert!(!shows.body.contains("tmdb_movie"), "{}", shows.body);
    assert_eq!(shows.header("X-Repsetarr-Count"), Some("2"));
}

#[tokio::test]
async fn media_type_accepts_the_spellings_a_human_types() {
    let state = state_from(CONFIG).await;
    let plural = get(&state, "/api/lists/everything?media_type=movies").await;
    let singular = get(&state, "/api/lists/everything?media_type=movie").await;

    assert_eq!(plural.status, StatusCode::OK);
    assert_eq!(plural.body, singular.body);
    assert_eq!(plural.header("X-Repsetarr-Count"), Some("2"));

    let any = get(&state, "/api/lists/everything?media_type=any").await;
    assert_eq!(any.header("X-Repsetarr-Count"), Some("4"));
}

#[tokio::test]
async fn a_media_type_the_format_cannot_serve_is_a_400() {
    let state = state_from(CONFIG).await;

    let reply = get(
        &state,
        "/api/lists/everything?format=radarr&media_type=shows",
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(
        reply
            .body
            .contains("serves movies only, but media_type `shows` was requested"),
        "the error quotes the value as it was typed: {}",
        reply.body
    );

    let agreeing = get(
        &state,
        "/api/lists/everything?format=radarr&media_type=movies",
    )
    .await;
    assert_eq!(
        agreeing.status,
        StatusCode::OK,
        "a media_type the format already implies is fine"
    );
}

#[tokio::test]
async fn an_unusable_query_is_a_400_with_a_json_error() {
    let state = state_from(CONFIG).await;

    for (uri, expected) in [
        ("/api/lists/everything?format=bogus", "unknown format"),
        (
            "/api/lists/everything?media_type=anime",
            "unknown media_type",
        ),
        ("/api/lists/everything?fomat=radarr", "unknown parameter"),
    ] {
        let reply = get(&state, uri).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            reply.json()["error"]
                .as_str()
                .expect("errors are JSON")
                .contains(expected),
            "{uri}: {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn an_unknown_list_is_a_404() {
    let state = state_from(CONFIG).await;
    for format in ["json", "radarr", "sonarr", "kometa"] {
        let reply = get(&state, &format!("/api/lists/ghost?format={format}")).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{format}");
        assert!(reply.body.contains("ghost"), "{}", reply.body);
    }
}

#[tokio::test]
async fn a_list_whose_source_has_never_been_fetched_is_a_503() {
    // Port 1 refuses immediately, so the source never gets any data.
    let state = state_from(
        r#"
cache:
  persist: false
sources:
  unreachable:
    type: json
    url: http://127.0.0.1:1/list.json
lists:
  broken:
    list_formula: "unreachable"
"#,
    )
    .await;

    let reply = get(&state, "/api/lists/broken?format=radarr").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(reply.body.contains("unreachable"), "{}", reply.body);

    let health = get(&state, "/api/health").await.json();
    assert_eq!(health["status"], "degraded");
    assert!(health["sources"]["unreachable"]["last_error"].is_string());
    assert_eq!(health["sources"]["unreachable"]["items"], json!(null));
}

#[tokio::test]
async fn health_reports_every_source_and_list() {
    let state = state_from(CONFIG).await;
    let health = get(&state, "/api/health").await.json();

    assert_eq!(health["status"], "ok");
    assert_eq!(health["sources"]["trending"]["items"], 3);
    assert_eq!(health["sources"]["trending"]["type"], "static");
    assert_eq!(health["sources"]["trending"]["stale"], false);
    assert_eq!(
        health["lists"],
        json!(["wanted_movies", "wanted_shows", "everything"])
    );
}

#[tokio::test]
async fn the_index_counts_movies_and_shows_per_list() {
    let state = state_from(CONFIG).await;
    let index = get(&state, "/api/lists").await.json();

    let everything = index
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "everything")
        .unwrap();
    assert_eq!(everything["items"], 4);
    assert_eq!(everything["movies"], 2);
    assert_eq!(everything["shows"], 2);
    assert_eq!(everything["sources"], json!(["trending", "owned", "shows"]));
    assert_eq!(everything["endpoints"]["json"], "/api/lists/everything");
    assert_eq!(
        everything["endpoints"]["kometa"],
        "/api/lists/everything?format=kometa"
    );
}

#[tokio::test]
async fn a_list_may_be_built_from_another_list_after_its_limit_applies() {
    let state = state_from(
        r#"
cache:
  persist: false
sources:
  all:
    type: static
    media_type: movie
    items: ["tmdb:1", "tmdb:2", "tmdb:3", "tmdb:4"]
  some:
    type: static
    media_type: movie
    items: ["tmdb:3", "tmdb:4"]
lists:
  first_two:
    list_formula: "all"
    limit: 2
  first_two_minus_some:
    list_formula: "first_two - some"
"#,
    )
    .await;

    let reply = get(&state, "/api/lists/first_two_minus_some?format=radarr").await;
    assert_eq!(
        reply.json(),
        json!([{"id": 1}, {"id": 2}]),
        "the referenced list contributes its post-processed contents"
    );
}

#[tokio::test]
async fn nothing_links_two_ids_unless_an_item_carries_both() {
    let state = state_from(
        r#"
cache:
  persist: false
sources:
  by_tmdb:
    type: static
    media_type: movie
    items: ["tmdb:550", "tmdb:603"]
  by_imdb:
    type: static
    media_type: movie
    items: ["imdb:tt0137523"]
lists:
  wanted:
    list_formula: "by_tmdb - by_imdb"
"#,
    )
    .await;

    // tt0137523 *is* Fight Club, but no item here carries both ids, so there is
    // nothing to match on and both TMDb entries survive. The bridging case is
    // covered in tests/sources_http.rs, where a fetched item carries both.
    assert_eq!(
        get(&state, "/api/lists/wanted?format=radarr").await.json(),
        json!([{"id": 550}, {"id": 603}])
    );
}
#[tokio::test]
async fn reload_picks_up_a_changed_file_and_refuses_a_broken_one() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yml");
    std::fs::write(&path, CONFIG).unwrap();
    let state = state_at(CONFIG, path.clone()).await;

    std::fs::write(
        &path,
        format!("{CONFIG}  added:\n    list_formula: \"trending\"\n    media_type: movie\n"),
    )
    .unwrap();
    let reply = post(&state, "/api/reload").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["lists"], 4);

    // The new list is live without a restart, and its sources are already warm.
    let added = get(&state, "/api/lists/added?format=radarr").await;
    assert_eq!(added.status, StatusCode::OK);
    assert_eq!(added.json(), json!([{"id": 550}, {"id": 603}, {"id": 13}]));

    std::fs::write(
        &path,
        "lists:\n  broken: { list_formula: \"nothing_here\" }\n",
    )
    .unwrap();
    let reply = post(&state, "/api/reload").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(reply.body.contains("nothing_here"), "{}", reply.body);

    // The previous configuration is still serving.
    assert_eq!(
        get(&state, "/api/lists/added?format=radarr").await.status,
        StatusCode::OK
    );
}
