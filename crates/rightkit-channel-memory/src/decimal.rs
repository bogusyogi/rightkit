//! Exact decimal for proof values. The text is kept as written and validated
//! against the same pattern as the schema, so no binary float ever enters a
//! comparison. Equality compares the exact value: `1240.00` equals `1240`.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{MemoryError, ReasonCode};

/// Longest accepted decimal text, matching the schema `maxLength`.
const MAX_LEN: usize = 40;

#[derive(Debug, Clone)]
pub struct Decimal {
    text: String,
}

impl Decimal {
    /// Accepts `-?(0|[1-9][0-9]*)(\.[0-9]+)?` and nothing else: no sign on
    /// zero padding, no separators, no exponent, no bare dot.
    pub fn parse(text: &str) -> Option<Decimal> {
        if text.len() > MAX_LEN {
            return None;
        }
        let body = text.strip_prefix('-').unwrap_or(text);
        let (whole, fraction) = match body.split_once('.') {
            Some((whole, fraction)) => (whole, Some(fraction)),
            None => (body, None),
        };
        let whole_ok = whole == "0"
            || (!whole.is_empty()
                && !whole.starts_with('0')
                && whole.bytes().all(|byte| byte.is_ascii_digit()));
        let fraction_ok = fraction.is_none_or(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        });
        if whole_ok && fraction_ok {
            Some(Decimal {
                text: text.to_string(),
            })
        } else {
            None
        }
    }

    /// Typed construction: the same acceptance as `parse`, refused with
    /// `E_NUMBER_INVALID` instead of `None`. Serde uses the same check.
    pub fn try_parse(text: &str) -> Result<Decimal, MemoryError> {
        Decimal::parse(text).ok_or_else(|| MemoryError::Refused {
            code: ReasonCode::NumberInvalid,
            detail: format!("`{text}` is not an exact decimal"),
        })
    }

    /// The text as written in the record.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Sign, whole digits and fraction digits without trailing zeros. Zero is
    /// never negative, so `-0` and `0.0` compare equal.
    fn canonical(&self) -> (bool, &str, &str) {
        let (negative, body) = match self.text.strip_prefix('-') {
            Some(body) => (true, body),
            None => (false, self.text.as_str()),
        };
        let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
        let fraction = fraction.trim_end_matches('0');
        let zero = whole == "0" && fraction.is_empty();
        (negative && !zero, whole, fraction)
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.canonical() == other.canonical()
    }
}

impl Eq for Decimal {}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Decimal::parse(&text)
            .ok_or_else(|| D::Error::custom(format!("`{text}` is not an exact decimal")))
    }
}

#[cfg(test)]
mod tests {
    use super::Decimal;

    fn dec(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or_else(|| panic!("`{text}` should parse"))
    }

    #[test]
    fn trailing_zeros_and_plain_form_are_the_same_value() {
        assert_eq!(dec("1240.00"), dec("1240"));
        assert_eq!(dec("-0"), dec("0.0"));
        assert_ne!(dec("1240.01"), dec("1240"));
        assert_ne!(dec("-1240"), dec("1240"));
    }

    #[test]
    fn malformed_text_is_refused() {
        for text in [
            "", "-", ".5", "1.", "1,240", "1e3", "01", "NaN", " 1", "1_0", "+1",
        ] {
            assert!(Decimal::parse(text).is_none(), "`{text}` must not parse");
        }
    }
}
