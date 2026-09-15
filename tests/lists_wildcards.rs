//! `*` in a list formula: shorthand for the union of every matching name.
//!
//! What these tests pin down is what a user actually feels: the expansion order
//! decides the item order, the index reports what a pattern resolved to, and a
//! source added later is picked up without touching the formula.

mod support;

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{get, post, state_at, state_from};

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

const STUDIOS: &str = r#"
cache:
  persist: false
sources:
  animation.studios.ghibli:
    type: static
    media_type: movie
    items: ["tmdb:1", "tmdb:2"]
  animation.studios.disney:
    type: static
    media_type: movie
    items: ["tmdb:3"]
  animation.studios.pixar:
    type: static
    media_type: movie
    items: ["tmdb:2", "tmdb:4"]
  live_action.nolan:
    type: static
    media_type: movie
    items: ["tmdb:9"]
lists:
  animation.all:
    list_formula: "animation.studios.*"
  everything_but_ghibli:
    list_formula: "*.studios.* - animation.studios.ghibli"
"#;

#[tokio::test]
async fn a_wildcard_unions_every_matching_source_in_declaration_order() {
    let state = state_from(STUDIOS).await;
    let reply = get(&state, "/api/lists/animation.all/radarr").await;
    assert_eq!(reply.status, StatusCode::OK);
    // Ghibli, then Disney, then Pixar - the order they are declared in, with
    // tmdb:2 kept where it first appeared.
    assert_eq!(ids(&reply.json()), vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn a_pattern_composes_with_the_rest_of_the_algebra() {
    let state = state_from(STUDIOS).await;
    let reply = get(&state, "/api/lists/everything_but_ghibli/radarr").await;
    assert_eq!(ids(&reply.json()), vec![3, 4]);
}

#[tokio::test]
async fn the_index_reports_what_a_pattern_expanded_to() {
    let state = state_from(STUDIOS).await;
    let body = get(&state, "/api/lists").await.json();
    let all = entry(&body, "animation.all");
    // The formula is echoed as written; the expansion shows up as dependencies.
    assert_eq!(all["list_formula"], "animation.studios.*");
    assert_eq!(
        all["sources"],
        json!([
            "animation.studios.ghibli",
            "animation.studios.disney",
            "animation.studios.pixar"
        ])
    );
    // `live_action.nolan` matches no pattern here, so nothing depends on it.
    assert!(
        !all["sources"]
            .as_array()
            .unwrap()
            .contains(&json!("live_action.nolan")),
        "{all}"
    );
}

#[tokio::test]
async fn a_source_added_later_widens_the_pattern_without_touching_the_formula() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yml");
    std::fs::write(&path, STUDIOS).unwrap();
    let state = state_at(STUDIOS, path.clone()).await;
    assert_eq!(
        ids(&get(&state, "/api/lists/animation.all/radarr").await.json()),
        vec![1, 2, 3, 4]
    );

    let widened = STUDIOS.replace(
        "  live_action.nolan:",
        "  animation.studios.laika:\n    type: static\n    media_type: movie\n    \
         items: [\"tmdb:7\"]\n  live_action.nolan:",
    );
    std::fs::write(&path, &widened).unwrap();
    assert_eq!(post(&state, "/api/reload").await.status, StatusCode::OK);

    // A reload recompiles but does not fetch, so a brand-new source the pattern
    // now reaches has no data yet - and a list fails while any source it
    // depends on is cold. The background refresher is what warms it.
    assert_eq!(
        get(&state, "/api/lists/animation.all/radarr").await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    repsetarr::refresh::prime(&state).await;

    let reply = get(&state, "/api/lists/animation.all/radarr").await;
    assert_eq!(ids(&reply.json()), vec![1, 2, 3, 4, 7]);
}

#[tokio::test]
async fn a_pattern_matching_nothing_is_refused_at_load() {
    let broken = r#"
cache:
  persist: false
sources:
  a: { type: static, media_type: movie, items: ["tmdb:1"] }
lists:
  bad: { list_formula: "ghost.*" }
"#;
    let parsed = repsetarr::config::parse_str(broken).expect("config parses");
    let error = repsetarr::config::compile(parsed, std::path::PathBuf::from("test.yml"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("matches no source or list"), "{error}");
}
