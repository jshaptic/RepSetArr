//! Shared helpers for the integration tests.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use repsetarr::cache::CacheStore;
use repsetarr::state::{AppState, SharedState};
use repsetarr::{api, config, refresh};
use tower::ServiceExt;

pub async fn state_from(yaml: &str) -> SharedState {
    state_at(yaml, PathBuf::from("test.yml")).await
}

pub async fn state_at(yaml: &str, path: PathBuf) -> SharedState {
    let parsed = config::parse_str(yaml).expect("config parses");
    let runtime = config::compile(parsed, path).expect("config compiles");
    let cache = Arc::new(CacheStore::new(
        std::env::temp_dir().join("repsetarr-tests"),
        false,
    ));
    let state: SharedState = Arc::new(AppState::new(runtime, cache, reqwest::Client::new()));
    refresh::prime(&state).await;
    state
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("body is JSON")
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

pub async fn get(state: &SharedState, uri: &str) -> Reply {
    request(state, "GET", uri).await
}

pub async fn post(state: &SharedState, uri: &str) -> Reply {
    request(state, "POST", uri).await
}

async fn request(state: &SharedState, method: &str, uri: &str) -> Reply {
    let response = api::router(Arc::clone(state))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("the router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).expect("UTF-8 body"),
    }
}
