//! Items written directly in the config: `items: ["tmdb:550", "imdb:tt0133093"]`.

use anyhow::{Context, Result};

use crate::config::StaticSource;
use crate::model::{Item, parse_id_spec};

pub fn fetch(source: &StaticSource) -> Result<Vec<Item>> {
    source
        .items
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let ids = parse_id_spec(spec)
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("static item #{}", index + 1))?;
            Ok(Item {
                media_type: source.media_type,
                ids,
                rank: Some(index as u32 + 1),
                ..Default::default()
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MediaType;

    #[test]
    fn specs_become_ranked_items() {
        let source = StaticSource {
            items: vec!["tmdb:550".into(), "imdb:tt0133093".into()],
            media_type: MediaType::Movie,
            extra: Default::default(),
        };
        let items = fetch(&source).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].ids.tmdb, Some(550));
        assert_eq!(items[0].rank, Some(1));
        assert_eq!(items[1].ids.imdb.as_deref(), Some("tt0133093"));
        assert_eq!(items[1].media_type, MediaType::Movie);
    }
}
