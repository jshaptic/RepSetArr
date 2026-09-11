//! Where items come from. Adding a provider means adding a module here and a
//! variant to [`crate::config::SourceConfig`].

pub mod arr;
pub mod json_url;
pub mod json_value;
pub mod mdblist;
pub mod static_list;

use anyhow::{Context, Result};

use crate::config::{Runtime, SourceConfig};
use crate::model::{Item, MediaType};

/// Fetch one source's current contents.
pub async fn fetch(runtime: &Runtime, name: &str, http: &reqwest::Client) -> Result<Vec<Item>> {
    let source = runtime
        .source(name)
        .with_context(|| format!("no source named `{name}`"))?;

    let items = match source {
        SourceConfig::Static(static_source) => static_list::fetch(static_source)?,
        SourceConfig::Mdblist(mdblist_source) => {
            let apikey = runtime
                .mdblist_apikey(mdblist_source)
                .context("no MDBList API key configured")?;
            mdblist::fetch(http, &runtime.mdblist_base_url(), &apikey, mdblist_source).await?
        }
        SourceConfig::Json(json_source) => json_url::fetch(http, json_source).await?,
        SourceConfig::Radarr(arr_source) => arr::fetch(http, arr_source, MediaType::Movie).await?,
        SourceConfig::Sonarr(arr_source) => arr::fetch(http, arr_source, MediaType::Show).await?,
    };

    Ok(items)
}
