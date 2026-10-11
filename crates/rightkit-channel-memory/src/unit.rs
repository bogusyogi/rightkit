//! Units of a proof value and of a permitted claim. The built-in codes are an
//! ISO 4217 currency code and `percent`, `count`, `days`, `seconds`. Any other
//! unit is an open slug.
//!
//! Two tokens are reserved: `_per_` and a trailing `_per`. A slug with neither
//! is a plain base. Any other slug must end in `_per_<period>`, where the
//! period is `day`, `week`, `month` or `year`, and the part before that suffix
//! must itself be a plain base.
//!
//! Length: a plain base is 1 to 32 characters. A suffixed slug has a base of 1
//! to 32 characters before the suffix, so the whole slug can be up to 42
//! characters (the longest suffix is `_per_month`, 10 characters).
//!
//! The schema `$defs/units` accepts exactly the same language. Its reservation
//! is `anyOf(allOf(not _per_, not _per$), allOf(_per_<period>$, not _per_.*_per_,
//! not _per_per_))` over the slug pattern. The regex engine behind the schema
//! validator has no lookahead, so the owner's clauses carry the reservation.
//! `_per_per_` is a separate clause because `_per_.*_per_` misses the overlapping
//! case `a_per_per_day`.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{MemoryError, ReasonCode};

const BUILT_IN: [&str; 4] = ["percent", "count", "days", "seconds"];
const PERIODS: [&str; 4] = ["day", "week", "month", "year"];
const MAX_BASE_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    text: String,
}

impl Unit {
    /// Accepts a currency code, a built-in code or an open slug, and nothing else.
    pub fn parse(text: &str) -> Option<Unit> {
        let accepted = is_currency(text) || BUILT_IN.contains(&text) || is_open_slug(text);
        accepted.then(|| Unit {
            text: text.to_string(),
        })
    }

    /// Typed construction: the same acceptance as `parse`, refused with
    /// `E_UNIT_INVALID` instead of `None`. Serde uses the same check.
    pub fn try_parse(text: &str) -> Result<Unit, MemoryError> {
        Unit::parse(text).ok_or_else(|| MemoryError::Refused {
            code: ReasonCode::UnitInvalid,
            detail: format!("`{text}` is not a unit"),
        })
    }

    /// The unit as written in the record.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

fn is_currency(text: &str) -> bool {
    text.len() == 3 && text.bytes().all(|byte| byte.is_ascii_uppercase())
}

/// A plain base has no `_per_` and does not end in `_per`. A plain slug is a
/// base of 1 to 32 characters. Any other slug must be a plain base followed by
/// `_per_<period>`.
fn is_open_slug(text: &str) -> bool {
    if is_plain(text) {
        return is_base_slug(text);
    }
    PERIODS.iter().any(|period| {
        text.strip_suffix(&format!("_per_{period}"))
            .is_some_and(|prefix| is_plain(prefix) && is_base_slug(prefix))
    })
}

/// Neither reserved token: no `_per_` substring and not ending in `_per`.
fn is_plain(text: &str) -> bool {
    !text.contains("_per_") && !text.ends_with("_per")
}

/// `[a-z][a-z0-9_]{0,31}`: one lowercase letter, then up to 31 more characters.
fn is_base_slug(base: &str) -> bool {
    let starts = base
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase());
    starts
        && base.len() <= MAX_BASE_LEN
        && base
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for Unit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Unit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Unit::parse(&text).ok_or_else(|| D::Error::custom(format!("`{text}` is not a unit")))
    }
}

#[cfg(test)]
mod tests {
    use super::Unit;

    #[test]
    fn built_in_and_open_units_are_accepted() {
        for text in [
            "USD",
            "EUR",
            "percent",
            "count",
            "days",
            "seconds",
            "views",
            "subscribers",
            "episodes",
            "shorts_per_week",
            "orders_per_month",
            "posts_per_day",
            "clicks_per_year",
            "views_per_day",
            // `_percentile` and `_percent` are not `_per_` or a trailing `_per`.
            "upper_percentile",
            "viewers_percent",
            // `per` at the start of a base is not the reserved `_per`.
            "per_capita_score",
            "super_fans",
        ] {
            assert!(Unit::parse(text).is_some(), "`{text}` must be accepted");
        }
    }

    #[test]
    fn malformed_units_are_refused() {
        let base_33 = format!("a{}", "b".repeat(32));
        assert_eq!(base_33.len(), 33);
        for text in [
            "Views",
            "shorts per week",
            "9lives",
            "",
            "US",
            "USDX",
            "_per_week",
            // `hour` is not a period, and `_per_` is reserved in the base.
            "x_per_hour",
            "a_per_week_per_day",
            "views_per_",
            "views_per",
            "x_per",
            "x_per_per_day",
            // The overlapping case that `_per_.*_per_` alone misses.
            "a_per_per_day",
            base_33.as_str(),
        ] {
            assert!(Unit::parse(text).is_none(), "`{text}` must be refused");
        }
    }

    #[test]
    fn a_base_of_32_characters_is_the_longest_open_unit() {
        let base_32 = format!("a{}", "b".repeat(31));
        assert!(Unit::parse(&base_32).is_some());
        assert!(Unit::parse(&format!("{base_32}_per_day")).is_some());
    }
}
