//! Lists that reference other lists.
//!
//! The algebra treats a list and a source as the same kind of operand, so a
//! formula composes over either. What these tests pin down is the part that is
//! easy to break: a referenced list contributes its *post-processed* contents,
//! the dependency graph is reported in evaluation order, and one source with no
//! data fails only the lists that actually reach it.

mod support;

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{get, state_from};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The ids a Radarr payload carries, in order.
fn ids(body: &Value) -> Vec<u64> {
    body.as_array()
        .expect("the Radarr payload is an array")
        .iter()
        .map(|movie| movie["id"].as_u64().expect("every entry has an id"))
        .collect()
}

/// One list's entry in the `/api/lists` index.
fn entry(body: &Value, name: &str) -> Value {
    body.as_array()
        .expect("the index is an array")
        .iter()
        .find(|list| list["name"] == name)
        .unwrap_or_else(|| panic!("`{name}` is in the index"))
        .clone()
}

const CHAIN: &str = r#"
cache:
  persist: false
sources:
  src1:
    type: static
    media_type: movie
    items: ["tmdb:1", "tmdb:2"]
  src2:
    type: static
    media_type: movie
    items: ["tmdb:3"]
  src3:
    type: static
    media_type: movie
    items: ["tmdb:2"]
  src4:
    type: static
    media_type: movie
    items: ["tmdb:3", "tmdb:9"]
lists:
  a:
    list_formula: "src1 | src2"
  b:
    list_formula: "a - src3"
  c:
    list_formula: "b & src4"
"#;

#[tokio::test]
async fn a_three_deep_chain_composes() {
    let state = state_from(CHAIN).await;

    assert_eq!(
        ids(&get(&state, "/api/lists/a?format=radarr").await.json()),
        [1, 2, 3]
    );
    assert_eq!(
        ids(&get(&state, "/api/lists/b?format=radarr").await.json()),
        [1, 3],
        "`b` subtracts src3 from what `a` produced"
    );
    assert_eq!(
        ids(&get(&state, "/api/lists/c?format=radarr").await.json()),
        [3],
        "`c` intersects src4 with what `b` produced; 9 is not in `b` and 1 is not in src4"
    );
}

#[tokio::test]
async fn the_index_reports_the_dependency_graph_in_evaluation_order() {
    let state = state_from(CHAIN).await;
    let index = get(&state, "/api/lists").await.json();

    assert_eq!(entry(&index, "a")["list_deps"], json!([]));
    assert_eq!(entry(&index, "b")["list_deps"], json!(["a"]));
    assert_eq!(
        entry(&index, "c")["list_deps"],
        json!(["a", "b"]),
        "transitive, dependencies first"
    );
    assert_eq!(
        entry(&index, "c")["sources"],
        json!(["src1", "src2", "src3", "src4"]),
        "source_deps is transitive too"
    );

    let health = get(&state, "/api/health").await.json();
    assert_eq!(health["list_order"], json!(["a", "b", "c"]));
}

#[tokio::test]
async fn a_diamond_dependency_is_evaluated_once_and_listed_once() {
    const DIAMOND: &str = r#"
cache:
  persist: false
sources:
  base_src:
    type: static
    media_type: movie
    items: ["tmdb:1", "tmdb:2", "tmdb:3", "tmdb:4"]
  x:
    type: static
    media_type: movie
    items: ["tmdb:1"]
  y:
    type: static
    media_type: movie
    items: ["tmdb:2"]
lists:
  base:
    list_formula: "base_src"
  left:
    list_formula: "base - x"
  right:
    list_formula: "base - y"
  top:
    list_formula: "left | right"
"#;
    let state = state_from(DIAMOND).await;

    assert_eq!(
        ids(&get(&state, "/api/lists/left?format=radarr").await.json()),
        [2, 3, 4]
    );
    assert_eq!(
        ids(&get(&state, "/api/lists/right?format=radarr").await.json()),
        [1, 3, 4]
    );
    assert_eq!(
        ids(&get(&state, "/api/lists/top?format=radarr").await.json()),
        [2, 3, 4, 1],
        "a union keeps the left side's order and appends what is new on the right"
    );

    let index = get(&state, "/api/lists").await.json();
    assert_eq!(
        entry(&index, "top")["list_deps"],
        json!(["base", "left", "right"]),
        "`base` is reached through both branches but named once"
    );
}

#[tokio::test]
async fn a_dependencys_limit_bounds_what_the_dependent_sees() {
    const LIMITED: &str = r#"
cache:
  persist: false
sources:
  everything:
    type: static
    media_type: movie
    items: ["tmdb:10", "tmdb:11", "tmdb:12", "tmdb:13"]
  owned:
    type: static
    media_type: movie
    items: ["tmdb:12"]
lists:
  shortlist:
    list_formula: "everything"
    limit: 2
  to_grab:
    list_formula: "shortlist - owned"
"#;
    let state = state_from(LIMITED).await;

    assert_eq!(
        ids(&get(&state, "/api/lists/shortlist?format=radarr")
            .await
            .json()),
        [10, 11]
    );
    assert_eq!(
        ids(&get(&state, "/api/lists/to_grab?format=radarr").await.json()),
        [10, 11],
        "13 was cut by `shortlist`'s limit, so subtracting `owned` cannot bring it back"
    );
}

#[tokio::test]
async fn a_dependencys_filter_bounds_what_the_dependent_sees() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"tmdb": 20, "title": "Twenty", "country": "ru"},
            {"tmdb": 21, "title": "Twenty-one", "country": "us"},
            {"tmdb": 22, "title": "Twenty-two", "country": "ru"}
        ])))
        .mount(&server)
        .await;

    let config = format!(
        r#"
cache:
  persist: false
sources:
  feed:
    type: json
    url: {}/feed.json
    media_type: movie
  owned:
    type: static
    media_type: movie
    items: ["tmdb:22"]
filters:
  russian:
    country: ru
lists:
  russian_only:
    list_formula: "feed"
    filter: "russian"
  to_grab:
    list_formula: "russian_only - owned"
"#,
        server.uri()
    );
    let state = state_from(&config).await;

    assert_eq!(
        ids(&get(&state, "/api/lists/russian_only?format=radarr")
            .await
            .json()),
        [20, 22]
    );
    assert_eq!(
        ids(&get(&state, "/api/lists/to_grab?format=radarr").await.json()),
        [20],
        "21 was dropped by `russian_only`'s filter, not by the subtraction"
    );
}

#[tokio::test]
async fn one_source_with_no_data_fails_only_the_lists_that_reach_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/broken.json"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let config = format!(
        r#"
cache:
  persist: false
sources:
  broken:
    type: json
    url: {}/broken.json
    media_type: movie
  healthy:
    type: static
    media_type: movie
    items: ["tmdb:30", "tmdb:31"]
lists:
  fine:
    list_formula: "healthy"
  doomed:
    list_formula: "broken"
  downstream:
    list_formula: "doomed | healthy"
"#,
        server.uri()
    );
    let state = state_from(&config).await;

    let index = get(&state, "/api/lists").await.json();
    assert_eq!(entry(&index, "fine")["items"], json!(2));
    assert!(entry(&index, "fine")["error"].is_null());
    assert!(
        entry(&index, "doomed")["error"]
            .as_str()
            .is_some_and(|error| error.contains("broken")),
        "{index}"
    );
    assert!(
        entry(&index, "downstream")["error"]
            .as_str()
            .is_some_and(|error| error.contains("broken")),
        "a list that reaches an unfetched source through a dependency fails too: {index}"
    );

    assert_eq!(
        get(&state, "/api/lists/fine?format=radarr").await.status,
        StatusCode::OK
    );
    assert_eq!(
        get(&state, "/api/lists/downstream?format=radarr")
            .await
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
