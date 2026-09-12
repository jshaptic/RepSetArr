//! Backfilling attributes that sources do not carry.
//!
//! The cost control here is scoping, not throttling. Three things narrow the
//! work before a single request is made:
//!
//! 1. Only attributes some `filter:` actually reads are fetched, so a config
//!    with no filters enriches nothing at all.
//! 2. Only sources that feed a filtered list are considered.
//! 3. Only items that still lack one of those attributes after their own
//!    payload and the metadata store have been consulted are asked about.
//!
//! What is left is batched 200 ids to a request, and remembered for a month -
//! including the fact that a provider had never heard of an id. Steady state is
//! therefore zero requests; the one-off cost of a cold 5,000-title library is
//! about 25.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::config::{MetadataConfig, Runtime};
use crate::filter::Attribute;
use crate::model::{Attrs, Item, ItemKey, MediaType, Provider, normalize_imdb};
use crate::sources::mdblist::attrs_from_json;
use crate::state::SharedState;

/// MDBList accepts up to 200 ids per request, and bills one unit per request
/// regardless of how many that is.
const BATCH: usize = 200;

/// Ids are asked for under one provider at a time; this is the order of
/// preference, most widely understood first.
const PROVIDER_ORDER: [Provider; 4] = [
    Provider::Tmdb,
    Provider::Imdb,
    Provider::Trakt,
    Provider::Tvdb,
];

/// One batch's worth of work: a provider, a media type, and the ids to ask for.
type Bucket = (Provider, MediaType);

/// Run one enrichment pass. Errors are logged, never propagated: a provider
/// being down must not stop the refresher.
pub async fn enrich_due(state: &SharedState) {
    let runtime = state.runtime();
    let config = runtime.config.metadata.clone();
    if !config.enabled {
        return;
    }

    let wanted = outstanding(&runtime, state);
    if wanted.is_empty() {
        return;
    }

    let Some(apikey) = runtime.mdblist_any_apikey() else {
        tracing::warn!(
            titles = wanted.values().map(Vec::len).sum::<usize>(),
            "metadata is needed but no MDBList apikey is configured; \
             set `providers.mdblist.apikey` or filter on attributes the sources already carry"
        );
        return;
    };

    let base_url = runtime.mdblist_base_url();
    let mut requests = 0usize;
    let mut learned = 0usize;
    let mut unknown = 0usize;

    // Sequential on purpose: the budget and the provider's remaining daily
    // allowance are both running totals, and a handful of requests per pass is
    // not worth making them concurrent and approximate.
    'outer: for ((provider, media_type), keys) in wanted {
        for chunk in keys.chunks(BATCH) {
            if requests >= config.budget {
                tracing::info!(
                    budget = config.budget,
                    "metadata budget reached for this pass"
                );
                break 'outer;
            }

            let outcome =
                fetch_batch(&state.http, &base_url, &apikey, provider, media_type, chunk).await;
            requests += 1;

            match outcome {
                Ok(batch) => {
                    let (known, missing) = absorb(state, media_type, chunk, batch.items);
                    learned += known;
                    unknown += missing;
                    if let Some(remaining) = batch.remaining
                        && remaining < config.reserve
                    {
                        tracing::warn!(
                            remaining,
                            reserve = config.reserve,
                            "pausing metadata enrichment to leave room for list fetches"
                        );
                        break 'outer;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        provider = provider.as_str(),
                        media_type = media_type.as_str(),
                        ids = chunk.len(),
                        error = %format!("{error:#}"),
                        "metadata batch failed"
                    );
                    // A failure here is usually the quota or the API being
                    // down; either way the rest of the pass will fail too.
                    break 'outer;
                }
            }
        }
    }

    if requests == 0 {
        return;
    }
    tracing::info!(requests, learned, unknown, "enriched metadata");
    if let Err(error) = state.meta.save().await {
        tracing::warn!(error = %format!("{error:#}"), "could not persist metadata cache");
    }
}

/// The ids still worth asking about, bucketed by the route they would use.
fn outstanding(runtime: &Runtime, state: &SharedState) -> HashMap<Bucket, Vec<ItemKey>> {
    let config = &runtime.config.metadata;
    let mut out: HashMap<Bucket, Vec<ItemKey>> = HashMap::new();

    let mut seen: HashSet<ItemKey> = HashSet::new();
    for (source, attributes) in needed_per_source(runtime) {
        let Some(items) = state.cache.items(&source) else {
            continue;
        };
        for item in items.iter() {
            if satisfied(item, state, &attributes) {
                continue;
            }
            let Some(key) = preferred_key(item) else {
                continue;
            };
            if state.meta.is_fresh(&key, config.ttl, config.miss_ttl) {
                continue;
            }
            if seen.insert(key.clone()) {
                out.entry((key.provider, key.media_type))
                    .or_default()
                    .push(key);
            }
        }
    }
    out
}

/// For each source, the metadata attributes some list reading it will consult.
fn needed_per_source(runtime: &Runtime) -> HashMap<String, BTreeSet<Attribute>> {
    let mut needed: HashMap<String, BTreeSet<Attribute>> = HashMap::new();
    for list in runtime.lists.values() {
        let attributes: BTreeSet<Attribute> = list
            .required_attrs
            .iter()
            .copied()
            .filter(|attribute| attribute.needs_metadata())
            .collect();
        if attributes.is_empty() {
            continue;
        }
        for source in &list.source_deps {
            needed
                .entry(source.clone())
                .or_default()
                .extend(attributes.iter().copied());
        }
    }
    needed
}

/// Whether an item already answers every attribute, counting both what its own
/// source gave it and what the store has learned since.
fn satisfied(item: &Item, state: &SharedState, attributes: &BTreeSet<Attribute>) -> bool {
    if attributes
        .iter()
        .all(|attribute| attribute.present_in(&item.attrs))
    {
        return true;
    }
    let mut known = item.attrs.clone();
    if let Some(stored) = state.meta.lookup(item) {
        known.fill_from(&stored);
    }
    attributes
        .iter()
        .all(|attribute| attribute.present_in(&known))
}

/// The id to ask about, in order of how widely the provider understands them.
fn preferred_key(item: &Item) -> Option<ItemKey> {
    PROVIDER_ORDER.iter().find_map(|provider| {
        let value = match provider {
            Provider::Tmdb => item.ids.tmdb.map(|id| id.to_string()),
            Provider::Imdb => item.ids.imdb.clone(),
            Provider::Trakt => item.ids.trakt.map(|id| id.to_string()),
            Provider::Tvdb => item.ids.tvdb.map(|id| id.to_string()),
        }?;
        Some(ItemKey {
            media_type: item.media_type,
            provider: *provider,
            value,
        })
    })
}

struct Batch {
    items: Vec<Value>,
    /// `X-RateLimit-Remaining`, when the provider sent it.
    remaining: Option<u64>,
}

async fn fetch_batch(
    http: &reqwest::Client,
    base_url: &str,
    apikey: &str,
    provider: Provider,
    media_type: MediaType,
    keys: &[ItemKey],
) -> Result<Batch> {
    let url = format!(
        "{}/{}/{}",
        base_url.trim_end_matches('/'),
        provider.as_str(),
        media_type.as_str()
    );
    let ids: Vec<&str> = keys.iter().map(|key| key.value.as_str()).collect();

    let response = http
        .post(&url)
        .query(&[("apikey", apikey)])
        .json(&serde_json::json!({ "ids": ids }))
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;

    let status = response.status();
    let remaining = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok());
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.context("reading MDBList response")?;

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!(
            "MDBList daily quota exhausted{}",
            match retry_after {
                Some(seconds) => format!(", resets in {seconds}s"),
                None => String::new(),
            }
        );
    }
    if !status.is_success() {
        bail!(
            "MDBList returned {status} for {url}: {}",
            truncate(&body, 300)
        );
    }

    // The same 200-with-an-error-body quirk the list endpoint has.
    let parsed: Value = serde_json::from_str(&body)
        .with_context(|| format!("parsing MDBList response: {}", truncate(&body, 300)))?;
    if let Some(error) = parsed.get("error").and_then(Value::as_str) {
        bail!("MDBList returned an error: {error}");
    }
    let Value::Array(items) = parsed else {
        bail!(
            "expected a JSON array from {url}, got {}",
            truncate(&body, 300)
        );
    };

    Ok(Batch { items, remaining })
}

/// Store what came back, and remember the ids that did not: MDBList silently
/// omits titles it does not know, and without a negative entry those would be
/// re-requested on every single pass.
fn absorb(
    state: &SharedState,
    media_type: MediaType,
    asked: &[ItemKey],
    returned: Vec<Value>,
) -> (usize, usize) {
    let mut answered: HashSet<ItemKey> = HashSet::new();
    let mut known = 0usize;

    for raw in returned {
        // The response reports its own media type; trust it over the route so a
        // mis-typed item is not filed under the wrong key.
        let media_type = raw
            .get("type")
            .and_then(Value::as_str)
            .and_then(MediaType::parse)
            .unwrap_or(media_type);
        let keys = keys_from_response(&raw, media_type);
        if keys.is_empty() {
            continue;
        }
        let attrs = attrs_from_json(&raw);
        if attrs.is_empty() {
            // Answered, but with nothing usable. Still worth recording so it is
            // not asked about again tomorrow.
            state.meta.record(&keys, Some(Attrs::default()));
        } else {
            state.meta.record(&keys, Some(attrs));
            known += 1;
        }
        answered.extend(keys);
    }

    let missing: Vec<ItemKey> = asked
        .iter()
        .filter(|key| !answered.contains(*key))
        .cloned()
        .collect();
    let unknown = missing.len();
    for key in missing {
        state.meta.record(std::slice::from_ref(&key), None);
    }
    (known, unknown)
}

/// Every id the response carries, so the title is findable by any of them.
/// This is a free side benefit: an item that arrived from Radarr with only a
/// TMDb id gains its IMDb and Trakt ids for future lookups.
fn keys_from_response(raw: &Value, media_type: MediaType) -> Vec<ItemKey> {
    let Some(ids) = raw.get("ids") else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(4);
    let mut push = |provider: Provider, value: String| {
        out.push(ItemKey {
            media_type,
            provider,
            value,
        });
    };
    for (field, provider) in [
        ("tmdb", Provider::Tmdb),
        ("trakt", Provider::Trakt),
        ("tvdb", Provider::Tvdb),
    ] {
        if let Some(number) = ids.get(field).and_then(Value::as_u64) {
            push(provider, number.to_string());
        }
    }
    if let Some(imdb) = ids
        .get("imdb")
        .and_then(Value::as_str)
        .and_then(normalize_imdb)
    {
        push(Provider::Imdb, imdb);
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// Seed the store from disk. Called once, before the port is bound.
pub async fn prime(state: &SharedState) {
    let config: MetadataConfig = state.runtime().config.metadata.clone();
    if !config.enabled {
        return;
    }
    match state.meta.restore(config.ttl, config.miss_ttl).await {
        Ok(0) => {}
        Ok(count) => tracing::info!(titles = count, "restored metadata cache"),
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "could not read metadata cache")
        }
    }
}
