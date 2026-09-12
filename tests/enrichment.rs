//! Metadata enrichment, against a mock provider.
//!
//! The point of these tests is the request count, not the payload: the whole
//! design exists so that a filter costs a bounded, one-off number of calls.

mod support;

use serde_json::{Value, json};
use support::{get, state_from};
use wiremock::matchers::{body_json_schema, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// A generic JSON feed carrying nothing but TMDb ids - the situation Radarr,
/// Sonarr and static sources are all in.
fn feed(ids: &[u32]) -> Value {
    json!(
        ids.iter()
            .map(|id| json!({"tmdb": id, "title": format!("Movie {id}")}))
            .collect::<Vec<Value>>()
    )
}

fn media(id: u32, country: &str) -> Value {
    json!({
        "id": id,
        "title": format!("Movie {id}"),
        "type": "movie",
        "ids": {"tmdb": id, "imdb": format!("tt{id:07}"), "trakt": id + 1000, "tvdb": null},
        "country": country,
        "language": "ru",
        "genres": [{"id": 6, "title": "Drama"}],
        "runtime": 100
    })
}

fn config(uri: &str) -> String {
    format!(
        r#"
cache:
  persist: false
providers:
  mdblist:
    apikey: test-key
    base_url: {uri}
sources:
  feed:
    type: json
    url: {uri}/feed.json
    media_type: movie
filters:
  russian:
    country: ru, su
lists:
  russian_only:
    list_formula: "feed"
    filter: "russian"
  everything:
    list_formula: "feed"
"#
    )
}

/// How many ids one POST asked for.
fn asked(request: &Request) -> usize {
    let body: Value = serde_json::from_slice(&request.body).expect("JSON body");
    body["ids"].as_array().expect("an ids array").len()
}

#[tokio::test]
async fn ids_are_looked_up_once_and_unknown_ones_are_remembered() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/feed.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(feed(&[1, 2, 3])))
        .mount(&server)
        .await;

    // Id 3 is silently omitted, which is what MDBList does for a title it does
    // not know. Without a negative entry it would be re-requested forever.
    Mock::given(method("POST"))
        .and(path("/tmdb/movie"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining", "99999")
                .set_body_json(json!([media(1, "su"), media(2, "us")])),
        )
        .mount(&server)
        .await;

    let state = state_from(&config(&server.uri())).await;

    let reply = get(&state, "/api/lists/russian_only/radarr").await;
    assert_eq!(reply.json(), json!([{"id": 1, "title": "Movie 1"}]));
    // Id 3's country is still unknown, so the filter had to judge it blind.
    assert_eq!(reply.header("X-Repsetarr-Unenriched"), Some("1"));

    async fn posts(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST)
            .count()
    }
    assert_eq!(posts(&server).await, 1, "one batch covers every id");

    // A second pass has nothing left to ask about: two answers and one
    // remembered miss.
    repsetarr::enrich::enrich_due(&state).await;
    assert_eq!(posts(&server).await, 1, "nothing is asked for twice");

    // The unfiltered list is untouched by any of this.
    assert_eq!(
        get(&state, "/api/lists/everything/radarr").await.json(),
        json!([
            {"id": 1, "title": "Movie 1"},
            {"id": 2, "title": "Movie 2"},
            {"id": 3, "title": "Movie 3"}
        ])
    );
}

#[tokio::test]
async fn large_libraries_are_batched_two_hundred_at_a_time() {
    let server = MockServer::start().await;
    let ids: Vec<u32> = (1..=450).collect();

    Mock::given(method("GET"))
        .and(path("/feed.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(feed(&ids)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/tmdb/movie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let state = state_from(&config(&server.uri())).await;
    let _ = get(&state, "/api/lists/russian_only/radarr").await;

    let requests = server.received_requests().await.expect("recorded");
    let sizes: Vec<usize> = requests
        .iter()
        .filter(|request| request.method == wiremock::http::Method::POST)
        .map(asked)
        .collect();
    assert_eq!(sizes, vec![200, 200, 50], "450 ids in three requests");
}

#[tokio::test]
async fn a_quota_error_leaves_the_list_serving_unfiltered_items() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/feed.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(feed(&[1, 2])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/tmdb/movie"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .mount(&server)
        .await;

    let state = state_from(&config(&server.uri())).await;

    // Unknown country, and the filter's default is to exclude, so the answer is
    // empty rather than wrong - and the header says the answer is provisional.
    let reply = get(&state, "/api/lists/russian_only/radarr").await;
    assert_eq!(reply.json(), json!([]));
    assert_eq!(reply.header("X-Repsetarr-Unenriched"), Some("2"));
    assert_eq!(reply.status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn a_config_with_no_filters_never_asks_for_metadata() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/feed.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(feed(&[1, 2, 3])))
        .mount(&server)
        .await;
    // Any POST at all is a failure; this mock exists only to make one visible.
    Mock::given(method("POST"))
        .and(body_json_schema::<Value>)
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
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
  feed:
    type: json
    url: {uri}/feed.json
    media_type: movie
lists:
  plain:
    list_formula: "feed"
"#,
        uri = server.uri()
    ))
    .await;

    let _ = get(&state, "/api/lists/plain/radarr").await;
    let requests = server.received_requests().await.expect("recorded");
    assert!(
        requests
            .iter()
            .all(|request| request.method == wiremock::http::Method::GET),
        "enrichment must be opt-in by use"
    );
}
