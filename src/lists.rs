//! Turning a configured list into items: fetch, identify, evaluate, post-process.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use indexmap::IndexSet;

use crate::cache::CacheStore;
use crate::config::{CompiledList, ListConfig, Runtime, SortKey, SortOrder};
use crate::expr::{self, Set};
use crate::identity::Interner;
use crate::meta::MetaStore;
use crate::model::Item;

#[derive(Debug)]
pub struct Evaluation {
    pub name: String,
    pub items: Vec<Item>,
    /// Sources whose data is past its TTL - the answer is still served, but the
    /// caller is told about it.
    pub stale_sources: Vec<String>,
    /// Items the list's filter had to judge without knowing every attribute it
    /// reads, because the enricher has not reached them yet. A non-zero count
    /// means the answer is provisional, not wrong.
    pub unenriched: usize,
    pub evaluated_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("no list named `{0}`")]
    UnknownList(String),
    #[error("no data has been fetched yet for source(s): {}", .0.join(", "))]
    NoData(Vec<String>),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Every configured list, evaluated in one pass.
///
/// One [`Interner`] covers every source, so identity is resolved once for the
/// whole config rather than once per list. That is what lets a single walk of
/// `list_order` serve every list: sets built under different interners have
/// incomparable group ids and cannot be shared.
///
/// Failure is per list, not per pass. A list whose sources have not been
/// fetched yet gets its own [`EvalError::NoData`] while every other list is
/// still answered, and because `source_deps` is transitive, a list that depends
/// on an unanswerable one fails for the same reason without needing a second
/// traversal.
pub fn evaluate_all(
    runtime: &Runtime,
    cache: &CacheStore,
    meta: &MetaStore,
) -> HashMap<String, Result<Evaluation, EvalError>> {
    // One interner for every source: items from different sources are only
    // comparable once they have been through the same identity pass.
    let mut interner = Interner::new();
    let mut slots: Vec<(String, Vec<usize>)> = Vec::with_capacity(runtime.config.sources.len());
    let mut missing: HashSet<&str> = HashSet::new();
    let mut stale: HashSet<&str> = HashSet::new();

    for source_name in runtime.config.sources.keys() {
        let status = cache.status(source_name);
        match status.items.clone() {
            None => {
                missing.insert(source_name);
            }
            Some(items) => {
                if status.is_stale(runtime.ttl_for(source_name)) {
                    stale.insert(source_name);
                }
                let source_slots = items
                    .iter()
                    .filter_map(|item| interner.insert(item))
                    .collect();
                slots.push((source_name.clone(), source_slots));
            }
        }
    }

    // Overlay what the enricher has learned. This reads the store only; a
    // request never fetches, so a cold store means a provisional answer rather
    // than a slow one.
    if runtime
        .lists
        .values()
        .any(|list| !list.required_attrs.is_empty())
    {
        for item in interner.items_mut() {
            if let Some(attrs) = meta.lookup(item) {
                item.attrs.fill_from(&attrs);
            }
        }
    }

    let mut sets: HashMap<String, Set> = HashMap::new();
    for (source_name, source_slots) in slots {
        let set: Set = source_slots
            .into_iter()
            .map(|slot| interner.resolve(slot))
            .collect();
        sets.insert(source_name, set);
    }

    let evaluated_at = Utc::now();
    let mut results: HashMap<String, Result<Evaluation, EvalError>> = HashMap::new();

    // Dependency order, so a list that references another list sees the other
    // list's post-processed result.
    for list_name in &runtime.list_order {
        let (Some(list), Some(config)) =
            (runtime.list(list_name), runtime.config.lists.get(list_name))
        else {
            continue;
        };

        // `source_deps` is transitive, so this also catches a list whose only
        // problem is a dependency list that cannot be answered.
        let blocked: Vec<String> = list
            .source_deps
            .iter()
            .filter(|source| missing.contains(source.as_str()))
            .cloned()
            .collect();
        if !blocked.is_empty() {
            results.insert(list_name.clone(), Err(EvalError::NoData(blocked)));
            continue;
        }

        let evaluated = match expr::eval(&list.expr, &sets) {
            Ok(set) => set,
            Err(error) => {
                results.insert(list_name.clone(), Err(EvalError::Other(error.into())));
                continue;
            }
        };
        let (processed, unenriched) = post_process(evaluated, list, config, &interner);

        let items: Vec<Item> = processed
            .iter()
            .map(|group| interner.item(*group).clone())
            .collect();
        let stale_sources: Vec<String> = list
            .source_deps
            .iter()
            .filter(|source| stale.contains(source.as_str()))
            .cloned()
            .collect();

        results.insert(
            list_name.clone(),
            Ok(Evaluation {
                name: list_name.clone(),
                items,
                stale_sources,
                unenriched,
                evaluated_at,
            }),
        );
        sets.insert(list_name.clone(), processed);
    }

    results
}

pub fn evaluate(
    runtime: &Runtime,
    cache: &CacheStore,
    meta: &MetaStore,
    name: &str,
) -> Result<Evaluation, EvalError> {
    if runtime.list(name).is_none() {
        return Err(EvalError::UnknownList(name.to_string()));
    }
    evaluate_all(runtime, cache, meta)
        .remove(name)
        .unwrap_or_else(|| Err(EvalError::UnknownList(name.to_string())))
}

/// Filter, sort and limit - applied after the algebra, so a list used inside
/// another expression contributes its post-processed contents.
///
/// Also reports how many *candidates* the filter had to judge without knowing
/// every attribute it reads. Counting before the filter rather than after is
/// the point: an item dropped for an unknown country is exactly the item the
/// caller needs to be told about.
fn post_process(
    set: Set,
    list: &CompiledList,
    config: &ListConfig,
    interner: &Interner,
) -> (Set, usize) {
    let mut unenriched = 0usize;
    let mut ids: Vec<usize> = set
        .into_iter()
        .filter(|group| {
            let item = interner.item(*group);
            if list
                .required_attrs
                .iter()
                .any(|attribute| !attribute.present_in(&item.attrs))
            {
                unenriched += 1;
            }
            keep(item, config)
                && list
                    .filter
                    .as_ref()
                    .is_none_or(|program| program.matches(item))
        })
        .collect();

    match config.sort {
        SortKey::None => {}
        SortKey::Rank => ids.sort_by_key(|group| interner.item(*group).rank.unwrap_or(u32::MAX)),
        SortKey::Title => ids.sort_by_key(|group| interner.item(*group).sort_title()),
        SortKey::Year => {
            ids.sort_by_key(|group| interner.item(*group).effective_year().unwrap_or(i32::MAX))
        }
        SortKey::Released => ids.sort_by_key(|group| {
            interner
                .item(*group)
                .released
                .unwrap_or(chrono::NaiveDate::MAX)
        }),
        SortKey::Random => {
            use rand::seq::SliceRandom;
            ids.shuffle(&mut rand::rng());
        }
    }

    if config.order == SortOrder::Desc && config.sort != SortKey::Random {
        ids.reverse();
    }

    if let Some(limit) = config.limit {
        ids.truncate(limit);
    }

    (ids.into_iter().collect::<IndexSet<usize>>(), unenriched)
}

fn keep(item: &Item, config: &ListConfig) -> bool {
    if !config.media_type.matches(item.media_type) {
        return false;
    }
    if let Some(min) = config.min_year
        && item.effective_year().is_none_or(|year| year < min)
    {
        return false;
    }
    if let Some(max) = config.max_year
        && item.effective_year().is_none_or(|year| year > max)
    {
        return false;
    }
    if let Some(after) = config.released_after
        && item.released.is_none_or(|released| released < after)
    {
        return false;
    }
    if let Some(before) = config.released_before
        && item.released.is_none_or(|released| released > before)
    {
        return false;
    }
    true
}
