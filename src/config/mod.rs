//! Loading, environment interpolation and validation of `config.yml`.

pub mod model;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use indexmap::{IndexMap, IndexSet};

use crate::expr::{self, Expr};
use crate::model::parse_id_spec;
pub use model::*;

/// A validated configuration: every expression parsed, every name resolved.
#[derive(Debug)]
pub struct Runtime {
    pub path: PathBuf,
    pub config: Arc<Config>,
    pub lists: IndexMap<String, CompiledList>,
    /// All lists in dependency order, dependencies first.
    pub list_order: Vec<String>,
}

#[derive(Debug)]
pub struct CompiledList {
    pub name: String,
    pub expr: Expr,
    /// Lists this list needs, transitively, in evaluation order.
    pub list_deps: Vec<String>,
    /// Sources this list needs, transitively.
    pub source_deps: Vec<String>,
}

impl Runtime {
    pub fn list(&self, name: &str) -> Option<&CompiledList> {
        self.lists.get(name)
    }

    pub fn source(&self, name: &str) -> Option<&SourceConfig> {
        self.config.sources.get(name)
    }

    /// The refresh interval for a source: its own `ttl`, else the global default.
    pub fn ttl_for(&self, name: &str) -> Duration {
        self.source(name)
            .and_then(SourceConfig::ttl)
            .unwrap_or(self.config.cache.default_ttl)
    }

    pub fn mdblist_apikey(&self, source: &MdblistSource) -> Option<String> {
        source.apikey.clone().or_else(|| {
            self.config
                .providers
                .mdblist
                .as_ref()
                .map(|provider| provider.apikey.clone())
        })
    }

    pub fn mdblist_base_url(&self) -> String {
        self.config
            .providers
            .mdblist
            .as_ref()
            .map(|provider| provider.base_url.clone())
            .unwrap_or_else(|| "https://api.mdblist.com".to_string())
    }
}

pub fn load(path: &Path) -> Result<Runtime> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    let config =
        parse_str(&raw).with_context(|| format!("reading config file {}", path.display()))?;
    compile(config, path.to_path_buf())
        .with_context(|| format!("validating config file {}", path.display()))
}

pub fn parse_str(raw: &str) -> Result<Config> {
    let mut document: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(raw).context("parsing YAML")?;
    if document.is_null() {
        // A file that is entirely comments is a valid, empty configuration.
        return Ok(Config::default());
    }
    interpolate_document(&mut document)?;
    serde_yaml_ng::from_value(document).context("reading config")
}

/// Expand `${VAR}` in every scalar of a parsed document.
///
/// Substituting into the raw text instead would also rewrite comments, so a
/// comment mentioning `${SOME_VAR}` would demand that the variable exist.
fn interpolate_document(value: &mut serde_yaml_ng::Value) -> Result<()> {
    use serde_yaml_ng::{Mapping, Value};

    match value {
        Value::String(text) => {
            let expanded = interpolate_env(text)?;
            // A scalar that is nothing but one `${…}` takes the type of what it
            // expanded to, so `port: ${PORT:-9797}` is still a number.
            *value = match (expanded != *text && is_single_expansion(text))
                .then(|| serde_yaml_ng::from_str::<Value>(&expanded))
            {
                // An unset variable with an empty fallback is an empty string,
                // not a null - `apikey: ${KEY:-}` must stay a string.
                Some(Ok(parsed)) if !parsed.is_null() => parsed,
                _ => Value::String(expanded),
            };
        }
        Value::Sequence(items) => {
            for item in items {
                interpolate_document(item)?;
            }
        }
        Value::Mapping(mapping) => {
            let mut rebuilt = Mapping::new();
            for (mut key, mut entry) in std::mem::take(mapping) {
                interpolate_document(&mut key)?;
                interpolate_document(&mut entry)?;
                rebuilt.insert(key, entry);
            }
            *mapping = rebuilt;
        }
        _ => {}
    }
    Ok(())
}

fn is_single_expansion(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("${") && trimmed.ends_with('}') && !trimmed[2..].contains("${")
}

/// Expand `${VAR}` and `${VAR:-fallback}`; `$${` is a literal `${`.
pub fn interpolate_env(raw: &str) -> Result<String> {
    interpolate_with(raw, |name| std::env::var(name).ok())
}

fn interpolate_with(raw: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'$') && chars.get(i + 2) == Some(&'{') {
            out.push_str("${");
            i += 3;
            continue;
        }
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            let Some(close) = chars[i..].iter().position(|c| *c == '}') else {
                bail!("unterminated `${{` in config");
            };
            let inner: String = chars[i + 2..i + close].iter().collect();
            let (name, fallback) = match inner.split_once(":-") {
                Some((name, fallback)) => (name.trim(), Some(fallback.to_string())),
                None => (inner.trim(), None),
            };
            if name.is_empty() {
                bail!("empty variable name in `${{}}`");
            }
            let value = lookup(name).or(fallback).ok_or_else(|| {
                anyhow::anyhow!(
                    "environment variable `{name}` is referenced by the config but not set \
                     (use `${{{name}:-default}}` to give it a fallback)"
                )
            })?;
            out.push_str(&value);
            i += close + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }

    Ok(out)
}

/// Validate a parsed config and pre-compile every list expression.
///
/// All problems are collected and reported together - fixing a config one error
/// per restart is miserable.
pub fn compile(config: Config, path: PathBuf) -> Result<Runtime> {
    let mut problems: Vec<String> = Vec::new();

    for (name, source) in &config.sources {
        let unknown = unknown_keys(source.extra());
        if !unknown.is_empty() {
            problems.push(format!(
                "source `{name}`: unknown option(s) {}",
                quoted_list(&unknown)
            ));
        }
        validate_source(name, source, &config, &mut problems);
    }

    for (name, list) in &config.lists {
        if config.sources.contains_key(name) {
            problems.push(format!(
                "`{name}` is used for both a source and a list; names must be unique"
            ));
        }
        validate_list_options(name, list, &mut problems);
    }

    // Parse every expression before resolving names so a syntax error does not
    // hide behind a name error.
    let mut parsed: IndexMap<String, Expr> = IndexMap::new();
    for (name, list) in &config.lists {
        match expr::parse(&list.expr) {
            Ok(expression) => {
                parsed.insert(name.clone(), expression);
            }
            Err(error) => problems.push(format!("list `{name}`: {error} in `{}`", list.expr)),
        }
    }

    for (name, expression) in &parsed {
        for referenced in expression.names() {
            if !config.sources.contains_key(referenced) && !config.lists.contains_key(referenced) {
                problems.push(format!(
                    "list `{name}` references `{referenced}`, which is not a source or a list{}",
                    dash_hint(referenced, &config)
                ));
            }
        }
    }

    let order = topological_order(&parsed, &config, &mut problems);

    if !problems.is_empty() {
        bail!(
            "config has {} problem(s):\n  - {}",
            problems.len(),
            problems.join("\n  - ")
        );
    }

    // Dependency-first order means a list's dependencies are already resolved
    // by the time it is expanded.
    let mut lists: IndexMap<String, CompiledList> = IndexMap::new();
    for name in &order {
        let expression = parsed.get(name).expect("every ordered list was parsed");
        let mut list_deps: IndexSet<String> = IndexSet::new();
        let mut source_deps: IndexSet<String> = IndexSet::new();
        for referenced in expression.names() {
            if config.sources.contains_key(referenced) {
                source_deps.insert(referenced.to_string());
            } else if let Some(dependency) = lists.get(referenced) {
                for inherited in &dependency.list_deps {
                    list_deps.insert(inherited.clone());
                }
                list_deps.insert(referenced.to_string());
                for inherited in &dependency.source_deps {
                    source_deps.insert(inherited.clone());
                }
            }
        }
        lists.insert(
            name.clone(),
            CompiledList {
                name: name.clone(),
                expr: expression.clone(),
                list_deps: list_deps.into_iter().collect(),
                source_deps: source_deps.into_iter().collect(),
            },
        );
    }

    Ok(Runtime {
        path,
        config: Arc::new(config),
        lists,
        list_order: order,
    })
}

fn validate_source(name: &str, source: &SourceConfig, config: &Config, problems: &mut Vec<String>) {
    match source {
        SourceConfig::Mdblist(mdblist) => {
            let addressed = [
                mdblist.list.is_some(),
                mdblist.list_id.is_some(),
                mdblist.url.is_some(),
            ]
            .iter()
            .filter(|set| **set)
            .count();
            if addressed == 0 {
                problems.push(format!(
                    "source `{name}`: set one of `list: user/listname`, `list_id: 1234` or `url:`"
                ));
            } else if addressed > 1 {
                problems.push(format!(
                    "source `{name}`: `list`, `list_id` and `url` are alternatives, set only one"
                ));
            }
            if let Some(list) = &mdblist.list
                && !list.contains('/')
            {
                problems.push(format!(
                    "source `{name}`: `list` must be `username/listname`, got `{list}`"
                ));
            }
            let apikey = mdblist.apikey.as_deref().or_else(|| {
                config
                    .providers
                    .mdblist
                    .as_ref()
                    .map(|provider| provider.apikey.as_str())
            });
            if apikey.is_none_or(str::is_empty) {
                problems.push(format!(
                    "source `{name}`: no MDBList API key; set `providers.mdblist.apikey` \
                     or `apikey` on the source"
                ));
            }
        }
        SourceConfig::Static(static_source) => {
            if static_source.items.is_empty() {
                problems.push(format!("source `{name}`: `items` is empty"));
            }
            for spec in &static_source.items {
                if let Err(error) = parse_id_spec(spec) {
                    problems.push(format!("source `{name}`: {error}"));
                }
            }
        }
        SourceConfig::Json(json) => {
            if json.url.trim().is_empty() {
                problems.push(format!("source `{name}`: `url` is empty"));
            }
        }
        SourceConfig::Radarr(arr) | SourceConfig::Sonarr(arr) => {
            if arr.url.trim().is_empty() {
                problems.push(format!("source `{name}`: `url` is empty"));
            }
            if arr.api_key.trim().is_empty() {
                problems.push(format!("source `{name}`: `api_key` is empty"));
            }
        }
    }
}

fn validate_list_options(name: &str, list: &ListConfig, problems: &mut Vec<String>) {
    if let (Some(min), Some(max)) = (list.min_year, list.max_year)
        && min > max
    {
        problems.push(format!(
            "list `{name}`: min_year {min} is after max_year {max}"
        ));
    }
    if let (Some(after), Some(before)) = (list.released_after, list.released_before)
        && after > before
    {
        problems.push(format!(
            "list `{name}`: released_after {after} is after released_before {before}"
        ));
    }
    if list.limit == Some(0) {
        problems.push(format!("list `{name}`: `limit: 0` would always be empty"));
    }
}

/// If an unknown name is a prefix of a configured name that contains a dash,
/// the user almost certainly wrote `top-250` where `"top-250"` was needed.
fn dash_hint(referenced: &str, config: &Config) -> String {
    let candidate = config
        .sources
        .keys()
        .chain(config.lists.keys())
        .find(|name| name.contains('-') && name.starts_with(referenced));
    match candidate {
        Some(name) => format!(" (`-` is the difference operator; quote it as `\"{name}\"`)"),
        None => String::new(),
    }
}

/// Dependency-first ordering of lists, reporting any cycle it finds.
fn topological_order(
    parsed: &IndexMap<String, Expr>,
    config: &Config,
    problems: &mut Vec<String>,
) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        InProgress,
        Done,
    }

    let mut marks: HashMap<&str, Mark> = parsed
        .keys()
        .map(|k| (k.as_str(), Mark::Unvisited))
        .collect();
    let mut order: Vec<String> = Vec::new();
    let mut stack: Vec<&str> = Vec::new();

    fn visit<'a>(
        name: &'a str,
        parsed: &'a IndexMap<String, Expr>,
        config: &Config,
        marks: &mut HashMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
        order: &mut Vec<String>,
        problems: &mut Vec<String>,
    ) {
        match marks.get(name).copied().unwrap_or(Mark::Done) {
            Mark::Done => return,
            Mark::InProgress => {
                let start = stack.iter().position(|entry| *entry == name).unwrap_or(0);
                let mut cycle: Vec<&str> = stack[start..].to_vec();
                cycle.push(name);
                problems.push(format!("lists form a cycle: {}", cycle.join(" -> ")));
                return;
            }
            Mark::Unvisited => {}
        }

        marks.insert(name, Mark::InProgress);
        stack.push(name);
        if let Some(expression) = parsed.get(name) {
            for referenced in expression.names() {
                if config.lists.contains_key(referenced) {
                    // Borrow the key owned by `parsed` so the lifetime outlives this call.
                    if let Some((key, _)) = parsed.get_key_value(referenced) {
                        visit(key.as_str(), parsed, config, marks, stack, order, problems);
                    }
                }
            }
        }
        stack.pop();
        marks.insert(name, Mark::Done);
        order.push(name.to_string());
    }

    for name in parsed.keys() {
        visit(
            name.as_str(),
            parsed,
            config,
            &mut marks,
            &mut stack,
            &mut order,
            problems,
        );
    }

    order
}

fn quoted_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile_str(raw: &str) -> Result<Runtime> {
        let config = parse_str(raw)?;
        compile(config, PathBuf::from("test.yml"))
    }

    /// The whole context chain, the way the binary reports it.
    fn compile_err(raw: &str) -> String {
        format!("{:#}", compile_str(raw).unwrap_err())
    }

    const SAMPLE: &str = r#"
sources:
  trending:
    type: static
    media_type: movie
    items: ["tmdb:1", "tmdb:2", "tmdb:3"]
  owned:
    type: static
    media_type: movie
    items: ["tmdb:2"]
lists:
  wanted:
    expr: "trending - owned"
  wanted_top:
    expr: "wanted & trending"
    limit: 10
"#;

    #[test]
    fn a_valid_config_compiles_with_dependencies_resolved() {
        let runtime = compile_str(SAMPLE).expect("compiles");
        assert_eq!(runtime.list_order, vec!["wanted", "wanted_top"]);
        let top = runtime.list("wanted_top").unwrap();
        assert_eq!(top.list_deps, vec!["wanted"]);
        assert_eq!(top.source_deps, vec!["trending", "owned"]);
        assert_eq!(runtime.config.server.port, 9797);
        assert_eq!(
            runtime.ttl_for("trending"),
            runtime.config.cache.default_ttl
        );
    }

    #[test]
    fn environment_variables_are_interpolated() {
        let expanded = interpolate_with("Bearer ${TOKEN}", |name| {
            (name == "TOKEN").then(|| "secret".to_string())
        })
        .unwrap();
        assert_eq!(expanded, "Bearer secret");
    }

    #[test]
    fn interpolation_supports_fallbacks_and_escaping() {
        assert_eq!(interpolate_with("${PORT:-9797}", |_| None).unwrap(), "9797");
        assert_eq!(
            interpolate_with("$${NOT_A_VAR}", |_| None).unwrap(),
            "${NOT_A_VAR}"
        );
    }

    #[test]
    fn a_missing_environment_variable_is_an_error() {
        let error = interpolate_with("${NOPE}", |_| None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("NOPE"), "{error}");
        assert!(interpolate_with("${UNCLOSED", |_| None).is_err());
    }

    #[test]
    fn comments_are_never_interpolated() {
        // The starter config documents `${VAR}` in a comment; needing that
        // variable to exist would make a fresh install fail to boot.
        let runtime = compile_str(
            r#"
# Values may contain ${SOME_VAR_THAT_IS_NOT_SET}.
sources:
  a: { type: static, items: ["tmdb:1"] }  # and ${NOR_IS_THIS}
lists: {}
"#,
        );
        assert!(runtime.is_ok(), "{:#}", runtime.unwrap_err());
    }

    #[test]
    fn an_expanded_scalar_keeps_the_type_it_expands_to() {
        unsafe { std::env::set_var("REPSETARR_TEST_PORT", "8123") };
        let config = parse_str("server:\n  port: ${REPSETARR_TEST_PORT}\n").unwrap();
        assert_eq!(config.server.port, 8123);
        unsafe { std::env::remove_var("REPSETARR_TEST_PORT") };
    }

    #[test]
    fn an_empty_expansion_stays_an_empty_string() {
        let config = parse_str("providers:\n  mdblist:\n    apikey: ${REPSETARR_UNSET_KEY:-}\n")
            .expect("parses");
        assert_eq!(config.providers.mdblist.unwrap().apikey, "");
    }

    #[test]
    fn a_config_of_only_comments_is_an_empty_config() {
        let runtime = compile_str("# nothing here\n").unwrap();
        assert!(runtime.config.sources.is_empty());
        assert!(runtime.config.lists.is_empty());
    }

    #[test]
    fn unknown_names_are_reported() {
        let error = compile_err(
            r#"
sources:
  a: { type: static, items: ["tmdb:1"] }
lists:
  bad: { expr: "a - ghost" }
"#,
        );
        assert!(error.contains("`ghost`"), "{error}");
    }

    #[test]
    fn a_dashed_name_gets_a_quoting_hint() {
        let error = compile_err(
            r#"
sources:
  top-250: { type: static, items: ["tmdb:1"] }
lists:
  bad: { expr: "top-250" }
"#,
        );
        assert!(error.contains("quote it as"), "{error}");
    }

    #[test]
    fn cycles_between_lists_are_reported() {
        let error = compile_err(
            r#"
sources:
  a: { type: static, items: ["tmdb:1"] }
lists:
  one: { expr: "a | two" }
  two: { expr: "a | one" }
"#,
        );
        assert!(error.contains("cycle"), "{error}");
    }

    #[test]
    fn a_list_may_not_share_a_name_with_a_source() {
        let error = compile_err(
            r#"
sources:
  same: { type: static, items: ["tmdb:1"] }
lists:
  same: { expr: "same" }
"#,
        );
        assert!(error.contains("names must be unique"), "{error}");
    }

    #[test]
    fn syntax_errors_name_the_list_and_the_column() {
        let error = compile_err(
            r#"
sources:
  a: { type: static, items: ["tmdb:1"] }
lists:
  bad: { expr: "a - " }
"#,
        );
        assert!(error.contains("list `bad`"), "{error}");
        assert!(error.contains("column"), "{error}");
    }

    #[test]
    fn unknown_source_options_are_rejected() {
        let error = compile_err(
            r#"
sources:
  a: { type: static, items: ["tmdb:1"], mediatype: movie }
lists: {}
"#,
        );
        assert!(error.contains("`mediatype`"), "{error}");
    }

    #[test]
    fn unknown_top_level_options_are_rejected() {
        let error = compile_err("serverr:\n  port: 1\n");
        assert!(error.contains("serverr"), "{error}");
    }

    #[test]
    fn bad_static_ids_fail_at_load_not_at_request_time() {
        let error = compile_err(
            r#"
sources:
  a: { type: static, items: ["550"] }
lists: {}
"#,
        );
        assert!(error.contains("provider:id"), "{error}");
    }

    #[test]
    fn mdblist_sources_need_an_api_key_and_exactly_one_address() {
        let error = compile_err(
            r#"
sources:
  a: { type: mdblist, list: "user/list" }
lists: {}
"#,
        );
        assert!(error.contains("API key"), "{error}");

        let error = compile_err(
            r#"
providers:
  mdblist: { apikey: "k" }
sources:
  a: { type: mdblist, list: "user/list", list_id: 5 }
lists: {}
"#,
        );
        assert!(error.contains("only one"), "{error}");
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let error = compile_err(
            r#"
sources:
  a: { type: static, items: [] }
lists:
  one: { expr: "ghost" }
  two: { expr: "a - " }
"#,
        );
        assert!(error.contains("3 problem(s)"), "{error}");
    }

    #[test]
    fn list_option_ranges_are_checked() {
        let error = compile_err(
            r#"
sources:
  a: { type: static, items: ["tmdb:1"] }
lists:
  bad: { expr: "a", min_year: 2020, max_year: 2000, limit: 0 }
"#,
        );
        assert!(error.contains("min_year"), "{error}");
        assert!(error.contains("limit: 0"), "{error}");
    }
}
