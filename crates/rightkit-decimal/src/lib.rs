//! Exact decimal values and comparison with no floats. std only.
//!
//! A [`Decimal`] is a sign, an integer digit string and a fraction digit string. Parsing accepts
//! plain decimal text (`-12.50`, `+0.1`, `.5`, `7.`): an optional sign, ASCII digits, and at most one
//! point. There is no exponent form, no thousands separator and no currency sign; a caller that
//! reads those first (see `rightkit-content-proof`) hands this crate the canonical string.
//! Input is bounded by [`MAX_DIGITS`] so a hostile string cannot cost unbounded time or memory.
//!
//! Comparison is exact: `1.0` equals `1` and `-0` equals `0`; no float rounding enters.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// Most digits (integer plus fraction) one parsed value may hold.
pub const MAX_DIGITS: usize = 1024;

/// Why a decimal string was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalError {
    /// No digits at all (empty text, a bare sign, or a bare point).
    Empty,
    /// A character other than a sign, an ASCII digit or one point.
    InvalidCharacter,
    /// More than one point, or a sign that is not first.
    Malformed,
    /// More than [`MAX_DIGITS`] digits.
    TooLong,
}

impl fmt::Display for DecimalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DecimalError::Empty => "decimal has no digits",
            DecimalError::InvalidCharacter => {
                "decimal has a character other than a sign, digit or point"
            }
            DecimalError::Malformed => "decimal has more than one point or a misplaced sign",
            DecimalError::TooLong => "decimal has more digits than the limit",
        })
    }
}

impl std::error::Error for DecimalError {}

/// A comparison operator, as metric thresholds name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl Comparison {
    /// The operator for `lt`, `le`, `eq`, `ge`, `gt` (also `<`, `<=`, `==`, `=`, `>=`, `>`).
    pub fn parse(text: &str) -> Option<Comparison> {
        match text.trim().to_ascii_lowercase().as_str() {
            "lt" | "<" => Some(Comparison::Lt),
            "le" | "<=" => Some(Comparison::Le),
            "eq" | "==" | "=" => Some(Comparison::Eq),
            "ge" | ">=" => Some(Comparison::Ge),
            "gt" | ">" => Some(Comparison::Gt),
            _ => None,
        }
    }

    /// Whether `ordering` (left compared with right) satisfies the operator.
    pub fn holds(self, ordering: Ordering) -> bool {
        match self {
            Comparison::Lt => ordering == Ordering::Less,
            Comparison::Le => ordering != Ordering::Greater,
            Comparison::Eq => ordering == Ordering::Equal,
            Comparison::Ge => ordering != Ordering::Less,
            Comparison::Gt => ordering == Ordering::Greater,
        }
    }
}

/// An exact decimal number. Always normalised: no leading zeros in the integer part, no trailing
/// zeros in the fraction, and zero is never negative.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    negative: bool,
    integer: String,
    fraction: String,
}

impl Decimal {
    /// Parse plain decimal text.
    pub fn parse(text: &str) -> Result<Decimal, DecimalError> {
        let bytes = text.as_bytes();
        let mut index = 0;
        let mut negative = false;
        if let Some(&first) = bytes.first() {
            if first == b'+' || first == b'-' {
                negative = first == b'-';
                index = 1;
            }
        }
        let mut integer = String::new();
        let mut fraction = String::new();
        let mut seen_point = false;
        for &byte in &bytes[index..] {
            match byte {
                b'0'..=b'9' => {
                    if seen_point {
                        fraction.push(byte as char);
                    } else {
                        integer.push(byte as char);
                    }
                }
                b'.' => {
                    if seen_point {
                        return Err(DecimalError::Malformed);
                    }
                    seen_point = true;
                }
                b'+' | b'-' => return Err(DecimalError::Malformed),
                _ => return Err(DecimalError::InvalidCharacter),
            }
            if integer.len() + fraction.len() > MAX_DIGITS {
                return Err(DecimalError::TooLong);
            }
        }
        if integer.is_empty() && fraction.is_empty() {
            return Err(DecimalError::Empty);
        }
        let integer = integer.trim_start_matches('0').to_string();
        let fraction = fraction.trim_end_matches('0').to_string();
        let is_zero = integer.is_empty() && fraction.is_empty();
        Ok(Decimal {
            negative: negative && !is_zero,
            integer,
            fraction,
        })
    }

    /// True for zero.
    pub fn is_zero(&self) -> bool {
        self.integer.is_empty() && self.fraction.is_empty()
    }

    /// True when below zero.
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    fn cmp_magnitude(&self, other: &Decimal) -> Ordering {
        self.integer
            .len()
            .cmp(&other.integer.len())
            .then_with(|| self.integer.cmp(&other.integer))
            .then_with(|| self.fraction.cmp(&other.fraction))
    }

    /// Whether `self <op> other` holds.
    pub fn satisfies(&self, op: Comparison, other: &Decimal) -> bool {
        op.holds(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Decimal) -> Ordering {
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Decimal) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Decimal {
    /// The canonical text: `-` for a negative, `0` for an empty integer part, a point only when a
    /// fraction remains.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negative {
            f.write_str("-")?;
        }
        if self.integer.is_empty() {
            f.write_str("0")?;
        } else {
            f.write_str(&self.integer)?;
        }
        if !self.fraction.is_empty() {
            f.write_str(".")?;
            f.write_str(&self.fraction)?;
        }
        Ok(())
    }
}

impl FromStr for Decimal {
    type Err = DecimalError;

    fn from_str(text: &str) -> Result<Decimal, DecimalError> {
        Decimal::parse(text)
    }
}

/// Why a string comparison could not be answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareError {
    /// The left operand did not parse.
    Left(DecimalError),
    /// The right operand did not parse.
    Right(DecimalError),
}

impl fmt::Display for CompareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompareError::Left(e) => write!(f, "left operand: {e}"),
            CompareError::Right(e) => write!(f, "right operand: {e}"),
        }
    }
}

impl std::error::Error for CompareError {}

/// Compare two decimal strings exactly: `left <op> right`. Used for metric thresholds, where a
/// stored value and a configured limit are both decimal text.
pub fn compare(left: &str, op: Comparison, right: &str) -> Result<bool, CompareError> {
    let left = Decimal::parse(left).map_err(CompareError::Left)?;
    let right = Decimal::parse(right).map_err(CompareError::Right)?;
    Ok(left.satisfies(op, &right))
}

/// The ordering of two decimal strings.
pub fn ordering(left: &str, right: &str) -> Result<Ordering, CompareError> {
    let left = Decimal::parse(left).map_err(CompareError::Left)?;
    let right = Decimal::parse(right).map_err(CompareError::Right)?;
    Ok(left.cmp(&right))
}
