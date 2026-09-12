//! A single `attribute[.modifier]: value` line, and the named block of them.
//!
//! Within one block every condition must hold (AND); within one condition the
//! comma-separated values are alternatives (OR). That is Kometa's rule, and it
//! is the one most people writing these filters already have in their head.

use chrono::NaiveDate;
use regex::RegexBuilder;
use serde_yaml_ng::Value;

use super::attr::{Attribute, Kind, Reading, known_attribute_names};
use crate::model::Item;

/// What a condition answers when the item simply does not know the attribute.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownPolicy {
    /// Unknown never matches, so the item is filtered out. Consistent with the
    /// way `min_year` already drops items that have no year.
    #[default]
    Exclude,
    Include,
}

impl UnknownPolicy {
    fn allows(self) -> bool {
        self == UnknownPolicy::Include
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    /// No suffix: any-of for codes, equality for numbers, contains for text.
    Is,
    Not,
    /// Every listed value must be present. Multi-valued attributes only.
    All,
    Gt,
    Gte,
    Lt,
    Lte,
    Before,
    After,
    Begins,
    Ends,
    Regex,
}

impl Modifier {
    fn parse(raw: &str) -> Option<Modifier> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "is" => Some(Modifier::Is),
            "not" | "isnot" => Some(Modifier::Not),
            "all" => Some(Modifier::All),
            "gt" => Some(Modifier::Gt),
            "gte" => Some(Modifier::Gte),
            "lt" => Some(Modifier::Lt),
            "lte" => Some(Modifier::Lte),
            "before" => Some(Modifier::Before),
            "after" => Some(Modifier::After),
            "begins" => Some(Modifier::Begins),
            "ends" => Some(Modifier::Ends),
            "regex" => Some(Modifier::Regex),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Modifier::Is => "is",
            Modifier::Not => "not",
            Modifier::All => "all",
            Modifier::Gt => "gt",
            Modifier::Gte => "gte",
            Modifier::Lt => "lt",
            Modifier::Lte => "lte",
            Modifier::Before => "before",
            Modifier::After => "after",
            Modifier::Begins => "begins",
            Modifier::Ends => "ends",
            Modifier::Regex => "regex",
        }
    }

    /// The modifiers an attribute of this kind accepts, in the order they are
    /// listed back to the user when they get one wrong.
    fn allowed_for(kind: Kind) -> &'static [Modifier] {
        match kind {
            Kind::Codes => &[Modifier::Is, Modifier::Not],
            Kind::List => &[Modifier::Is, Modifier::Not, Modifier::All],
            Kind::Number => &[
                Modifier::Is,
                Modifier::Not,
                Modifier::Gt,
                Modifier::Gte,
                Modifier::Lt,
                Modifier::Lte,
            ],
            Kind::Date => &[Modifier::Before, Modifier::After],
            Kind::Text => &[
                Modifier::Is,
                Modifier::Not,
                Modifier::Begins,
                Modifier::Ends,
                Modifier::Regex,
            ],
        }
    }
}

/// The parsed right-hand side, already in the shape the comparison needs.
#[derive(Debug, Clone)]
enum Operand {
    Codes(Vec<String>),
    Number(i64),
    Date(NaiveDate),
    Text(String),
    Pattern(regex::Regex),
}

#[derive(Debug, Clone)]
pub struct Condition {
    pub attribute: Attribute,
    pub modifier: Modifier,
    operand: Operand,
}

impl Condition {
    /// Parse one `attribute[.modifier]: value` entry. The error is a plain
    /// sentence; the caller prefixes it with the filter name.
    pub fn parse(key: &str, value: &Value) -> Result<Condition, String> {
        let (name, suffix) = match key.split_once('.') {
            Some((name, suffix)) => (name, Some(suffix)),
            None => (key, None),
        };

        let attribute = Attribute::parse(name).ok_or_else(|| {
            format!(
                "unknown filter attribute `{name}` (known: {})",
                known_attribute_names().join(", ")
            )
        })?;

        let modifier = match suffix {
            None => default_modifier(attribute.kind()),
            Some(suffix) => Modifier::parse(suffix).ok_or_else(|| {
                format!("unknown modifier `.{suffix}` on `{}`", attribute.as_str())
            })?,
        };

        if !Modifier::allowed_for(attribute.kind()).contains(&modifier) {
            let allowed: Vec<String> = Modifier::allowed_for(attribute.kind())
                .iter()
                .map(|modifier| format!(".{}", modifier.as_str()))
                .collect();
            return Err(format!(
                "`.{}` does not apply to `{}` (allowed: {})",
                modifier.as_str(),
                attribute.as_str(),
                allowed.join(", ")
            ));
        }

        let operand = parse_operand(attribute, modifier, value)?;
        Ok(Condition {
            attribute,
            modifier,
            operand,
        })
    }

    pub fn matches(&self, item: &Item, unknown: UnknownPolicy) -> bool {
        // "Not known" is answered by the policy, never by negation: an item of
        // unknown country fails `country.not: us` just as it fails `country: us`,
        // because we cannot show either way.
        let Some(reading) = self.attribute.read(item) else {
            return unknown.allows();
        };

        match (&self.operand, reading) {
            (Operand::Codes(wanted), Reading::Codes(actual)) => {
                let any = wanted
                    .iter()
                    .any(|want| actual.iter().any(|have| have.eq_ignore_ascii_case(want)));
                match self.modifier {
                    Modifier::Not => !any,
                    Modifier::All => wanted
                        .iter()
                        .all(|want| actual.iter().any(|have| have.eq_ignore_ascii_case(want))),
                    _ => any,
                }
            }
            (Operand::Number(wanted), Reading::Number(actual)) => match self.modifier {
                Modifier::Not => actual != *wanted,
                Modifier::Gt => actual > *wanted,
                Modifier::Gte => actual >= *wanted,
                Modifier::Lt => actual < *wanted,
                Modifier::Lte => actual <= *wanted,
                _ => actual == *wanted,
            },
            (Operand::Date(wanted), Reading::Date(actual)) => match self.modifier {
                Modifier::Before => actual < *wanted,
                _ => actual > *wanted,
            },
            (Operand::Text(wanted), Reading::Text(actual)) => {
                let actual = actual.to_lowercase();
                match self.modifier {
                    Modifier::Not => !actual.contains(wanted.as_str()),
                    Modifier::Begins => actual.starts_with(wanted.as_str()),
                    Modifier::Ends => actual.ends_with(wanted.as_str()),
                    _ => actual.contains(wanted.as_str()),
                }
            }
            (Operand::Pattern(pattern), Reading::Text(actual)) => pattern.is_match(actual),
            // Attribute kind and operand kind are paired at parse time.
            _ => false,
        }
    }
}

fn default_modifier(kind: Kind) -> Modifier {
    match kind {
        // A bare `release:` would be ambiguous, so dates always say which side.
        Kind::Date => Modifier::After,
        _ => Modifier::Is,
    }
}

fn parse_operand(
    attribute: Attribute,
    modifier: Modifier,
    value: &Value,
) -> Result<Operand, String> {
    let name = attribute.as_str();
    match attribute.kind() {
        Kind::Codes | Kind::List => {
            let codes = flatten_codes(value)
                .ok_or_else(|| format!("`{name}` expects a string or a list of strings"))?;
            if codes.is_empty() {
                return Err(format!("`{name}` has no values"));
            }
            Ok(Operand::Codes(codes))
        }
        Kind::Number => {
            let number = as_number(value)
                .ok_or_else(|| format!("`{name}` expects a single whole number"))?;
            Ok(Operand::Number(number))
        }
        Kind::Date => {
            let text =
                as_text(value).ok_or_else(|| format!("`{name}` expects a date as `YYYY-MM-DD`"))?;
            let date = NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                .map_err(|_| format!("`{text}` is not a date in `YYYY-MM-DD` form"))?;
            Ok(Operand::Date(date))
        }
        Kind::Text => {
            let text = as_text(value).ok_or_else(|| format!("`{name}` expects a single string"))?;
            if modifier == Modifier::Regex {
                let pattern = RegexBuilder::new(&text)
                    .case_insensitive(true)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|error| format!("invalid regex `{text}`: {error}"))?;
                return Ok(Operand::Pattern(pattern));
            }
            Ok(Operand::Text(text.to_lowercase()))
        }
    }
}

/// `ru, su`, `[ru, su]` and `ru` all mean the same thing.
fn flatten_codes(value: &Value) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut push = |text: &str| {
        for part in text.split(',') {
            let part = part.trim();
            if !part.is_empty() && !out.iter().any(|seen: &String| seen == part) {
                out.push(part.to_string());
            }
        }
    };
    match value {
        Value::Sequence(entries) => {
            for entry in entries {
                push(&as_text(entry)?);
            }
        }
        _ => push(&as_text(value)?),
    }
    Some(out)
}

/// YAML scalars: a bare `PG` is a string, a bare `18` is a number, and `true`
/// would be a bool. All of them are legitimate values to compare against.
fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn as_number(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// A named block of conditions, all of which must hold.
#[derive(Debug, Clone)]
pub struct FilterDef {
    pub name: String,
    pub conditions: Vec<Condition>,
    pub unknown: UnknownPolicy,
}

impl FilterDef {
    pub fn matches(&self, item: &Item) -> bool {
        self.conditions
            .iter()
            .all(|condition| condition.matches(item, self.unknown))
    }

    /// Every attribute this filter reads, for scoping metadata enrichment.
    pub fn attributes(&self) -> impl Iterator<Item = Attribute> + '_ {
        self.conditions.iter().map(|condition| condition.attribute)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attrs, MediaType};

    fn yaml(raw: &str) -> Value {
        serde_yaml_ng::from_str(raw).expect("value parses")
    }

    fn condition(key: &str, raw: &str) -> Condition {
        Condition::parse(key, &yaml(raw)).expect("condition parses")
    }

    fn come_and_see() -> Item {
        let mut item = Item::new(MediaType::Movie);
        item.title = Some("Come and See".into());
        item.year = Some(1985);
        item.attrs = Attrs {
            country: Some("su".into()),
            original_language: Some("ru".into()),
            spoken_language: Some("ru".into()),
            genres: vec!["drama".into(), "war".into()],
            runtime: Some(142),
            content_rating: Some("M".into()),
            status: Some("released".into()),
        };
        item
    }

    fn bare() -> Item {
        let mut item = Item::new(MediaType::Movie);
        item.title = Some("Unknown Quantity".into());
        item
    }

    #[test]
    fn commas_and_lists_both_mean_any_of() {
        for spelling in ["ru, su", "[ru, su]", "[\"ru\", \"su\"]"] {
            let condition = condition("country", spelling);
            assert!(
                condition.matches(&come_and_see(), UnknownPolicy::Exclude),
                "{spelling}"
            );
        }
        assert!(!condition("country", "ru, fr").matches(&come_and_see(), UnknownPolicy::Exclude));
    }

    #[test]
    fn codes_compare_case_insensitively() {
        assert!(condition("country", "SU").matches(&come_and_see(), UnknownPolicy::Exclude));
        assert!(condition("content_rating", "m").matches(&come_and_see(), UnknownPolicy::Exclude));
    }

    #[test]
    fn not_inverts_a_known_value() {
        assert!(condition("country.not", "us").matches(&come_and_see(), UnknownPolicy::Exclude));
        assert!(!condition("country.not", "su").matches(&come_and_see(), UnknownPolicy::Exclude));
    }

    #[test]
    fn genres_support_any_of_and_all_of() {
        let item = come_and_see();
        assert!(condition("genre", "war, comedy").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("genre.all", "drama, war").matches(&item, UnknownPolicy::Exclude));
        assert!(!condition("genre.all", "drama, comedy").matches(&item, UnknownPolicy::Exclude));
    }

    #[test]
    fn numbers_compare_with_the_expected_modifiers() {
        let item = come_and_see();
        assert!(condition("runtime.lte", "150").matches(&item, UnknownPolicy::Exclude));
        assert!(!condition("runtime.lte", "100").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("runtime.gt", "141").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("year.gte", "1980").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("year", "1985").matches(&item, UnknownPolicy::Exclude));
    }

    #[test]
    fn dates_say_which_side_they_mean() {
        let mut item = come_and_see();
        item.released = chrono::NaiveDate::from_ymd_opt(1985, 10, 17);
        assert!(condition("release.after", "1980-01-01").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("release.before", "1990-01-01").matches(&item, UnknownPolicy::Exclude));
        // A bare `release:` is `.after`, because a date equality test is useless.
        assert!(condition("release", "1980-01-01").matches(&item, UnknownPolicy::Exclude));
    }

    #[test]
    fn text_matching_is_case_insensitive() {
        let item = come_and_see();
        assert!(condition("title", "and see").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("title.begins", "come").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("title.ends", "SEE").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("title.regex", "^come .* see$").matches(&item, UnknownPolicy::Exclude));
        assert!(!condition("title.regex", "^see").matches(&item, UnknownPolicy::Exclude));
    }

    #[test]
    fn media_type_is_always_known() {
        let item = bare();
        assert!(condition("type", "movie").matches(&item, UnknownPolicy::Exclude));
        assert!(!condition("type", "show").matches(&item, UnknownPolicy::Exclude));
    }

    #[test]
    fn an_unknown_attribute_answers_to_the_policy_not_to_negation() {
        let item = bare();
        // Neither "it is Russian" nor "it is not Russian" can be shown, so both
        // give the same answer, and the policy decides which.
        assert!(!condition("country", "ru").matches(&item, UnknownPolicy::Exclude));
        assert!(!condition("country.not", "ru").matches(&item, UnknownPolicy::Exclude));
        assert!(condition("country", "ru").matches(&item, UnknownPolicy::Include));
        assert!(condition("country.not", "ru").matches(&item, UnknownPolicy::Include));
    }

    #[test]
    fn every_condition_in_a_block_must_hold() {
        let filter = FilterDef {
            name: "russian_war_film".into(),
            conditions: vec![
                condition("country", "ru, su"),
                condition("genre", "war"),
                condition("runtime.gte", "120"),
            ],
            unknown: UnknownPolicy::Exclude,
        };
        assert!(filter.matches(&come_and_see()));

        let mut short = come_and_see();
        short.attrs.runtime = Some(90);
        assert!(!filter.matches(&short));
    }

    #[test]
    fn aliases_resolve_to_the_same_attribute() {
        for (alias, canonical) in [
            ("language", Attribute::OriginalLanguage),
            ("genres", Attribute::Genre),
            ("certification", Attribute::ContentRating),
            ("media_type", Attribute::Type),
            ("released", Attribute::Release),
        ] {
            let key = if canonical == Attribute::Release {
                format!("{alias}.after")
            } else {
                alias.to_string()
            };
            let value = if canonical == Attribute::Release {
                "1980-01-01"
            } else {
                "x"
            };
            assert_eq!(condition(&key, value).attribute, canonical);
        }
    }

    #[test]
    fn bad_spellings_are_rejected_with_the_alternatives() {
        let error = Condition::parse("countryy", &yaml("ru")).unwrap_err();
        assert!(error.contains("unknown filter attribute"), "{error}");
        assert!(error.contains("country"), "{error}");

        let error = Condition::parse("country.nto", &yaml("ru")).unwrap_err();
        assert!(error.contains("unknown modifier"), "{error}");

        // `.gte` is meaningless on a country.
        let error = Condition::parse("country.gte", &yaml("ru")).unwrap_err();
        assert!(error.contains("does not apply"), "{error}");
        assert!(error.contains(".not"), "{error}");

        let error = Condition::parse("release.after", &yaml("last tuesday")).unwrap_err();
        assert!(error.contains("YYYY-MM-DD"), "{error}");

        let error = Condition::parse("runtime.lte", &yaml("ninety")).unwrap_err();
        assert!(error.contains("whole number"), "{error}");

        let error = Condition::parse("title.regex", &yaml("\"[unclosed\"")).unwrap_err();
        assert!(error.contains("invalid regex"), "{error}");

        assert!(Condition::parse("country", &yaml("[]")).is_err());
    }
}
