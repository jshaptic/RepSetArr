//! Item identity: two items are the same when they share *any* external id.
//!
//! Sources rarely agree on which ids they carry - MDBList gives TMDb+IMDb,
//! Sonarr gives TVDb+IMDb, a homemade JSON feed may give IMDb only. Matching on
//! a single provider would silently split one title into two. Instead every id
//! an item carries is a key, all keys of one item are linked, and linked keys
//! form a group. Matching is therefore transitive: an IMDb-only item bridges a
//! TMDb-only item and a TVDb-only item that both share its IMDb id.

use std::collections::HashMap;

use crate::model::{Item, ItemKey};

/// Identifies one real-world title across every configured source.
pub type GroupId = usize;

/// A slot handed out by [`Interner::insert`]. Slots are merged as more items
/// arrive, so a slot is only turned into a [`GroupId`] by [`Interner::resolve`]
/// once every item has been inserted.
pub type SlotId = usize;

#[derive(Debug, Default)]
pub struct Interner {
    parent: Vec<usize>,
    items: Vec<Item>,
    keys: HashMap<ItemKey, SlotId>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an item, merging it into any group it shares an id with.
    ///
    /// Items with no ids at all cannot be identified and are rejected.
    pub fn insert(&mut self, item: &Item) -> Option<SlotId> {
        let keys = item.keys();
        if keys.is_empty() {
            return None;
        }

        // Every group any of this item's keys already belongs to.
        let mut roots: Vec<usize> = Vec::new();
        for key in &keys {
            if let Some(&slot) = self.keys.get(key) {
                let root = self.find(slot);
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }

        let slot = match roots.split_first() {
            None => {
                let slot = self.items.len();
                self.parent.push(slot);
                self.items.push(item.clone());
                slot
            }
            Some((&first, rest)) => {
                let mut root = first;
                // This item bridges groups that were previously unrelated.
                for &other in rest {
                    root = self.union(root, other);
                }
                self.items[root].merge_from(item);
                root
            }
        };

        for key in keys {
            self.keys.entry(key).or_insert(slot);
        }
        Some(slot)
    }

    fn find(&mut self, mut node: usize) -> usize {
        while self.parent[node] != node {
            self.parent[node] = self.parent[self.parent[node]];
            node = self.parent[node];
        }
        node
    }

    fn union(&mut self, a: usize, b: usize) -> usize {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return a;
        }
        // The older slot always survives, so the item seen first keeps its
        // title and year and group ids stay in first-seen order.
        let (keep, absorb) = if a < b { (a, b) } else { (b, a) };
        self.parent[absorb] = keep;
        let absorbed = self.items[absorb].clone();
        self.items[keep].merge_from(&absorbed);
        keep
    }

    /// The group a slot ended up in, after all merges.
    pub fn resolve(&mut self, slot: SlotId) -> GroupId {
        self.find(slot)
    }

    pub fn item(&self, group: GroupId) -> &Item {
        &self.items[group]
    }

    /// Every interned item, so a later pass can fill in what the sources did
    /// not carry. Non-root slots are visited too, which is harmless: only roots
    /// are ever read back through [`Interner::item`].
    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.items.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaIds, MediaType};

    fn item(
        media_type: MediaType,
        tmdb: Option<u32>,
        imdb: Option<&str>,
        tvdb: Option<u32>,
    ) -> Item {
        Item {
            media_type,
            ids: MediaIds {
                tmdb,
                imdb: imdb.map(str::to_string),
                tvdb,
                trakt: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_shared_id_merges_two_items() {
        let mut interner = Interner::new();
        let a = interner
            .insert(&item(MediaType::Movie, Some(550), None, None))
            .unwrap();
        let b = interner
            .insert(&item(MediaType::Movie, Some(550), Some("tt0137523"), None))
            .unwrap();
        assert_eq!(interner.resolve(a), interner.resolve(b));
        let group = interner.resolve(a);
        assert_eq!(interner.item(group).ids.imdb.as_deref(), Some("tt0137523"));
    }

    #[test]
    fn matching_is_transitive_across_disjoint_id_sets() {
        let mut interner = Interner::new();
        // A knows only TMDb, C knows only TVDb; B bridges them via IMDb.
        let a = interner
            .insert(&item(MediaType::Show, Some(1396), None, None))
            .unwrap();
        let c = interner
            .insert(&item(MediaType::Show, None, None, Some(81189)))
            .unwrap();
        assert_ne!(interner.resolve(a), interner.resolve(c));
        let b = interner
            .insert(&item(
                MediaType::Show,
                Some(1396),
                Some("tt0903747"),
                Some(81189),
            ))
            .unwrap();
        assert_eq!(interner.resolve(a), interner.resolve(c));
        assert_eq!(interner.resolve(a), interner.resolve(b));
        assert_eq!(
            interner.len(),
            2,
            "two slots exist, but they resolve to one group"
        );
    }

    #[test]
    fn the_same_tmdb_id_on_different_media_types_stays_separate() {
        let mut interner = Interner::new();
        let movie = interner
            .insert(&item(MediaType::Movie, Some(550), None, None))
            .unwrap();
        let show = interner
            .insert(&item(MediaType::Show, Some(550), None, None))
            .unwrap();
        assert_ne!(interner.resolve(movie), interner.resolve(show));
    }

    #[test]
    fn items_without_any_id_are_rejected() {
        let mut interner = Interner::new();
        assert!(interner.insert(&Item::new(MediaType::Movie)).is_none());
    }

    #[test]
    fn the_first_item_wins_on_conflicting_metadata() {
        let mut interner = Interner::new();
        let mut first = item(MediaType::Movie, Some(550), None, None);
        first.title = Some("Fight Club".into());
        let mut second = item(MediaType::Movie, Some(550), None, None);
        second.title = Some("Bojovy Klub".into());
        second.year = Some(1999);
        let a = interner.insert(&first).unwrap();
        interner.insert(&second).unwrap();
        let group = interner.resolve(a);
        assert_eq!(interner.item(group).title.as_deref(), Some("Fight Club"));
        assert_eq!(interner.item(group).year, Some(1999));
    }
}
