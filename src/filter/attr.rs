//! The attribute vocabulary a filter condition can talk about.
//!
//! Names follow Kometa/TMDb spelling (`content_rating`, `original_language`)
//! rather than the internal field names, because that is what someone writing
//! these filters has seen before.

use chrono::NaiveDate;

use crate::model::Item;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attribute {
    Country,
    OriginalLanguage,
    SpokenLanguage,
    Genre,
    ContentRating,
    Status,
    Runtime,
    Year,
    Release,
    Title,
    Type,
}

/// What shape of value an attribute holds, which decides the modifiers it
/// accepts and how its values are parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One or more short codes compared case-insensitively: country, language,
    /// certification, media type.
    Codes,
    /// Like [`Kind::Codes`] but genuinely multi-valued, so `.all` means something.
    List,
    Number,
    Date,
    Text,
}

/// The value read off an item, or `None` when the item does not know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading<'a> {
    Codes(Vec<&'a str>),
    Number(i64),
    Date(NaiveDate),
    Text(&'a str),
}

impl Attribute {
    /// Every spelling accepted in a filter block. The first is canonical.
    pub const ALL: &'static [(&'static str, Attribute)] = &[
        ("country", Attribute::Country),
        ("original_language", Attribute::OriginalLanguage),
        ("language", Attribute::OriginalLanguage),
        ("spoken_language", Attribute::SpokenLanguage),
        ("genre", Attribute::Genre),
        ("genres", Attribute::Genre),
        ("content_rating", Attribute::ContentRating),
        ("certification", Attribute::ContentRating),
        ("status", Attribute::Status),
        ("runtime", Attribute::Runtime),
        ("year", Attribute::Year),
        ("release", Attribute::Release),
        ("released", Attribute::Release),
        ("title", Attribute::Title),
        ("type", Attribute::Type),
        ("media_type", Attribute::Type),
    ];

    pub fn parse(raw: &str) -> Option<Attribute> {
        let key = raw.trim().to_ascii_lowercase();
        Attribute::ALL
            .iter()
            .find(|(spelling, _)| *spelling == key)
            .map(|(_, attribute)| *attribute)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Attribute::Country => "country",
            Attribute::OriginalLanguage => "original_language",
            Attribute::SpokenLanguage => "spoken_language",
            Attribute::Genre => "genre",
            Attribute::ContentRating => "content_rating",
            Attribute::Status => "status",
            Attribute::Runtime => "runtime",
            Attribute::Year => "year",
            Attribute::Release => "release",
            Attribute::Title => "title",
            Attribute::Type => "type",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Attribute::Country
            | Attribute::OriginalLanguage
            | Attribute::SpokenLanguage
            | Attribute::ContentRating
            | Attribute::Status
            | Attribute::Type => Kind::Codes,
            Attribute::Genre => Kind::List,
            Attribute::Runtime | Attribute::Year => Kind::Number,
            Attribute::Release => Kind::Date,
            Attribute::Title => Kind::Text,
        }
    }

    /// Whether this attribute has to be fetched from a metadata provider, or is
    /// already carried by every item. Drives what the enricher asks for.
    pub fn needs_metadata(self) -> bool {
        match self {
            Attribute::Country
            | Attribute::OriginalLanguage
            | Attribute::SpokenLanguage
            | Attribute::Genre
            | Attribute::ContentRating
            | Attribute::Status
            | Attribute::Runtime => true,
            Attribute::Year | Attribute::Release | Attribute::Title | Attribute::Type => false,
        }
    }

    /// Whether a set of attributes already answers this one. Attributes that
    /// live on the item itself rather than in metadata are always answered.
    pub fn present_in(self, attrs: &crate::model::Attrs) -> bool {
        match self {
            Attribute::Country => attrs.country.is_some(),
            Attribute::OriginalLanguage => attrs.original_language.is_some(),
            Attribute::SpokenLanguage => attrs.spoken_language.is_some(),
            Attribute::Genre => !attrs.genres.is_empty(),
            Attribute::ContentRating => attrs.content_rating.is_some(),
            Attribute::Status => attrs.status.is_some(),
            Attribute::Runtime => attrs.runtime.is_some(),
            Attribute::Year | Attribute::Release | Attribute::Title | Attribute::Type => true,
        }
    }

    /// Read the attribute off an item. `None` means "not known", which is what
    /// the filter's `unknown:` policy decides the answer for.
    pub fn read<'a>(self, item: &'a Item) -> Option<Reading<'a>> {
        let single =
            |value: &'a Option<String>| value.as_deref().map(|text| Reading::Codes(vec![text]));
        match self {
            Attribute::Country => single(&item.attrs.country),
            Attribute::OriginalLanguage => single(&item.attrs.original_language),
            Attribute::SpokenLanguage => single(&item.attrs.spoken_language),
            Attribute::ContentRating => single(&item.attrs.content_rating),
            Attribute::Status => single(&item.attrs.status),
            Attribute::Genre => (!item.attrs.genres.is_empty())
                .then(|| Reading::Codes(item.attrs.genres.iter().map(String::as_str).collect())),
            Attribute::Runtime => item
                .attrs
                .runtime
                .map(|minutes| Reading::Number(minutes.into())),
            Attribute::Year => item
                .effective_year()
                .map(|year| Reading::Number(year.into())),
            Attribute::Release => item.released.map(Reading::Date),
            Attribute::Title => item.title.as_deref().map(Reading::Text),
            Attribute::Type => Some(Reading::Codes(vec![item.media_type.as_str()])),
        }
    }
}

/// The spellings accepted for an attribute, for "did you mean" style errors.
pub fn known_attribute_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Attribute::ALL.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names
}
